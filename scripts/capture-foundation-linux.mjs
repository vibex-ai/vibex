import { createHash } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { classifyGpuiEvidence } from './evidence-applicability.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const EVIDENCE = 'docs/platform/evidence/foundation-linux.json';
const SCREENSHOT_ROOT = 'docs/parity/screenshots/current/foundation';
const APP_ID = 'dev.vibex.desktop.preview';
const RUNTIME_READY = 'vibex-foundation: runtime-ready';
const UI_STATE_FLUSHED = 'vibex-foundation: ui-state-flushed';
const RUNTIME_STOPPED = 'vibex-foundation: runtime-stopped';
const SETTINGS_DIALOG_OPEN = 'vibex-foundation: settings-dialog-open';
const REQUIRED_VIEWPORTS = [
  [1600, 1000],
  [1200, 780],
  [900, 720],
  [760, 1000],
  [360, 800],
  [360, 620]
];
const SOURCE_ROOTS = [
  'Cargo.lock',
  'Cargo.toml',
  'apps/desktop/Cargo.toml',
  'apps/desktop/src',
  'crates/desktop-model/Cargo.toml',
  'crates/desktop-model/src',
  'crates/desktop-runtime/Cargo.toml',
  'crates/desktop-runtime/src',
  'crates/vibex-ui',
  'scripts/generate-tokens.mjs',
  'scripts/capture-foundation-linux.mjs'
];

function fail(message) {
  throw new Error(message);
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function repo(path) {
  return join(ROOT, path);
}

function sourceFiles(path) {
  const absolute = repo(path);
  if (!existsSync(absolute)) fail('Missing Foundation input ' + path);
  if (statSync(absolute).isFile()) return [path];
  return readdirSync(absolute, { withFileTypes: true })
    .sort((left, right) => left.name.localeCompare(right.name))
    .flatMap((entry) => sourceFiles(path + '/' + entry.name));
}

function sourceTreeSha256() {
  const hash = createHash('sha256');
  for (const path of SOURCE_ROOTS.flatMap(sourceFiles)) {
    hash.update(path);
    hash.update('\0');
    hash.update(readFileSync(repo(path)));
    hash.update('\0');
  }
  return hash.digest('hex');
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: 'utf8',
    timeout: 60_000,
    maxBuffer: 16 * 1024 * 1024,
    ...options
  });
  if (result.error) fail(command + ' failed to start: ' + result.error.message);
  if (result.status !== 0) {
    fail(command + ' ' + args.join(' ') + ' failed:\n' + (result.stderr || result.stdout || ''));
  }
  return result.stdout || '';
}

function hyprlandJson(args) {
  return JSON.parse(run('hyprctl', [...args, '-j']));
}

async function waitForClient(app) {
  for (let attempt = 0; attempt < 150; attempt += 1) {
    if (app.exitCode !== null) fail('GPUI exited before creating a Foundation window');
    const client = hyprlandJson(['clients']).find(
      (candidate) => candidate.pid === app.pid && candidate.class === APP_ID
    );
    if (client) return client;
    await delay(100);
  }
  fail('Timed out waiting for the GPUI Foundation window');
}

async function waitForRuntime(app, stderr) {
  for (let attempt = 0; attempt < 600; attempt += 1) {
    if (stderr.value.includes(RUNTIME_READY)) return;
    if (stderr.value.includes('vibex-foundation: runtime-failed')) {
      fail('GPUI Foundation runtime failed:\n' + stderr.value);
    }
    if (app.exitCode !== null) fail('GPUI exited before its shared runtime became ready');
    await delay(100);
  }
  fail('Timed out waiting for the shared GPUI runtime:\n' + stderr.value);
}

async function waitForMarker(app, stderr, marker, description) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (stderr.value.includes(marker)) return;
    if (app.exitCode !== null) fail('GPUI exited before ' + description);
    await delay(100);
  }
  fail('Timed out waiting for ' + description + ':\n' + stderr.value);
}

function parseMetrics(output) {
  const [width, height, colors, entropy, mean, deviation] = output.trim().split('\t').map(Number);
  const metrics = {
    width,
    height,
    uniqueColors: colors,
    entropy,
    mean,
    standardDeviation: deviation
  };
  if (Object.values(metrics).some((value) => !Number.isFinite(value))) {
    fail('ImageMagick returned invalid metrics: ' + output);
  }
  return metrics;
}

function defaultUiState(theme, locale) {
  return {
    schemaVersion: 1,
    sourceAppVersion: '0.1.0-rc.1',
    appearance: {
      theme,
      locale,
      interfaceFont: { family: 'Inter Variable', size: 14, weight: 400 },
      codeFont: { family: null, size: 13, weight: 400 },
      reducedMotion: false,
      highContrast: false
    },
    workbench: {
      activeTab: 'agent',
      selectedWorkspaceId: null,
      selectedSessionId: null,
      sidebarVisible: true,
      previewVisible: true,
      rightRailVisible: true,
      sidebarWidth: 280,
      previewWidth: 520,
      rightRailWidth: 360
    },
    sidebar: {
      projectOrder: [],
      sessionOrder: [],
      pinnedSessionIds: [],
      collapsedProjectIds: []
    },
    preview: { focusedPaneId: null, pinnedTabIds: [], splitSizes: [1] },
    terminal: { tabOrder: [], selectedTerminalId: null },
    rightRail: { activityOrder: [], selectedActivityId: null },
    migration: { importedFromTauri: false, sourceSchema: null }
  };
}

function prepareHome(theme, locale) {
  const root = mkdtempSync(join(tmpdir(), 'vibex-foundation-'));
  const previewHome = join(root, 'desktop-preview');
  mkdirSync(previewHome, { recursive: true });
  writeFileSync(
    join(previewHome, 'desktop-ui-state.json'),
    JSON.stringify(defaultUiState(theme, locale), null, 2) + '\n'
  );
  return root;
}

async function waitForExit(app, timeoutMs) {
  if (app.exitCode !== null) return app.exitCode;
  return await Promise.race([
    new Promise((resolveExit) => app.once('exit', (code) => resolveExit(code))),
    delay(timeoutMs).then(() => fail('Timed out waiting for the GPUI process to exit'))
  ]);
}

async function captureScenario(binary, monitor, scenario) {
  const home = prepareHome(scenario.theme, scenario.locale);
  const stderr = { value: '' };
  let app;
  try {
    const env = {
      ...process.env,
      XDG_SESSION_TYPE: 'wayland',
      VIBEX_DB_PATH: join(home, 'vibex.db'),
      VIBEX_FOUNDATION_SKIP_ADAPTER_INSTALL: '1',
      LANG: scenario.systemLocale
    };
    if (scenario.settingsOpen) env.VIBEX_FOUNDATION_OPEN_SETTINGS = '1';
    app = spawn(binary, [], {
      cwd: ROOT,
      env,
      stdio: ['ignore', 'ignore', 'pipe']
    });
    app.stderr.on('data', (chunk) => {
      if (stderr.value.length < 128 * 1024) stderr.value += chunk.toString('utf8');
    });
    let client = await waitForClient(app);
    const selector = 'address:' + client.address;
    if (!client.floating) run('hyprctl', ['dispatch', 'togglefloating', selector]);
    await waitForRuntime(app, stderr);
    if (scenario.settingsOpen) {
      await waitForMarker(app, stderr, SETTINGS_DIALOG_OPEN, 'the settings dialog');
    }
    run('hyprctl', [
      'dispatch',
      'resizewindowpixel',
      'exact 1200 780,' + selector
    ]);
    run('hyprctl', [
      'dispatch',
      'movewindowpixel',
      'exact ' + (monitor.x + 60) + ' ' + (monitor.y + 60) + ',' + selector
    ]);
    await delay(300);
    const captures = [];
    for (const [width, height] of scenario.viewports) {
      run('hyprctl', [
        'dispatch',
        'movewindowpixel',
        'exact ' + (monitor.x + 60) + ' ' + (monitor.y + 60) + ',' + selector
      ]);
      let requestedWidth = width;
      let requestedHeight = height;
      for (let attempt = 0; attempt < 3; attempt += 1) {
        run('hyprctl', [
          'dispatch',
          'resizewindowpixel',
          'exact ' + requestedWidth + ' ' + requestedHeight + ',' + selector
        ]);
        await delay(300);
        client = hyprlandJson(['clients']).find(
          (candidate) => candidate.address === client.address
        );
        if (!client) fail('Foundation window disappeared during ' + scenario.id);
        if (client.size[0] === width && client.size[1] === height) break;
        requestedWidth += width - client.size[0];
        requestedHeight += height - client.size[1];
      }
      run('hyprctl', [
        'dispatch',
        'movewindowpixel',
        'exact ' + (monitor.x + 60) + ' ' + (monitor.y + 60) + ',' + selector
      ]);
      await delay(400);
      client = hyprlandJson(['clients']).find((candidate) => candidate.address === client.address);
      if (!client) fail('Foundation window disappeared during ' + scenario.id);
      if (client.xwayland) fail('Foundation matrix unexpectedly used XWayland');
      if (
        Math.abs(client.size[0] - width) > 1 ||
        Math.abs(client.size[1] - height) > 1
      ) {
        fail(
          scenario.id +
            ' expected ' +
            width +
            'x' +
            height +
            ', got ' +
            client.size.join('x')
        );
      }
      const screenshotPath =
        SCREENSHOT_ROOT + '/' + scenario.id + '-' + width + 'x' + height + '.png';
      mkdirSync(dirname(repo(screenshotPath)), { recursive: true });
      run('grim', [
        '-g',
        client.at[0] + ',' + client.at[1] + ' ' + width + 'x' + height,
        repo(screenshotPath)
      ]);
      const metrics = parseMetrics(
        run('identify', [
          '-format',
          '%w\t%h\t%k\t%[entropy]\t%[fx:mean]\t%[fx:standard_deviation]',
          repo(screenshotPath)
        ])
      );
      if (
        metrics.width !== width ||
        metrics.height !== height ||
        metrics.uniqueColors <= 16 ||
        metrics.entropy <= 0.01 ||
        metrics.standardDeviation <= 0.01
      ) {
        fail('Foundation capture is blank or implausible: ' + screenshotPath);
      }
      captures.push({
        viewport: { width, height },
        window: {
          appId: APP_ID,
          xwayland: client.xwayland,
          floating: client.floating,
          scale: monitor.scale
        },
        screenshotPath,
        screenshotSha256: sha256(readFileSync(repo(screenshotPath))),
        metrics
      });
    }
    return {
      id: scenario.id,
      theme: scenario.theme,
      locale: scenario.locale,
      systemLocale: scenario.systemLocale,
      settingsOpen: scenario.settingsOpen,
      settingsSheetObserved: scenario.settingsOpen
        ? stderr.value.includes(SETTINGS_DIALOG_OPEN)
        : false,
      runtimeReady: true,
      captures
    };
  } finally {
    if (app && app.exitCode === null) app.kill('SIGTERM');
    await delay(400);
    if (app && app.exitCode === null) app.kill('SIGKILL');
    rmSync(home, { recursive: true, force: true });
  }
}

async function captureGracefulClose(binary, lockProbe, monitor) {
  const home = prepareHome('light', 'en');
  const previewHome = join(home, 'desktop-preview');
  const uiStatePath = join(previewHome, 'desktop-ui-state.json');
  const initialState = JSON.parse(readFileSync(uiStatePath, 'utf8'));
  initialState.exitFlushSentinel = 'must-be-removed';
  writeFileSync(uiStatePath, JSON.stringify(initialState) + '\n');
  const beforeHash = sha256(readFileSync(uiStatePath));
  const stderr = { value: '' };
  let app;
  try {
    app = spawn(binary, [], {
      cwd: ROOT,
      env: {
        ...process.env,
        XDG_SESSION_TYPE: 'wayland',
        VIBEX_DB_PATH: join(home, 'vibex.db'),
        VIBEX_FOUNDATION_SKIP_ADAPTER_INSTALL: '1',
        LANG: 'en_US.UTF-8'
      },
      stdio: ['ignore', 'ignore', 'pipe']
    });
    app.stderr.on('data', (chunk) => {
      if (stderr.value.length < 128 * 1024) stderr.value += chunk.toString('utf8');
    });
    const client = await waitForClient(app);
    const selector = 'address:' + client.address;
    if (!client.floating) run('hyprctl', ['dispatch', 'togglefloating', selector]);
    run('hyprctl', [
      'dispatch',
      'movewindowpixel',
      'exact ' + (monitor.x + 60) + ' ' + (monitor.y + 60) + ',' + selector
    ]);
    await waitForRuntime(app, stderr);
    const contending = spawnSync(lockProbe, [previewHome], {
      cwd: ROOT,
      encoding: 'utf8',
      timeout: 10_000
    });
    if (
      contending.status !== 3 ||
      !contending.stderr.includes('desktop_runtime_home_locked')
    ) {
      fail('A second process unexpectedly acquired the live GPUI runtime home');
    }
    run('hyprctl', ['dispatch', 'closewindow', selector]);
    const exitCode = await waitForExit(app, 20_000);
    if (exitCode !== 0) {
      fail('GPUI graceful close exited with code ' + exitCode + ':\n' + stderr.value);
    }
    if (!stderr.value.includes(UI_STATE_FLUSHED) || !stderr.value.includes(RUNTIME_STOPPED)) {
      fail('GPUI graceful close missed lifecycle markers:\n' + stderr.value);
    }
    const persistedBytes = readFileSync(uiStatePath);
    const persistedState = JSON.parse(persistedBytes.toString('utf8'));
    if (
      persistedState.exitFlushSentinel !== undefined ||
      sha256(persistedBytes) === beforeHash
    ) {
      fail('GPUI graceful close did not perform the final UI-state flush');
    }
    const released = spawnSync(lockProbe, [previewHome], {
      cwd: ROOT,
      encoding: 'utf8',
      timeout: 10_000
    });
    if (released.status !== 0 || released.stdout.trim() !== 'locked') {
      fail('GPUI graceful close did not release the runtime home lock');
    }
    return {
      closeDispatch: 'hyprctl dispatch closewindow',
      processExitCode: exitCode,
      uiStateExitFlush: true,
      runtimeShutdownAwaited: true,
      homeLockedWhileRunning: true,
      homeLockReleasedAfterExit: true
    };
  } finally {
    if (app && app.exitCode === null) app.kill('SIGTERM');
    await delay(400);
    if (app && app.exitCode === null) app.kill('SIGKILL');
    rmSync(home, { recursive: true, force: true });
  }
}

function verify() {
  if (!existsSync(repo(EVIDENCE))) fail(EVIDENCE + ' is missing');
  const evidence = JSON.parse(readFileSync(repo(EVIDENCE), 'utf8'));
  if (evidence.schemaVersion !== 'foundation-linux.v1') {
    fail('Unsupported GPUI Foundation evidence schema');
  }
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE, evidence.source);
  if (applicability === 'current') {
    if (
      evidence.source.cargoLockSha256 !== sha256(readFileSync(repo('Cargo.lock'))) ||
      evidence.source.foundationTreeSha256 !== sourceTreeSha256()
    ) {
      fail('GPUI Foundation evidence source identity is stale');
    }
  }
  const defaultScenario = evidence.scenarios.find((scenario) => scenario.id === 'default-light-en');
  const settingsScenario = evidence.scenarios.find(
    (scenario) => scenario.id === 'settings-dark-zh-tw'
  );
  if (
    !defaultScenario?.runtimeReady ||
    JSON.stringify(defaultScenario.captures.map((capture) => [
      capture.viewport.width,
      capture.viewport.height
    ])) !== JSON.stringify(REQUIRED_VIEWPORTS)
  ) {
    fail('Default Foundation viewport matrix is incomplete');
  }
  if (
    !settingsScenario?.runtimeReady ||
    settingsScenario.theme !== 'dark' ||
    settingsScenario.locale !== 'zh_tw' ||
    settingsScenario.settingsOpen !== true ||
    settingsScenario.settingsSheetObserved !== true
  ) {
    fail('Dark Traditional Chinese settings scenario is incomplete');
  }
  for (const scenario of evidence.scenarios) {
    for (const capture of scenario.captures) {
      const screenshot = repo(capture.screenshotPath);
      if (
        !existsSync(screenshot) ||
        capture.screenshotSha256 !== sha256(readFileSync(screenshot)) ||
        capture.metrics.uniqueColors <= 16 ||
        capture.metrics.entropy <= 0.01 ||
        capture.metrics.standardDeviation <= 0.01 ||
        capture.window.xwayland !== false ||
        capture.window.scale !== 1
      ) {
        fail('Invalid Foundation capture ' + capture.screenshotPath);
      }
    }
  }
  if (
    evidence.lifecycle?.processExitCode !== 0 ||
    evidence.lifecycle?.uiStateExitFlush !== true ||
    evidence.lifecycle?.runtimeShutdownAwaited !== true ||
    evidence.lifecycle?.homeLockedWhileRunning !== true ||
    evidence.lifecycle?.homeLockReleasedAfterExit !== true
  ) {
    fail('GPUI Foundation graceful-close evidence is incomplete');
  }
  console.log(
    'GPUI Foundation Linux evidence verified: ' +
      evidence.scenarios.reduce((count, scenario) => count + scenario.captures.length, 0) +
      ' native captures; applicability=' + applicability
  );
}

async function capture() {
  for (const command of ['cargo', 'grim', 'hyprctl', 'identify']) {
    if (spawnSync('which', [command]).status !== 0) fail('Missing command ' + command);
  }
  if (process.platform !== 'linux' || process.env.XDG_SESSION_TYPE !== 'wayland') {
    fail('GPUI Foundation capture requires a physical Linux Wayland session');
  }
  const build = spawnSync(
    'cargo',
    ['build', '-p', 'vibex-desktop', '--locked'],
    { cwd: ROOT, stdio: 'inherit' }
  );
  if (build.status !== 0) fail('GPUI Foundation build failed');
  const lockProbeBuild = spawnSync(
    'cargo',
    ['build', '-p', 'vibex-desktop-runtime', '--bin', 'vibex-home-lock-probe', '--locked'],
    { cwd: ROOT, stdio: 'inherit' }
  );
  if (lockProbeBuild.status !== 0) fail('Runtime home-lock probe build failed');
  const monitor =
    hyprlandJson(['monitors']).find((candidate) => candidate.focused) ??
    hyprlandJson(['monitors'])[0];
  if (!monitor || monitor.width < 1720 || monitor.height < 1120 || monitor.scale !== 1) {
    fail('Foundation capture requires a scale-1 monitor large enough for 1600x1000');
  }
  const binary = repo('target/debug/vibex-desktop');
  const lockProbe = repo('target/debug/vibex-home-lock-probe');
  const scenarios = [];
  scenarios.push(
    await captureScenario(binary, monitor, {
      id: 'default-light-en',
      theme: 'light',
      locale: 'en',
      systemLocale: 'en_US.UTF-8',
      settingsOpen: false,
      viewports: REQUIRED_VIEWPORTS
    })
  );
  scenarios.push(
    await captureScenario(binary, monitor, {
      id: 'settings-dark-zh-tw',
      theme: 'dark',
      locale: 'zh_tw',
      systemLocale: 'zh_TW.UTF-8',
      settingsOpen: true,
      viewports: [[760, 1000]]
    })
  );
  const lifecycle = await captureGracefulClose(binary, lockProbe, monitor);
  const evidence = {
    schemaVersion: 'foundation-linux.v1',
    capturedAt: new Date().toISOString(),
    source: {
      parentCommit: run('git', ['rev-parse', 'HEAD']).trim(),
      cargoLockSha256: sha256(readFileSync(repo('Cargo.lock'))),
      foundationTreeSha256: sourceTreeSha256()
    },
    runner: {
      platform: process.platform,
      architecture: process.arch,
      compositor: 'Hyprland',
      displayBackend: 'wayland',
      syntheticDisplay: false,
      monitor: {
        width: monitor.width,
        height: monitor.height,
        scale: monitor.scale
      }
    },
    requiredViewports: REQUIRED_VIEWPORTS.map(([width, height]) => ({ width, height })),
    scenarios,
    lifecycle
  };
  mkdirSync(dirname(repo(EVIDENCE)), { recursive: true });
  writeFileSync(repo(EVIDENCE), JSON.stringify(evidence, null, 2) + '\n');
  verify();
}

if (process.argv.includes('--write')) {
  await capture();
} else if (process.argv.length === 2) {
  verify();
} else {
  fail('usage: node scripts/capture-foundation-linux.mjs [--write]');
}
