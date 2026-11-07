//! The window's hint layer.
//!
//! GPUI paints a `deferred` draw after every inline element of a frame, so the
//! priority a layer is deferred at — not its place in the element tree — decides
//! whether a hint stays readable over an open dialog. The kit renders dialogs
//! through `gpui_base::Dialog` as deferred draws at priority `10 + layer`, popups
//! (menus, selects, popovers) at `gpui_base::POPUP_PRIORITY` (100), and tooltips
//! at 200.
//!
//! Up to GPUI Kit 0.6.4 the workbench rendered the kit's notification list itself
//! and deferred it at [`HINT_LAYER_PRIORITY`], in the gap between the dialog band
//! and the popup band, so a result pushed while a dialog was still open — a
//! settings operation, a rejected shortcut chord — stayed legible above the
//! dialog's backdrop instead of being dimmed by it. 0.7.0 moved the notification
//! layer inside `gpui_base::Root` and no longer exposes the list, which would put
//! every hint below that backdrop.
//!
//! The workbench therefore owns the list, and with it the card. A kit
//! `Notification` is sealed: it always paints a close button in its top right
//! corner, revealed on hover, and dismisses on a click only when the caller asks
//! for one. A hint is a message, not a form — the whole card is the dismiss
//! target and there is no second close control — so [`HintList`] mounts the same
//! `gpui_base` stack and lifecycle the kit's list runs on, with [`HintCard`] as
//! the presentation above it, and [`hint_notification`] as the one constructor
//! behind [`push`]. `Root` builds one plugin instance per window, so a hint still
//! lands on the window it was pushed from.

use std::{
    any::TypeId,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use gpui::{
    Anchor, AnimationExt as _, App, AppContext as _, BoxShadow, ClickEvent, ClipboardItem, Context,
    ElementId, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, deferred, div, hsla, px, relative,
};
use gpui_base::{
    Root, RootPlugin, StyledExt as _, Toast, ToastManager, ToastMotion, ToastOptions, ToastStack,
    ToastStackState, ToastTransitionStatus,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    notification::NotificationType,
};

use crate::{
    locale,
    motion::{self, MotionSpec},
};

/// Deferred paint priority for the hint layer.
///
/// The layer takes the gap between the dialog band and the popup band: above
/// every dialog backdrop, below the menus and tooltips the user is actively
/// pointing at.
const HINT_LAYER_PRIORITY: usize = 99;

/// How often the list samples the hint lifecycle while a hint can still change.
///
/// The card's own transitions are animated by gpui; this clock only advances the
/// enter and exit phases and the autohide countdown between frames.
const HINT_ADVANCE_INTERVAL: Duration = Duration::from_millis(50);

/// How long a hint stays on screen before it hides itself.
const HINT_AUTOHIDE: Duration = Duration::from_secs(5);

/// How far a card travels while it enters or leaves.
///
/// The distance the kit's notification list used, so a hint still appears to
/// come from the window edge it is anchored to.
const HINT_SLIDE_DISTANCE: Pixels = px(96.);

/// Card entrance and exit, the kit notification list's own timings on the
/// workbench's easing curve.
///
/// These have to stay in step with [`hint_motion`]: the manager unmounts an
/// ending hint once *its* exit duration has elapsed, so a card whose fade
/// outlasts that clock would be cut off mid-transition. Both honor the
/// measurement knob through [`MotionSpec::animation`].
const HINT_ENTER: MotionSpec = MotionSpec::new(400, motion::EASE);
const HINT_EXIT: MotionSpec = MotionSpec::new(200, motion::EASE);

/// Ink of the card's elevation at full strength.
const HINT_SHADOW_INK: f32 = 0.1;

/// Element id of the copy button every hint carries.
const HINT_COPY_BUTTON_ID: &str = "hint-copy-button";
/// Test hook for the card's own bounds.
const HINT_CARD_DEBUG_KEY: &str = "hint-card";
/// Test hook for the laid-out message.
const HINT_TEXT_DEBUG_KEY: &str = "hint-message-text";
/// Test hook for the laid-out copy button.
const HINT_COPY_DEBUG_KEY: &str = "hint-message-copy";

/// Serial that keeps a hint pushed without an explicit id its own entry.
static NEXT_HINT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// What a hint does when its card is pressed, beyond dismissing it.
type HintClickHandler = dyn Fn(&ClickEvent, &mut Window, &mut App);

/// What a press on a card does: dismiss that hint.
type HintDismissHandler = dyn Fn(&mut Window, &mut App);

/// Registers the hint layer for every window root created afterwards.
///
/// Must run during application initialization, before the first window opens.
pub fn init(cx: &mut App) {
    Root::register_plugin::<HintLayer>(cx, HintLayer::new);
}

/// A light hint or error result on the notification layer.
///
/// Built by [`hint_notification`]: the tone and the message are the whole
/// payload, because the card that paints them — the copy button included — is
/// owned by the layer, not by the caller. The one thing a caller may add to what
/// that button copies is [`Hint::copy_text`].
#[derive(Clone)]
pub struct Hint {
    id: HintId,
    tone: NotificationType,
    message: SharedString,
    /// What the copy button copies when that is more than the card says.
    copy_text: Option<SharedString>,
    autohide: bool,
    on_click: Option<Rc<HintClickHandler>>,
}

impl Hint {
    /// A hint with an id of its own, so pushing the same message again stacks a
    /// second card instead of replacing the first.
    fn new(tone: NotificationType, message: impl Into<SharedString>) -> Self {
        Self {
            id: HintId::Loose(NEXT_HINT_SERIAL.fetch_add(1, Ordering::Relaxed)),
            tone,
            message: message.into(),
            copy_text: None,
            autohide: true,
            on_click: None,
        }
    }

    /// Replace the previous hint of `T`'s family instead of stacking on it.
    ///
    /// Each hint family gets its own private zero-sized type, so unrelated hints
    /// never replace each other.
    pub fn id<T: Sized + 'static>(mut self) -> Self {
        self.id = HintId::Family(TypeId::of::<T>());
        self
    }

    /// Replace the previous hint of `T`'s family for the same `key` only.
    ///
    /// For a family with one hint per subject: one session's failure is one card
    /// however many paths report it, and another session's failure is its own.
    pub fn id_for<T: Sized + 'static>(mut self, key: impl Into<SharedString>) -> Self {
        self.id = HintId::Keyed(TypeId::of::<T>(), key.into());
        self
    }

    /// What the copy button copies instead of the message.
    ///
    /// For a card that says what happened in one line while the diagnostics a
    /// reader would paste somewhere else are longer. A newer hint that replaces
    /// a mounted one keeps what the older one could copy; see [`HintList::push`].
    pub fn copy_text(mut self, copy_text: impl Into<SharedString>) -> Self {
        self.copy_text = Some(copy_text.into());
        self
    }

    /// The line the card paints.
    #[cfg(test)]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether the hint hides itself after [`HINT_AUTOHIDE`]. Default: true.
    ///
    /// A hint that has to stay until the user has read it — a failed session
    /// fork, whose message is the only account of what went wrong — turns it off.
    pub fn autohide(mut self, autohide: bool) -> Self {
        self.autohide = autohide;
        self
    }

    /// What the press does beyond dismissing the hint.
    ///
    /// The card dismisses first and runs this afterwards, so a hint that offers
    /// an answer to its own message — "a dev server was detected, click to open
    /// it" — does not leave the notice it answered on screen.
    pub fn on_click(
        mut self,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(on_click));
        self
    }
}

/// What makes one hint on the layer distinct from another.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum HintId {
    /// One family of hints: a newer push replaces the older one.
    Family(TypeId),
    /// One subject inside a family: a newer push for it replaces the older one.
    Keyed(TypeId, SharedString),
    /// A hint pushed without an id, alone in its own entry.
    Loose(u64),
}

/// A light hint or error result that copies its whole message in one click, and
/// dismisses on a click anywhere else on the card.
///
/// The kit paints a notification's `message` as plain text that no selection
/// layer can reach, and a drag across a line that is already fading is a poor
/// way to ask for the error text a user most often needs to paste somewhere
/// else. The message therefore travels as the card's content, with one icon
/// button beside it that puts the message on the clipboard whole — or the longer
/// account the hint carries in [`Hint::copy_text`], when its message is a summary.
///
/// The message is never also painted by a second path: every hint the workbench
/// pushes is delivered in-app, so there is no system notification body that
/// needs the plain string either. This is the only constructor for a [`Hint`],
/// so a hint that skips the copy button cannot be built by accident.
pub fn hint_notification(tone: NotificationType, message: impl Into<SharedString>) -> Hint {
    Hint::new(tone, message)
}

/// Pushes one hint onto the layer of the window it is delivered to.
///
/// The `id` a hint carries decides replace-by-id, so a newer hint of the same
/// kind replaces the older one instead of stacking on it.
pub fn push(window: &mut Window, hint: Hint, cx: &mut App) {
    let layer = Root::read(window, cx).plugin::<HintLayer>().expect(
        "the hint layer is not registered on this window; call hint_layer::init before opening it",
    );
    layer.update(cx, |layer, cx| {
        layer
            .list
            .update(cx, |list, cx| list.push(hint, window, cx));
    });
}

/// The hint list mounted on `window`'s layer, if that window has one.
///
/// Deliver hints through [`push`]; this is for reading the mounted set — how
/// many hints are on screen.
pub fn list(window: &Window, cx: &App) -> Option<Entity<HintList>> {
    Root::read(window, cx)
        .plugin::<HintLayer>()
        .map(|layer| layer.read(cx).list.clone())
}

/// The per-window host of the workbench's hints.
struct HintLayer {
    list: Entity<HintList>,
}

impl HintLayer {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            list: cx.new(HintList::new),
        }
    }
}

impl Render for HintLayer {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        deferred(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(self.list.clone()),
        )
        .with_priority(HINT_LAYER_PRIORITY)
    }
}

impl RootPlugin for HintLayer {}

/// The window's mounted hints.
///
/// Lifecycle — the enter and exit phases, autohide, replace-by-id — is
/// `gpui_base`'s toast manager, the same machinery the kit's notification list
/// runs on. The presentation is [`HintCard`], so the card can be a plain message
/// that dismisses anywhere.
pub struct HintList {
    hints: ToastManager<HintId, Hint>,
    /// Hover and focus state of the stack. An expanded stack pauses the autohide
    /// countdown, so a hint the pointer rests on does not vanish under it.
    stack_state: ToastStackState,
    focus_handle: FocusHandle,
    /// Whether the lifecycle clock is running. The loop clears it as it exits.
    is_advancing: bool,
}

impl HintList {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            hints: ToastManager::new(hint_motion()),
            stack_state: ToastStackState::default(),
            focus_handle: cx.focus_handle().tab_stop(true),
            is_advancing: false,
        }
    }

    /// The hints currently mounted, in display order.
    ///
    /// A hint that is running its exit transition is still mounted, and still
    /// listed, until its card has left.
    pub fn hints(&self) -> Vec<&Hint> {
        self.hints.iter().map(|(_, hint, _)| hint).collect()
    }

    /// Mounts a hint, replacing the mounted hint with the same id.
    ///
    /// Replacing keeps a repeated report to one card, but the reports need not
    /// know the same things and need not arrive in a fixed order, so a hint with
    /// copy text takes over what the one it replaces could copy as well.
    fn push(&mut self, mut hint: Hint, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(previous) = self
            .hints
            .get(&hint.id)
            .and_then(|previous| previous.copy_text.as_deref())
            && let Some(next) = hint.copy_text.as_deref()
        {
            hint.copy_text = Some(merged_copy_text(previous, next).into());
        }
        let timeout = hint.autohide.then_some(HINT_AUTOHIDE);
        self.hints.push(
            hint.id.clone(),
            hint,
            ToastOptions { timeout },
            cx.background_executor().now(),
        );
        self.start_advancing(window, cx);
        cx.notify();
    }

    /// Start one hint's exit transition.
    ///
    /// The manager unmounts it once the transition's clock has run out.
    fn dismiss(&mut self, id: &HintId, window: &mut Window, cx: &mut Context<Self>) {
        if self.hints.dismiss(id, cx.background_executor().now()) {
            self.start_advancing(window, cx);
        }
        cx.notify();
    }

    /// Dismiss the frontmost hint — the one a keyboard dismissal addresses.
    ///
    /// A hint that is already leaving is skipped: it is on its way out, so the
    /// next press has to reach the card behind it.
    fn dismiss_front(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let front = self
            .hints
            .iter()
            .rev()
            .find(|(_, _, status)| *status != ToastTransitionStatus::Ending)
            .map(|(id, _, _)| id.clone());
        if let Some(front) = front {
            self.dismiss(&front, window, cx);
        }
    }

    /// Tick the hint lifecycle while a hint still has time to track.
    ///
    /// It advances the transition phases and samples the pause input (stack
    /// expansion) that reaches the list through no event. With nothing mounted,
    /// or only persistent hints at rest, there is nothing to do, so the window
    /// arms no timer until a push or a close.
    fn start_advancing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_advancing {
            return;
        }
        self.is_advancing = true;
        // Detached rather than kept as a `Task`: the loop ends itself, and a
        // handle on the list would have to be dropped from inside its own future.
        cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor().timer(HINT_ADVANCE_INTERVAL).await;
                let running = view.update_in(cx, |view, _, cx| {
                    view.advance(cx);
                    view.is_advancing = view.needs_clock();
                    view.is_advancing
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Whether a mounted hint is in transition or counting down to autohide. A
    /// paused countdown still needs its last sample kept current.
    fn needs_clock(&self) -> bool {
        self.hints.iter().any(|(_, hint, status)| match status {
            ToastTransitionStatus::Present => hint.autohide,
            ToastTransitionStatus::Starting | ToastTransitionStatus::Ending => true,
        })
    }

    fn advance(&mut self, cx: &mut Context<Self>) {
        // A card reads its phase straight off the manager while the list
        // renders, so a phase change only has to re-render the list. There is no
        // per-hint entity to keep in sync.
        let changed = self
            .hints
            .advance(
                cx.background_executor().now(),
                self.stack_state.is_expanded(),
            )
            .changed;
        if changed {
            cx.notify();
        }
    }
}

impl Render for HintList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (placement, margins, width, max_items) = {
            let settings = &cx.theme().notification;
            (
                settings.placement,
                settings.margins.clone(),
                settings.width,
                settings.max_items,
            )
        };
        let list = cx.entity().downgrade();
        let items = self
            .hints
            .visible(max_items)
            .map(|(id, hint, status)| {
                (
                    ElementId::from(format!("{id:?}")),
                    hint_card(hint.clone(), status, list.clone(), cx).into_any_element(),
                )
            })
            .collect::<Vec<_>>();

        let stack = items
            .into_iter()
            .fold(
                ToastStack::new(stack_id(placement), self.stack_state.clone()),
                |stack, (id, item)| stack.item(id, item),
            )
            .placement(placement)
            .focus_handle(self.focus_handle.clone())
            .on_key_down({
                // Escape dismisses the front hint while the keyboard is on the
                // layer. The card is the only dismiss affordance a hint has, so
                // without this a hint that outlives its autohide — a failed
                // session fork — would be un-dismissable without a pointer.
                move |event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key != "escape" {
                        return;
                    }
                    if let Some(list) = list.upgrade() {
                        list.update(cx, |list, cx| list.dismiss_front(window, cx));
                        cx.stop_propagation();
                    }
                }
            })
            .v_flex()
            .w(width)
            .max_h(window.viewport_size().height)
            .absolute();
        let stack = match placement {
            Anchor::TopLeft => stack.top(margins.top).left(margins.left),
            Anchor::TopRight => stack.top(margins.top).right(margins.right),
            Anchor::TopCenter => stack.top(margins.top).left(relative(0.5)).ml(-width / 2.),
            Anchor::BottomLeft => stack.bottom(margins.bottom).left(margins.left),
            Anchor::BottomRight => stack.bottom(margins.bottom).right(margins.right),
            Anchor::BottomCenter => stack
                .bottom(margins.bottom)
                .left(relative(0.5))
                .ml(-width / 2.),
            Anchor::LeftCenter => stack.left(margins.left).top_0().bottom_0().my_auto(),
            Anchor::RightCenter => stack.right(margins.right).top_0().bottom_0().my_auto(),
        };

        div().size_full().child(stack)
    }
}

/// What a card copies once `next` has replaced `previous`: both, in the order
/// they arrived, without repeating a line either already holds.
fn merged_copy_text(previous: &str, next: &str) -> String {
    let mut merged = previous.to_string();
    for line in next.lines() {
        if !previous.lines().any(|known| known == line) {
            merged.push('\n');
            merged.push_str(line);
        }
    }
    merged
}

/// The element id one placement's stack renders under.
///
/// Keyed by the placement rather than by position among the mounted stacks: a
/// stack whose id changes loses the element state of its whole subtree, which
/// replays the enter animation of every hint in it.
fn stack_id(anchor: Anchor) -> ElementId {
    let ix = match anchor {
        Anchor::TopLeft => 0,
        Anchor::TopCenter => 1,
        Anchor::TopRight => 2,
        Anchor::BottomLeft => 3,
        Anchor::BottomCenter => 4,
        Anchor::BottomRight => 5,
        Anchor::LeftCenter => 6,
        Anchor::RightCenter => 7,
    };
    ("hint-list", ix as usize).into()
}

/// The stack's motion, matching [`HINT_ENTER`] and [`HINT_EXIT`].
fn hint_motion() -> ToastMotion {
    let base = ToastMotion::sonner();
    ToastMotion {
        duration: HINT_ENTER.total().mul_f32(motion::speed_scale()),
        exit_duration: HINT_EXIT.total().mul_f32(motion::speed_scale()),
        ..base
    }
}

/// One hint's card.
///
/// It keeps the kit notification card's geometry — the tone glyph hanging in the
/// leading padding, the message, one trailing icon button — and drops its close
/// control: with the × gone the card itself is the dismiss target, which is what
/// the hit area a reader aims at already was.
fn hint_card(
    hint: Hint,
    status: ToastTransitionStatus,
    list: WeakEntity<HintList>,
    cx: &mut App,
) -> impl IntoElement {
    let closing = status == ToastTransitionStatus::Ending;
    let placement = cx.theme().notification.placement;
    let icon = match hint.tone {
        NotificationType::Info => Icon::new(IconName::Info).text_color(cx.theme().info),
        NotificationType::Success => {
            Icon::new(IconName::CircleCheck).text_color(cx.theme().success)
        }
        NotificationType::Warning => {
            Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning)
        }
        NotificationType::Error => Icon::new(IconName::CircleX).text_color(cx.theme().danger),
    };
    let copy_label = if hint.copy_text.is_some() {
        locale::text("Copy details", "复制详情", "複製詳情")
    } else {
        locale::text("Copy message", "复制消息", "複製訊息")
    };
    let copy_value = hint
        .copy_text
        .clone()
        .unwrap_or_else(|| hint.message.clone());
    // What a press on the card does, wherever it lands: a hint answers every
    // press the same way, and only the copy button below is excluded.
    let dismiss: Rc<HintDismissHandler> = {
        let list = list;
        let id = hint.id.clone();
        Rc::new(move |window, cx| {
            if let Some(list) = list.upgrade() {
                list.update(cx, |list, cx| list.dismiss(&id, window, cx));
            }
        })
    };
    let dismiss_click = dismiss.clone();
    let dismiss_middle = dismiss;
    let on_click = hint.on_click.clone();

    Toast::new("hint")
        .transition_status(status)
        .h_flex()
        .occlude()
        .debug_selector(|| HINT_CARD_DEBUG_KEY.to_string())
        .w_full()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().tokens.popover)
        .rounded(cx.theme().radius_lg)
        .shadow(hint_shadow(1.))
        .py_3p5()
        .px_4()
        .gap_3()
        .child(div().absolute().top(px(18.)).left_4().child(icon))
        .child(
            div().v_flex().flex_1().overflow_hidden().pl_6().child(
                div()
                    .text_sm()
                    .w_full()
                    .min_w_0()
                    .debug_selector(|| HINT_TEXT_DEBUG_KEY.to_string())
                    .child(hint.message.clone()),
            ),
        )
        .child(
            // The one control a press may land on without dismissing the card.
            // The kit's `Button` prevents focus on mouse down, so the caret
            // stays in whatever the user was typing at.
            Button::new(HINT_COPY_BUTTON_ID)
                .ghost()
                .flex_none()
                .small()
                // The trailing inset the kit gave an action, so the copy
                // button keeps the place it had on the card.
                .mr_3p5()
                .icon(IconName::Copy)
                .debug_selector(|| HINT_COPY_DEBUG_KEY.to_string())
                .accessibility_label(copy_label)
                .tooltip(copy_label)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_value.to_string()));
                    // A copy is not a decision to close the hint: the press
                    // stops at the button so the message stays on screen to
                    // be read or copied again.
                    cx.stop_propagation();
                }),
        )
        .on_click(move |event: &ClickEvent, window, cx| {
            dismiss_click(window, cx);
            if let Some(on_click) = &on_click {
                on_click(event, window, cx);
            }
        })
        .on_aux_click(move |event: &ClickEvent, window, cx| {
            if event.is_middle_click() {
                dismiss_middle(window, cx);
            }
        })
        .with_animation(
            ElementId::NamedInteger("hint-slide".into(), closing as u64),
            if closing {
                HINT_EXIT.animation()
            } else {
                HINT_ENTER.animation()
            },
            move |this, delta| {
                // A hint enters from the edge it is anchored to and leaves
                // the same way. The shadow is ramped by the cube of the
                // fade: gpui paints it *under* the card and has no group
                // compositing, so a full-ink shadow would show through a
                // card that is still translucent as a dark slab.
                let opacity = if closing { 1. - delta } else { delta };
                let offset = match placement {
                    Anchor::TopLeft | Anchor::TopRight | Anchor::TopCenter => {
                        (if closing { -delta } else { delta - 1. }) * HINT_SLIDE_DISTANCE
                    }
                    Anchor::BottomLeft | Anchor::BottomRight | Anchor::BottomCenter => {
                        (if closing { delta } else { 1. - delta }) * HINT_SLIDE_DISTANCE
                    }
                    _ => px(0.),
                };
                this.top(offset)
                    .opacity(opacity)
                    .shadow(hint_shadow(opacity.powi(3)))
            },
        )
}

/// The card's elevation at `strength` of its full ink.
///
/// It keeps the kit notification card's lift — shadcn's `shadow-lg` with
/// Tailwind's blur radii **halved**, which is the conversion CSS requires and
/// not a taste adjustment: CSS defines a blur radius as twice the gaussian's
/// standard deviation while gpui's shader takes the field as the deviation
/// itself. Repeating Tailwind's `10px`/`4px` would spread the shadow over twice
/// the distance and read as a haze around the card.
fn hint_shadow(strength: f32) -> Vec<BoxShadow> {
    let ink = hsla(0., 0., 0., HINT_SHADOW_INK * strength.clamp(0., 1.));
    vec![
        BoxShadow::new(px(0.), px(10.), ink)
            .blur_radius(px(7.5))
            .spread_radius(px(-3.)),
        BoxShadow::new(px(0.), px(4.), ink)
            .blur_radius(px(3.))
            .spread_radius(px(-4.)),
    ]
}
