const INSERT_INPUT_TYPES = new Set([
  "insertText",
  "insertReplacementText",
  "insertFromComposition"
]);
const DELETE_INPUT_TYPES = new Map([
  ["deleteContentBackward", "Backspace"],
  ["deleteContentForward", "Delete"]
]);
const TAP_THRESHOLD_CSS_PX = 8;

function finite(value, fallback = 0) {
  return Number.isFinite(value) ? value : fallback;
}

function bounded(value, minimum, maximum) {
  return Math.min(maximum, Math.max(minimum, finite(value, minimum)));
}

function plugin(capacitor, name) {
  let instance = capacitor?.Plugins?.[name];
  if (
    !instance &&
    capacitor?.isNativePlatform?.() &&
    capacitor?.isPluginAvailable?.(name) &&
    typeof capacitor.registerPlugin === "function"
  ) {
    instance = capacitor.registerPlugin(name);
  }
  return instance ?? null;
}

function cloneDiagnostics(diagnostics) {
  return JSON.parse(JSON.stringify(diagnostics));
}

function canvasCoordinateScale(canvas) {
  const rect = canvas.getBoundingClientRect();
  const dpr = Math.max(1, finite(window.devicePixelRatio, 1));
  const scaleX = rect.width > 0 && canvas.width > 0 ? canvas.width / rect.width / dpr : 1;
  const scaleY = rect.height > 0 && canvas.height > 0 ? canvas.height / rect.height / dpr : 1;
  return {
    x: bounded(scaleX, 0.25, 4),
    y: bounded(scaleY, 0.25, 4)
  };
}

function logicalCanvasPosition(canvas, event, scale) {
  const rect = canvas.getBoundingClientRect();
  const offsetX = Number.isFinite(event.offsetX) ? event.offsetX : event.clientX - rect.left;
  const offsetY = Number.isFinite(event.offsetY) ? event.offsetY : event.clientY - rect.top;
  return {
    x: Math.max(0, offsetX * scale.x),
    y: Math.max(0, offsetY * scale.y)
  };
}

export function createPlatformCompatibility({ onViewportChange, requestPlatformBack }) {
  const diagnostics = {
    schemaVersion: "vibex-platform-compat.v1",
    owner: "apps/web",
    capabilities: {
      inputEventFallback: typeof InputEvent === "function" && typeof CompositionEvent === "function",
      touchScrollFallback: typeof PointerEvent === "function" && typeof WheelEvent === "function",
      capacitorKeyboard: false,
      capacitorApp: false
    },
    input: {
      enabled: false,
      fallbackCommits: 0,
      fallbackDeletes: 0,
      nativeConsumed: 0,
      compositionOwned: 0,
      duplicateSuppressed: 0,
      lastInputType: null,
      lastData: null
    },
    touch: {
      enabled: false,
      state: "idle",
      tapsReplayed: 0,
      scrollGestures: 0,
      wheelEvents: 0,
      cancelled: 0,
      coordinateScale: [1, 1],
      lastTapPosition: null,
      lastWheelPosition: null
    },
    keyboard: {
      visible: false,
      inset: 0,
      source: "none",
      nativeEvents: 0
    },
    back: {
      events: 0,
      lastResult: "unhandled",
      lastFallback: "none"
    }
  };

  let installedCanvas = null;
  let installedInput = null;
  let nativeKeyboardVisible = false;
  let nativeKeyboardInset = 0;
  let keyboardBaselineHeight = Math.max(
    finite(window.innerHeight),
    finite(window.visualViewport?.height)
  );

  function snapshot() {
    return cloneDiagnostics(diagnostics);
  }

  function keyboardSnapshot() {
    const visual = window.visualViewport;
    const layoutHeight = finite(window.innerHeight);
    const visualHeight = finite(visual?.height, layoutHeight);
    const visualInset = Math.max(0, layoutHeight - visualHeight - finite(visual?.offsetTop));

    if (!nativeKeyboardVisible) {
      keyboardBaselineHeight = Math.max(layoutHeight, visualHeight, keyboardBaselineHeight || 0);
    }

    const source = nativeKeyboardVisible
      ? "capacitor"
      : visualInset > 0.5
        ? "visual_viewport"
        : "none";
    const inset = source === "capacitor" ? nativeKeyboardInset : visualInset;
    const visible = inset > 0.5;
    diagnostics.keyboard.visible = visible;
    diagnostics.keyboard.inset = inset;
    diagnostics.keyboard.source = source;
    return { visible, inset, source };
  }

  function normalizeNativeKeyboardInset(rawInset) {
    const dpr = Math.max(1, finite(window.devicePixelRatio, 1));
    const baseline = Math.max(
      keyboardBaselineHeight,
      finite(window.innerHeight),
      finite(window.visualViewport?.height),
      1
    );
    let inset = Math.max(0, finite(rawInset));
    if (inset > baseline * 1.25 && dpr > 1) inset /= dpr;
    return bounded(inset, 0, baseline * 0.9);
  }

  function updateNativeKeyboard(visible, rawInset = 0) {
    nativeKeyboardVisible = visible;
    nativeKeyboardInset = visible ? normalizeNativeKeyboardInset(rawInset) : 0;
    diagnostics.keyboard.nativeEvents += 1;
    keyboardSnapshot();
    onViewportChange();
  }

  function installInputFallback(input) {
    if (!diagnostics.capabilities.inputEventFallback) return;
    diagnostics.input.enabled = true;
    let composing = false;
    let reentry = false;
    let recentDeleteKey = null;
    let recentComposition = null;

    input.addEventListener(
      "compositionstart",
      () => {
        if (!reentry) composing = true;
      },
      true
    );
    input.addEventListener(
      "compositionend",
      (event) => {
        if (reentry) return;
        composing = false;
        recentComposition = {
          data: typeof event.data === "string" ? event.data : "",
          at: performance.now()
        };
      },
      true
    );
    input.addEventListener(
      "keydown",
      (event) => {
        if (!reentry && (event.key === "Backspace" || event.key === "Delete")) {
          recentDeleteKey = { key: event.key, at: performance.now() };
        }
      },
      true
    );
    input.addEventListener(
      "beforeinput",
      (event) => {
        diagnostics.input.lastInputType = event.inputType || null;
        diagnostics.input.lastData = typeof event.data === "string" ? event.data.slice(0, 32) : null;
      },
      true
    );
    input.addEventListener(
      "input",
      (event) => {
        if (reentry) return;
        const inputType = event.inputType || "";
        const eventData = typeof event.data === "string" ? event.data : "";
        diagnostics.input.lastInputType = inputType || null;
        diagnostics.input.lastData = eventData ? eventData.slice(0, 32) : null;

        if (composing || event.isComposing) {
          diagnostics.input.compositionOwned += 1;
          return;
        }

        queueMicrotask(() => {
          if (reentry || composing || !input.isConnected) return;
          const residual = input.value;

          if (INSERT_INPUT_TYPES.has(inputType)) {
            if (!residual) {
              diagnostics.input.nativeConsumed += 1;
              return;
            }
            const text = eventData || residual;
            if (
              recentComposition &&
              recentComposition.data === text &&
              performance.now() - recentComposition.at < 100
            ) {
              input.value = "";
              diagnostics.input.duplicateSuppressed += 1;
              return;
            }

            input.value = "";
            reentry = true;
            try {
              input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true, data: "" }));
              input.dispatchEvent(new CompositionEvent("compositionupdate", { bubbles: true, data: text }));
              input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: text }));
              diagnostics.input.fallbackCommits += 1;
            } finally {
              reentry = false;
              input.value = "";
            }
            return;
          }

          const deleteKey = DELETE_INPUT_TYPES.get(inputType);
          if (!deleteKey) return;
          input.value = "";
          if (
            recentDeleteKey?.key === deleteKey &&
            performance.now() - recentDeleteKey.at < 100
          ) {
            diagnostics.input.duplicateSuppressed += 1;
            return;
          }

          reentry = true;
          try {
            input.dispatchEvent(
              new KeyboardEvent("keydown", {
                bubbles: true,
                cancelable: true,
                key: deleteKey,
                code: deleteKey
              })
            );
            input.dispatchEvent(
              new KeyboardEvent("keyup", {
                bubbles: true,
                cancelable: true,
                key: deleteKey,
                code: deleteKey
              })
            );
            diagnostics.input.fallbackDeletes += 1;
          } finally {
            reentry = false;
          }
        });
      },
      true
    );
  }

  function installTouchFallback(canvas) {
    if (!diagnostics.capabilities.touchScrollFallback) return;
    diagnostics.touch.enabled = true;
    let gesture = null;
    let replaying = false;

    function block(event) {
      event.preventDefault();
      event.stopImmediatePropagation();
    }

    function release(pointerId) {
      if (canvas.hasPointerCapture?.(pointerId)) {
        try {
          canvas.releasePointerCapture(pointerId);
        } catch {
          // The browser may release capture before pointercancel reaches the canvas.
        }
      }
    }

    function finish(state, event) {
      if (!gesture) return;
      const pointerId = gesture.pointerId;
      gesture = null;
      diagnostics.touch.state = state;
      release(pointerId);
      if (state === "cancelled") diagnostics.touch.cancelled += 1;
      if (event) block(event);
    }

    function replayTap(event) {
      const scale = canvasCoordinateScale(canvas);
      const position = logicalCanvasPosition(canvas, event, scale);
      const common = {
        bubbles: true,
        cancelable: true,
        composed: true,
        pointerId: event.pointerId,
        pointerType: "mouse",
        isPrimary: true,
        clientX: event.clientX,
        clientY: event.clientY,
        screenX: event.screenX,
        screenY: event.screenY,
        button: 0
      };
      diagnostics.touch.coordinateScale = [scale.x, scale.y];
      diagnostics.touch.lastTapPosition = [position.x, position.y];
      replaying = true;
      try {
        for (const [type, buttons, pressure] of [
          ["pointerdown", 1, 0.5],
          ["pointerup", 0, 0]
        ]) {
          const replay = new PointerEvent(type, { ...common, buttons, pressure });
          Object.defineProperties(replay, {
            offsetX: { configurable: true, value: position.x },
            offsetY: { configurable: true, value: position.y }
          });
          canvas.dispatchEvent(replay);
        }
        diagnostics.touch.tapsReplayed += 1;
      } finally {
        replaying = false;
      }
    }

    canvas.addEventListener(
      "pointerdown",
      (event) => {
        if (replaying || event.pointerType !== "touch" || !event.isPrimary) return;
        if (gesture) {
          block(event);
          return;
        }
        gesture = {
          pointerId: event.pointerId,
          startX: event.clientX,
          startY: event.clientY,
          lastX: event.clientX,
          lastY: event.clientY,
          state: "pending_tap"
        };
        diagnostics.touch.state = "pending_tap";
        try {
          canvas.setPointerCapture?.(event.pointerId);
        } catch {
          // Capture is an optimization; touch-action:none still keeps the stream on the canvas.
        }
        block(event);
      },
      { capture: true, passive: false }
    );

    canvas.addEventListener(
      "pointermove",
      (event) => {
        if (replaying || event.pointerType !== "touch" || event.pointerId !== gesture?.pointerId) return;
        block(event);
        const deltaX = event.clientX - gesture.lastX;
        const deltaY = event.clientY - gesture.lastY;
        const distance = Math.hypot(event.clientX - gesture.startX, event.clientY - gesture.startY);
        gesture.lastX = event.clientX;
        gesture.lastY = event.clientY;
        if (gesture.state === "pending_tap" && distance >= TAP_THRESHOLD_CSS_PX) {
          gesture.state = "scrolling";
          diagnostics.touch.state = "scrolling";
          diagnostics.touch.scrollGestures += 1;
        }
        if (gesture.state !== "scrolling" || (deltaX === 0 && deltaY === 0)) return;

        const scale = canvasCoordinateScale(canvas);
        const position = logicalCanvasPosition(canvas, event, scale);
        diagnostics.touch.coordinateScale = [scale.x, scale.y];
        diagnostics.touch.lastWheelPosition = [position.x, position.y];

        const wheel = new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          composed: true,
          clientX: event.clientX,
          clientY: event.clientY,
          deltaX: -deltaX * scale.x,
          deltaY: -deltaY * scale.y,
          deltaMode: WheelEvent.DOM_DELTA_PIXEL
        });
        // gpui_web expects logical GPUI pixels. Most browsers expose a physical canvas
        // backing store at CSS pixels * DPR, but some WebViews report CSS pixels from
        // devicePixelContentBoxSize. Derive the conversion from the actual canvas so
        // the fallback stays independent of browser or Android version.
        Object.defineProperties(wheel, {
          offsetX: { configurable: true, value: position.x },
          offsetY: { configurable: true, value: position.y }
        });
        canvas.dispatchEvent(wheel);
        diagnostics.touch.wheelEvents += 1;
      },
      { capture: true, passive: false }
    );

    canvas.addEventListener(
      "pointerup",
      (event) => {
        if (replaying || event.pointerType !== "touch" || event.pointerId !== gesture?.pointerId) return;
        const shouldTap = gesture.state === "pending_tap";
        finish("ended", event);
        if (shouldTap) replayTap(event);
      },
      { capture: true, passive: false }
    );

    for (const eventName of ["pointercancel", "lostpointercapture", "pointerleave"]) {
      canvas.addEventListener(
        eventName,
        (event) => {
          if (replaying || event.pointerType !== "touch" || event.pointerId !== gesture?.pointerId) return;
          finish("cancelled", event);
        },
        { capture: true, passive: false }
      );
    }
  }

  function installGpuiElements(canvas, input) {
    if (canvas !== installedCanvas) {
      installedCanvas = canvas;
      installTouchFallback(canvas);
    }
    if (input !== installedInput) {
      installedInput = input;
      installInputFallback(input);
    }
  }

  async function platformBack(source = "test_bridge", event = null, appPlugin = null) {
    diagnostics.back.events += 1;
    const result = await requestPlatformBack(source);
    diagnostics.back.lastResult = result;
    diagnostics.back.lastFallback = "none";
    if (result !== "unhandled") return result;

    if (event?.canGoBack && history.length > 1) {
      history.back();
      diagnostics.back.lastFallback = "history_back";
    } else if (appPlugin?.exitApp) {
      await appPlugin.exitApp();
      diagnostics.back.lastFallback = "exit_app";
    }
    return result;
  }

  async function installNativeBridges() {
    const capacitor = globalThis.Capacitor;
    if (!capacitor?.isNativePlatform?.()) return;

    const keyboard = plugin(capacitor, "Keyboard");
    if (keyboard?.addListener) {
      diagnostics.capabilities.capacitorKeyboard = true;
      await Promise.all([
        keyboard.addListener("keyboardWillShow", (event) =>
          updateNativeKeyboard(true, event?.keyboardHeight)
        ),
        keyboard.addListener("keyboardDidShow", (event) =>
          updateNativeKeyboard(true, event?.keyboardHeight)
        ),
        keyboard.addListener("keyboardWillHide", () => updateNativeKeyboard(false)),
        keyboard.addListener("keyboardDidHide", () => updateNativeKeyboard(false))
      ]);
    }

    const app = plugin(capacitor, "App");
    if (app?.addListener) {
      diagnostics.capabilities.capacitorApp = true;
      await app.addListener("backButton", (event) => {
        void platformBack("capacitor_app", event, app);
      });
    }
  }

  return {
    installGpuiElements,
    installNativeBridges,
    keyboardSnapshot,
    platformBack,
    snapshot
  };
}
