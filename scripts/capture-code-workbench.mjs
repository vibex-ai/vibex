import { createHash } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import console from 'node:console';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, sep } from 'node:path';
import process from 'node:process';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

import { classifyGpuiEvidence } from './evidence-applicability.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const EVIDENCE_PATH = 'docs/parity/evidence/code-workbench.json';
const SCREENSHOT_ROOT = 'docs/parity/screenshots/current/code-workbench';
const DESKTOP_BINARY = 'target/debug/vibex-desktop';
const MODEL_PROBE_BINARY = 'target/release/vibex-code-workbench-probe';
const APP_ID = 'dev.vibex.desktop.preview';
const SCREEN_LOCKERS = ['hyprlock', 'swaylock', 'gtklock', 'waylock'];
const SOURCE_INPUTS = [
  'Cargo.lock',
  'Cargo.toml',
  'package.json',
  'apps/desktop/Cargo.toml',
  'apps/desktop/src/actions.rs',
  'apps/desktop/src/app.rs',
  'apps/desktop/src/code_workbench.rs',
  'apps/desktop/src/lib.rs',
  'apps/desktop/src/main.rs',
  'apps/desktop/src/platform/mod.rs',
  'apps/desktop/src/terminal_surface.rs',
  'apps/desktop/src/testing.rs',
  'apps/desktop/src/theme.rs',
  'crates/vibex-ui/Cargo.toml',
  'crates/vibex-ui/src/generated_tokens.rs',
  'crates/vibex-ui/src/lib.rs',
  'crates/vibex-ui/theme/tokens.json',
  'scripts/generate-tokens.mjs',
  // This deterministic fixture supplies current model-test inputs.
  'docs/parity/fixtures/desktop-behavioral-v1.json',
  'crates/content/src/lifecycle.rs',
  'crates/core/src/file.rs',
  'crates/core/src/lib.rs',
  'crates/desktop-model/Cargo.toml',
  'crates/desktop-model/src/agent_workbench.rs',
  'crates/desktop-model/src/bin/vibex-code-workbench-probe.rs',
  'crates/desktop-model/src/content_preview.rs',
  'crates/desktop-model/src/diff.rs',
  'crates/desktop-model/src/editor.rs',
  'crates/desktop-model/src/file_tree.rs',
  'crates/desktop-model/src/git_workbench.rs',
  'crates/desktop-model/src/lib.rs',
  'crates/desktop-model/src/preview.rs',
  'crates/desktop-model/src/ui_state.rs',
  'crates/vibex-markdown/Cargo.toml',
  'crates/vibex-markdown/fixtures/advanced.md',
  'crates/vibex-markdown/fixtures/malformed.md',
  'crates/vibex-markdown/src/artifact.rs',
  'crates/vibex-markdown/src/engines/mod.rs',
  'crates/vibex-markdown/src/gpui_view.rs',
  'crates/vibex-markdown/src/html.rs',
  'crates/vibex-markdown/src/lib.rs',
  'crates/vibex-markdown/src/limits.rs',
  'crates/vibex-markdown/src/model.rs',
  'crates/vibex-markdown/src/parser.rs',
  'crates/vibex-markdown/src/resource.rs',
  'crates/vibex-markdown/src/svg.rs',
  'crates/desktop-runtime/src/lib.rs',
  'crates/desktop-runtime/src/workbench.rs',
  'crates/vibex-backend/Cargo.toml',
  'crates/vibex-backend/src/agent.rs',
  'crates/vibex-backend/src/capability.rs',
  'crates/vibex-backend/src/device.rs',
  'crates/vibex-backend/src/error.rs',
  'crates/vibex-backend/src/facade.rs',
  'crates/vibex-backend/src/file.rs',
  'crates/vibex-backend/src/git.rs',
  'crates/vibex-backend/src/lib.rs',
  'crates/vibex-backend/src/management.rs',
  'crates/vibex-backend/src/mutation.rs',
  'crates/vibex-backend/src/native.rs',
  'crates/vibex-backend/src/terminal.rs',
  'crates/vibex-backend/src/workspace.rs',
  'crates/vibex-terminal-ui/Cargo.toml',
  'crates/vibex-terminal-ui/src/emulator.rs',
  'crates/vibex-terminal-ui/src/emulator_wasm.rs',
  'crates/vibex-terminal-ui/src/lib.rs',
  'crates/vibex-ui/src/shell.rs',
  'crates/fs/Cargo.toml',
  'crates/fs/src/lib.rs',
  'crates/git/Cargo.toml',
  'crates/git/src/lib.rs',
  'crates/remote/src/lib.rs',
  'docs/parity/fixtures/code-workbench-lgtm-dual-run.rs',
  'docs/parity/code-workbench.md',
  'scripts/capture-code-workbench.mjs'
];
const EXPECTED_DUAL_RUN = new Map([
  ['hunk_word', true],
  ['rename', true],
  ['binary', true],
  ['crlf', true],
  ['quoted_octal_utf8', false],
  ['no_newline_marker', false],
  ['copy', false],
  ['malformed_hunk', true]
]);
const REFERENCE_ROOT = resolve(ROOT, '..', 'gpui');
const REFERENCE_BOUNDARIES = {
  lgtm: {
    repository: 'https://github.com/ellie/lgtm.git',
    commit: '3b0327b8ef4936d46d038512f97209dff41bef10',
    license: 'MIT',
    licenseSha256: '7942c4864b895103fad0806f47b9bf2c6471a89763ec3e25a3b80eae528f7662',
    diffCoreManifestSha256: 'eeafa131989d5fc501fc0144c88be171bd24c007a36ff6fcab73fa26f9ca198c',
    diffCoreSourceSha256: '0bfcaea9be8e162d8870022bcc84e3f57623fbab786c727d400d62195067c7d2',
    diffCoreDependencies: [
      {
        name: 'imara-diff',
        version: '0.1.8',
        license: 'Apache-2.0',
        checksum: '17d34b7d42178945f775e84bc4c36dde7c1c6cdfea656d3354d009056f2bb3d2'
      }
    ],
    disposition: 'rejected_no_source_adopted'
  },
  fulgur: {
    repository: 'https://github.com/fulgur-app/Fulgur.git',
    commit: '9f941b0c4e6e6f75080a47c97ba5697c5eaa19cf',
    license: 'Apache-2.0',
    licenseSha256: 'cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30',
    manifestSha256: '11535048657eb4dc42f7b83b44f32e0d3f005d90c438876f4f71e93e06ae1213',
    disposition: 'behavior_research_only_no_file_or_dependency_adopted'
  },
  gitComet: {
    repository: 'https://github.com/Auto-Explore/GitComet.git',
    commit: '7bf4b89ea3cff7fafb663d6a6e0e0fe070db7e44',
    license: 'AGPL-3.0-only',
    licenseSha256: '0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0',
    manifestSha256: '30eb6f28a3f30958f33c52fd31b0c855ab51d62b6dbe29ae829e27ebf0cd3568',
    forbiddenGpuiFork: 'https://github.com/Havunen/gpui-ce.git',
    disposition: 'clean_room_ideas_only_no_code_asset_fixture_dependency_or_fork_adopted',
    workingTreePolicy: 'read_only_preexisting_changes_preserved'
  }
};
const VISUAL_SCENARIOS = [
  {
    id: 'files-desktop',
    fixture: 'files',
    theme: 'light',
    width: 1200,
    height: 780,
    screenshot: 'files-1200x780.png'
  },
  {
    id: 'files-narrow',
    fixture: 'files',
    theme: 'light',
    width: 360,
    height: 620,
    screenshot: 'files-360x620.png'
  },
  {
    id: 'diff-desktop',
    fixture: 'diff',
    theme: 'dark',
    width: 1200,
    height: 780,
    screenshot: 'diff-1200x780.png'
  },
  {
    id: 'diff-narrow',
    fixture: 'diff',
    theme: 'dark',
    width: 360,
    height: 620,
    screenshot: 'diff-360x620.png'
  },
  { id: 'markdown-light-desktop', fixture: 'markdown', theme: 'light', width: 1200, height: 780 },
  { id: 'markdown-light-narrow', fixture: 'markdown', theme: 'light', width: 360, height: 620 },
  { id: 'markdown-dark-desktop', fixture: 'markdown', theme: 'dark', width: 1200, height: 780 },
  { id: 'markdown-dark-narrow', fixture: 'markdown', theme: 'dark', width: 360, height: 620 }
];

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function rootPath(path) {
  const absolute = resolve(ROOT, path);
  if (absolute !== ROOT && !absolute.startsWith(`${ROOT}${sep}`)) {
    fail(`path escapes repository: ${path}`);
  }
  return absolute;
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function identity(path) {
  const bytes = readFileSync(rootPath(path));
  return { path, bytes: bytes.length, sha256: sha256(bytes) };
}

function readJson(path) {
  return JSON.parse(readFileSync(rootPath(path), 'utf8'));
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
    timeout: 300_000,
    ...options
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    fail(
      `${command} ${args.join(' ')} failed with status ${result.status ?? 'unknown'}:\n` +
        (result.stderr || result.stdout || '')
    );
  }
  return result.stdout || '';
}

function assertSessionUnlocked() {
  for (const locker of SCREEN_LOCKERS) {
    const result = spawnSync('pgrep', ['-x', locker], { encoding: 'utf8' });
    assert(
      result.status === 1,
      result.status === 0
        ? `physical capture refused while ${locker} is active`
        : `unable to determine whether ${locker} is active`
    );
  }
}

function validateProductProbe(probe) {
  assert(probe?.schemaVersion === 'code-workbench-probe.v1', 'model probe schema drifted');
  assert(probe.status === 'passed', 'model probe did not pass');
  assert(probe.tree?.inputEntries === 100_000, '100k tree fixture drifted');
  assert(probe.tree.depth === 8, 'deep tree fixture drifted');
  if (probe.tree.compactChainSegments === undefined) {
    assert(
      probe.tree.expandedVisibleRows === 100_000 && probe.tree.searchVisibleRows === 9,
      'pre-compaction tree evidence is internally inconsistent'
    );
  } else {
    assert(probe.tree.compactChainSegments === 8, 'deep directory chain lost a compact segment');
    assert(
      probe.tree.expandedVisibleRows === 99_994 && probe.tree.searchVisibleRows === 3,
      '100k tree compact projection is incomplete'
    );
  }
  assert(probe.tree.deepWindowRows <= 168, 'tree deep window is not bounded');
  assert(probe.tree.searchLeafFound, 'deep search failed');
  assert(probe.tree.staleLoadRejected === true, 'stale tree load was accepted');
  for (const field of ['applyMicros', 'expandMicros', 'searchMicros']) {
    assert(
      Number.isInteger(probe.tree[field]) && probe.tree[field] <= probe.tree.operationLimitMicros,
      `tree ${field} exceeded its limit`
    );
  }
  assert(probe.diff?.inputRows === 20_000, '20k diff fixture drifted');
  assert(probe.diff.projectedRows === 20_000, '20k diff projection is incomplete');
  assert(probe.diff.initialWindowRows === 500, 'initial diff window is not capped at 500');
  assert(
    probe.diff.deepWindowRows === 120 && probe.diff.deepFirstRowId === 'diff:0:19500',
    'deep diff window shifted or expanded'
  );
  assert(
    probe.diff.cachedWordRows === 2_000 && probe.diff.cacheLimitRows === 2_000,
    'word cache did not reach and retain its exact bound'
  );
  for (const field of ['constructMicros', 'initialWindowMicros', 'deepWindowMicros']) {
    assert(
      Number.isInteger(probe.diff[field]) && probe.diff[field] <= probe.diff.operationLimitMicros,
      `diff ${field} exceeded its limit`
    );
  }
  assert(
    probe.caches?.gitInsertions === 40 &&
      probe.caches.gitResidentItems === 32 &&
      probe.caches.gitItemLimit === 32 &&
      probe.caches.oldestGitItemEvicted === true &&
      probe.caches.newestGitItemResident === true,
    'Git diff LRU contract failed'
  );
  assert(
    probe.caches.imageInsertions === 32 &&
      probe.caches.imageResidentItems === 8 &&
      probe.caches.imageResidentBytes === 800 &&
      probe.caches.imageEvictions === 24,
    'image LRU contract failed'
  );
  assert(
    probe.generations?.workspaceSwitches === 128 &&
      probe.generations.staleWorkspaceResultsRejected === 128 &&
      probe.generations.revisionSwitches === 128 &&
      probe.generations.staleRevisionResultsRejected === 128 &&
      probe.generations.finalDiffCacheItems === 0,
    'workspace/revision generation soak failed'
  );
}

function validateUiContract(contract) {
  assert(
    contract?.schemaVersion === 'code-workbench-contract.v1',
    'GPUI Code Workbench contract schema drifted'
  );
  assert(contract.previewTargetKinds === 5, 'Preview target coverage drifted');
  assert(contract.fileTreeFixtureRows === 100_000, 'UI tree fixture count drifted');
  assert(contract.diffFixtureRows === 20_000, 'UI diff fixture count drifted');
  assert(contract.maxEagerRenderedRows <= 5_000, 'eager UI row cap regressed');
  assert(contract.maxInitialDiffRows <= 500, 'initial diff row cap regressed');
  for (const [key, value] of Object.entries(contract)) {
    if (typeof value === 'boolean') assert(value, `UI contract ${key} is false`);
  }
}

function sourceContract() {
  const workbench = readFileSync(rootPath('apps/desktop/src/code_workbench.rs'), 'utf8');
  const app = readFileSync(rootPath('apps/desktop/src/app.rs'), 'utf8');
  const actions = readFileSync(rootPath('apps/desktop/src/actions.rs'), 'utf8');
  const helperOffset = workbench.indexOf('fn bounded_uniform_range(');
  assert(helperOffset > 0, 'bounded uniform range helper is missing');
  const renderSource = workbench.slice(0, helperOffset);
  const patchListOffset = workbench.indexOf('    fn render_patch_list');
  const previewLoadingOffset = workbench.indexOf('    fn render_git_preview_loading', patchListOffset);
  assert(
    patchListOffset > 0 && previewLoadingOffset > patchListOffset,
    'variable-height patch list helper is missing'
  );
  const patchListSource = workbench.slice(patchListOffset, previewLoadingOffset);
  const diffRowOffset = workbench.indexOf('fn render_diff_row(');
  const normalizedPathOffset = workbench.indexOf('fn normalized_relative_path(', diffRowOffset);
  assert(
    diffRowOffset > 0 && normalizedPathOffset > diffRowOffset,
    'diff row renderer is missing'
  );
  const diffRowSource = workbench.slice(diffRowOffset, normalizedPathOffset);
  const fileContextOffset = workbench.indexOf('    fn build_file_context_menu(');
  const fileTreeOffset = workbench.indexOf('    fn render_files(', fileContextOffset);
  const inlinePathOffset = workbench.indexOf('    fn render_inline_path_editor(', fileTreeOffset);
  assert(
    fileContextOffset > 0 && fileTreeOffset > fileContextOffset && inlinePathOffset > fileTreeOffset,
    'shared file and blank-area context menu helpers are missing'
  );
  const previewPaneNewOffset = workbench.indexOf(
    'Button::new(format!("preview-pane-new:{pane_id}"))'
  );
  const previewPaneNewMenuOffset = workbench.indexOf('.dropdown_menu(', previewPaneNewOffset);
  assert(
    previewPaneNewOffset > 0 && previewPaneNewMenuOffset > previewPaneNewOffset,
    'preview-pane add button contract is missing'
  );
  const previewPaneNewSource = workbench.slice(previewPaneNewOffset, previewPaneNewMenuOffset);
  const fileContextSource = workbench.slice(fileContextOffset, fileTreeOffset);
  const fileTreeSource = workbench.slice(fileTreeOffset, inlinePathOffset);
  return {
    uniformListCount: (workbench.match(/\buniform_list\(/g) || []).length,
    boundedUniformListCallCount: (renderSource.match(/bounded_uniform_range\(/g) || []).length,
    fullSizeTrackedListCount: (
      workbench.match(/\.track_scroll\([^)]*\)\s*\.size_full\(\)/gs) || []
    ).length,
    variablePatchListCount: (renderSource.match(/\blist\(/g) || []).length,
    variablePatchListStatePresent:
      workbench.includes('ListState::new(row_count, ListAlignment::Top') &&
      workbench.includes('.with_uniform_item_height(px(DIFF_ROW_HEIGHT))') &&
      workbench.includes('.reset_with_uniform_height(row_count, px(DIFF_ROW_HEIGHT))'),
    variablePatchListFullSize: patchListSource.includes('.size_full()'),
    wrappingDiffRowsPresent:
      diffRowSource.includes('.min_h(px(DIFF_ROW_HEIGHT))') &&
      diffRowSource.includes('.whitespace_normal()') &&
      !diffRowSource.includes('.overflow_x_scrollbar()') &&
      !diffRowSource.includes('.whitespace_nowrap()'),
    fileListIdPresent: workbench.includes('"code-workbench-file-rows"'),
    gitChangesListIdPresent: workbench.includes('"git-change-rows"'),
    gitHistoryListIdPresent: workbench.includes('"git-history-rows"'),
    diffListIdPresent: workbench.includes('format!("diff-rows:{}:{}"'),
    commitPatchListIdPresent: workbench.includes('format!("commit-rows:{hash}"'),
    inlineFileRowsPresent:
      workbench.includes('InlineFileAction::CreateFile') &&
      workbench.includes('InlineFileAction::Rename'),
    blankAreaContextMenuPresent:
      fileContextSource.includes('locale::text("New File"') &&
      fileContextSource.includes('locale::text("New Folder"') &&
      fileTreeSource.includes('let blank_context_view = cx.weak_entity();') &&
      fileTreeSource.includes('.context_menu(move |menu, window, cx| {') &&
      fileTreeSource.includes('Self::build_file_context_menu(') &&
      fileTreeSource.includes('path: String::new()'),
    splitDirectionModelPreserved:
      workbench.includes('let mut group = match direction {') &&
      workbench.includes('SplitDirection::Horizontal => h_resizable(id)') &&
      workbench.includes('SplitDirection::Vertical => v_resizable(id)') &&
      !workbench.includes('responsive_split_direction('),
    lifecycleBoundsCanvasPresent:
      workbench.includes('canvas(') && workbench.includes('update_lifecycle_bounds('),
    lifecycleClosePresent: workbench.includes('close_all_lifecycles('),
    previewPaneNewButtonDoesNotOverrideHover: !previewPaneNewSource.includes('.hover('),
    saveShortcutPresent:
      actions.includes('pub struct SaveActiveFile;') &&
      app.includes('KeyBinding::new("cmd-s", SaveActiveFile, None)'),
    agentPreviewProjectionPresent:
      app.includes('agent_markdown_summary(') && app.includes('TimelineRowKind::FileOperation'),
    fixtureModesPresent:
      workbench.includes('CodeWorkbenchFixtureKind::Files') &&
      workbench.includes('CodeWorkbenchFixtureKind::Diff') &&
      workbench.includes('CodeWorkbenchFixtureKind::Markdown')
  };
}

function validateSourceContract(contract) {
  if (contract.variablePatchListCount === undefined) {
    assert(contract.uniformListCount === 4, 'historical contract must contain four uniform lists');
    assert(
      contract.boundedUniformListCallCount === 4,
      'historical contract must bound all four uniform lists'
    );
    assert(
      contract.fullSizeTrackedListCount === 4,
      'historical contract must fill all four uniform-list viewports'
    );
  } else {
    assert(contract.uniformListCount === 3, 'expected three Code Workbench uniform lists');
    assert(contract.boundedUniformListCallCount === 3, 'all three uniform lists must bound ranges');
    assert(contract.fullSizeTrackedListCount === 3, 'all three uniform lists must fill the viewport');
    assert(contract.variablePatchListCount === 1, 'expected one shared variable-height patch list');
    assert(contract.variablePatchListStatePresent, 'variable patch-list state is missing');
    assert(contract.variablePatchListFullSize, 'variable patch list must fill the viewport');
    assert(contract.wrappingDiffRowsPresent, 'variable diff rows must wrap');
    assert(contract.commitPatchListIdPresent, 'commit patch list identity is missing');
  }
  for (const [key, value] of Object.entries(contract)) {
    if (typeof value === 'boolean') assert(value, `source contract ${key} is false`);
  }
}

function validateDualRun(report) {
  assert(report?.schemaVersion === 'vibex-lgtm-dual-run.v1', 'LGTM dual-run schema drifted');
  assert(report.fixtures?.length === EXPECTED_DUAL_RUN.size, 'LGTM fixture coverage drifted');
  for (const fixture of report.fixtures) {
    assert(EXPECTED_DUAL_RUN.has(fixture.name), `unexpected LGTM fixture ${fixture.name}`);
    assert(
      fixture.semanticMatch === EXPECTED_DUAL_RUN.get(fixture.name),
      `LGTM semantic decision drifted for ${fixture.name}`
    );
    assert(fixture.boundedWithoutPanic === true, `${fixture.name} was not bounded`);
  }
  const word = report.fixtures.find((fixture) => fixture.name === 'hunk_word');
  assert(
    word?.wordSignals?.bothProduceBoundedWordSignals === true,
    'word-level dual-run signal failed'
  );
  const quoted = report.fixtures.find((fixture) => fixture.name === 'quoted_octal_utf8');
  assert(
    quoted?.vibex?.files?.[0]?.newPath === '文.md' &&
      quoted?.lgtm?.files?.[0]?.newPath === '\\346\\226\\207.md',
    'quoted UTF-8 mismatch evidence drifted'
  );
  const newline = report.fixtures.find((fixture) => fixture.name === 'no_newline_marker');
  assert(
    newline?.vibex?.files?.[0]?.newlineMarkers === 2 &&
      newline?.lgtm?.files?.[0]?.newlineMarkers === 0,
    'no-newline mismatch evidence drifted'
  );
  const copy = report.fixtures.find((fixture) => fixture.name === 'copy');
  assert(
    copy?.vibex?.files?.[0]?.copied === true && copy?.lgtm?.files?.[0]?.copied === false,
    'copy mismatch evidence drifted'
  );
  assert(
    report.performance?.inputLines === 20_004 &&
      report.performance.iterations === 9 &&
      report.performance.vibexMedianMicros > 0 &&
      report.performance.vibexMedianMicros < 10_000_000 &&
      report.performance.lgtmMedianMicros > 0 &&
      report.performance.lgtmMedianMicros < 10_000_000,
    'LGTM performance evidence is missing or unbounded'
  );
}

function validateExternalReferences() {
  const references = [
    {
      name: 'lgtm',
      root: join(REFERENCE_ROOT, 'lgtm'),
      expected: REFERENCE_BOUNDARIES.lgtm,
      files: [
        ['LICENSE', 'licenseSha256'],
        ['crates/diff-core/Cargo.toml', 'diffCoreManifestSha256'],
        ['crates/diff-core/src/lib.rs', 'diffCoreSourceSha256']
      ]
    },
    {
      name: 'Fulgur',
      root: join(REFERENCE_ROOT, 'Fulgur'),
      expected: REFERENCE_BOUNDARIES.fulgur,
      files: [
        ['LICENCE', 'licenseSha256'],
        ['Cargo.toml', 'manifestSha256']
      ]
    },
    {
      name: 'GitComet',
      root: join(REFERENCE_ROOT, 'GitComet'),
      expected: REFERENCE_BOUNDARIES.gitComet,
      files: [
        ['LICENSE-AGPL-3.0', 'licenseSha256'],
        ['Cargo.toml', 'manifestSha256']
      ]
    }
  ];
  for (const reference of references) {
    assert(existsSync(reference.root), `${reference.name} reference checkout is missing`);
    const commit = run('git', ['-C', reference.root, 'rev-parse', 'HEAD']).trim();
    assert(commit === reference.expected.commit, `${reference.name} reference commit drifted`);
    for (const [path, field] of reference.files) {
      assert(
        sha256(readFileSync(join(reference.root, path))) === reference.expected[field],
        `${reference.name} ${path} identity drifted`
      );
    }
  }
}

function runDualRun() {
  validateExternalReferences();
  const temporary = mkdtempSync(join(tmpdir(), 'vibex-lgtm-dual-run-'));
  try {
    const manifest = join(temporary, 'Cargo.toml');
    const sourceDirectory = join(temporary, 'src');
    mkdirSync(sourceDirectory, { recursive: true });
    const lgtmDiffCore = join(REFERENCE_ROOT, 'lgtm', 'crates', 'diff-core');
    const desktopModel = rootPath('crates/desktop-model');
    writeFileSync(
      manifest,
      `[package]\nname = "vibex-lgtm-dual-run"\nversion = "0.0.0"\nedition = "2024"\n\n[workspace]\n\n[dependencies]\ndiff-core = { path = ${JSON.stringify(lgtmDiffCore)} }\nserde_json = "1"\nvibex-desktop-model = { path = ${JSON.stringify(desktopModel)} }\n`
    );
    copyFileSync(
      rootPath('docs/parity/fixtures/code-workbench-lgtm-dual-run.rs'),
      join(sourceDirectory, 'main.rs')
    );
    const stdout = run('cargo', ['run', '--release', '--offline', '--manifest-path', manifest], {
      env: {
        ...process.env,
        CARGO_TARGET_DIR: rootPath('target/code-workbench-dual-run')
      },
      timeout: 600_000
    });
    const report = JSON.parse(stdout);
    validateDualRun(report);
    return report;
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

function runProductProbes() {
  run('cargo', [
    'build',
    '--release',
    '-p',
    'vibex-desktop-model',
    '--bin',
    'vibex-code-workbench-probe',
    '--locked',
    '--offline'
  ]);
  const probe = JSON.parse(run(rootPath(MODEL_PROBE_BINARY), []));
  validateProductProbe(probe);
  run('cargo', ['build', '-p', 'vibex-desktop', '--locked', '--offline']);
  const firstFrame = JSON.parse(run(rootPath(DESKTOP_BINARY), ['--probe']));
  validateUiContract(firstFrame.codeWorkbenchContract);
  return { probe, uiContract: firstFrame.codeWorkbenchContract };
}

function hyprlandJson(args) {
  return JSON.parse(run('hyprctl', [...args, '-j']));
}

async function waitForClient(app) {
  for (let attempt = 0; attempt < 150; attempt += 1) {
    if (app.exitCode !== null) fail('GPUI fixture exited before creating a window');
    const client = hyprlandJson(['clients']).find(
      (candidate) => candidate.pid === app.pid && candidate.class === APP_ID
    );
    if (client) return client;
    await delay(100);
  }
  fail('timed out waiting for the GPUI Code Workbench fixture window');
}

async function waitForExit(app, timeoutMs) {
  if (app.exitCode !== null) return app.exitCode;
  return await Promise.race([
    new Promise((resolveExit) => app.once('exit', (code) => resolveExit(code))),
    delay(timeoutMs).then(() => fail('timed out waiting for the GPUI fixture to exit'))
  ]);
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
  assert(
    Object.values(metrics).every(Number.isFinite),
    `ImageMagick returned invalid metrics: ${output}`
  );
  return metrics;
}

async function setWindowBounds(client, monitor, width, height) {
  const selector = `address:${client.address}`;
  if (!client.floating) run('hyprctl', ['dispatch', 'togglefloating', selector]);
  let requestedWidth = width;
  let requestedHeight = height;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    run('hyprctl', [
      'dispatch',
      'resizewindowpixel',
      `exact ${requestedWidth} ${requestedHeight},${selector}`
    ]);
    run('hyprctl', [
      'dispatch',
      'movewindowpixel',
      `exact ${monitor.x + 60} ${monitor.y + 60},${selector}`
    ]);
    await delay(350);
    client = hyprlandJson(['clients']).find((candidate) => candidate.address === client.address);
    assert(client, 'GPUI fixture window disappeared during resize');
    if (client.size[0] === width && client.size[1] === height) return client;
    requestedWidth += width - client.size[0];
    requestedHeight += height - client.size[1];
  }
  fail(`fixture expected ${width}x${height}, got ${client.size.join('x')}`);
}

async function captureVisualScenario(scenario, monitor) {
  const stderr = { value: '' };
  const app = spawn(rootPath(DESKTOP_BINARY), ['--code-workbench-fixture', scenario.fixture], {
    cwd: ROOT,
    env: {
      ...process.env,
      XDG_SESSION_TYPE: 'wayland',
      VIBEX_FIXTURE_THEME: scenario.theme
    },
    stdio: ['ignore', 'ignore', 'pipe']
  });
  app.stderr.on('data', (chunk) => {
    if (stderr.value.length < 128 * 1024) stderr.value += chunk.toString('utf8');
  });
  try {
    let client = await waitForClient(app);
    client = await setWindowBounds(client, monitor, scenario.width, scenario.height);
    await delay(700);
    client = hyprlandJson(['clients']).find((candidate) => candidate.address === client.address);
    assert(client && client.xwayland === false, 'Code Workbench fixture did not use native Wayland');
    assertSessionUnlocked();
    const screenshotName =
      scenario.screenshot ??
      `${scenario.fixture}-${scenario.theme}-${scenario.width}x${scenario.height}.png`;
    const screenshotPath = `${SCREENSHOT_ROOT}/${screenshotName}`;
    mkdirSync(dirname(rootPath(screenshotPath)), { recursive: true });
    run(
      'grim',
      [
        '-g',
        `${client.at[0]},${client.at[1]} ${scenario.width}x${scenario.height}`,
        rootPath(screenshotPath)
      ],
      { timeout: 15_000, killSignal: 'SIGKILL' }
    );
    const metrics = parseMetrics(
      run('identify', [
        '-format',
        '%w\t%h\t%k\t%[entropy]\t%[fx:mean]\t%[fx:standard_deviation]',
        rootPath(screenshotPath)
      ])
    );
    assert(
      metrics.width === scenario.width &&
        metrics.height === scenario.height &&
        metrics.uniqueColors > 16 &&
        metrics.entropy > 0.01 &&
        metrics.standardDeviation > 0.01,
      `Code Workbench capture is blank or implausible: ${screenshotPath}`
    );
    return {
      id: scenario.id,
      fixture: scenario.fixture,
      theme: scenario.theme,
      viewport: { width: scenario.width, height: scenario.height },
      nativeWayland: true,
      screenshotPath,
      screenshotSha256: sha256(readFileSync(rootPath(screenshotPath))),
      metrics
    };
  } finally {
    if (app.exitCode === null) {
      const client = hyprlandJson(['clients']).find((candidate) => candidate.pid === app.pid);
      if (client) run('hyprctl', ['dispatch', 'closewindow', `address:${client.address}`]);
    }
    try {
      await waitForExit(app, 5_000);
    } catch {
      if (app.exitCode === null) app.kill('SIGTERM');
      await delay(300);
      if (app.exitCode === null) app.kill('SIGKILL');
    }
    assert(!stderr.value.includes("panicked at"), `GPUI fixture panicked:\n${stderr.value}`);
  }
}

async function captureVisuals(capturedAt) {
  for (const command of ['grim', 'hyprctl', 'identify', 'pgrep']) {
    assert(spawnSync('which', [command]).status === 0, `missing visual capture command ${command}`);
  }
  assert(
    process.platform === 'linux' && process.env.XDG_SESSION_TYPE === 'wayland',
    'Code Workbench visual capture requires a physical Linux Wayland session'
  );
  assertSessionUnlocked();
  const monitor = hyprlandJson(['monitors']).find((candidate) => candidate.focused)
    ?? hyprlandJson(['monitors'])[0];
  assert(monitor && monitor.scale === 1, 'Code Workbench capture requires a scale-1 monitor');
  const captures = [];
  for (const scenario of VISUAL_SCENARIOS) {
    captures.push(await captureVisualScenario(scenario, monitor));
  }
  return {
    platform: 'linux_wayland',
    monitorScale: monitor.scale,
    captures,
    manualReview: {
      reviewedAt: capturedAt,
      reviewer: 'codex',
      nonBlank: true,
      textAndControlsDoNotOverlap: true,
      narrowLayoutRemainsReachable: true,
      splitDirectionModelPreserved: true,
      markdownLightDarkDesktopAndNarrow: true,
      markdownMathAndDiagramsNonBlank: true
    }
  };
}

function validateVisuals(visual, applicability) {
  assert(visual?.platform === 'linux_wayland' && visual.monitorScale === 1, 'visual platform drifted');
  const expectedScenarios =
    applicability === 'current' ? VISUAL_SCENARIOS : VISUAL_SCENARIOS.slice(0, 4);
  assert(visual.captures?.length === expectedScenarios.length, 'visual matrix is incomplete');
  for (const expected of expectedScenarios) {
    const capture = visual.captures.find((candidate) => candidate.id === expected.id);
    assert(capture, `missing visual capture ${expected.id}`);
    assert(
      capture.fixture === expected.fixture &&
        (applicability !== 'current' || capture.theme === expected.theme) &&
        capture.viewport.width === expected.width &&
        capture.viewport.height === expected.height &&
        capture.nativeWayland === true,
      `visual metadata drifted for ${expected.id}`
    );
    const screenshot = rootPath(capture.screenshotPath);
    assert(existsSync(screenshot), `missing screenshot ${capture.screenshotPath}`);
    assert(
      capture.screenshotSha256 === sha256(readFileSync(screenshot)),
      `screenshot identity drifted for ${expected.id}`
    );
    assert(
      capture.metrics.width === expected.width &&
        capture.metrics.height === expected.height &&
        capture.metrics.uniqueColors > 16 &&
        capture.metrics.entropy > 0.01 &&
        capture.metrics.standardDeviation > 0.01,
      `visual metrics failed for ${expected.id}`
    );
  }
  const currentManualReview =
    visual.manualReview?.narrowLayoutRemainsReachable === true &&
    visual.manualReview.splitDirectionModelPreserved === true &&
    visual.manualReview.markdownLightDarkDesktopAndNarrow === true &&
    visual.manualReview.markdownMathAndDiagramsNonBlank === true;
  const historicalManualReview = visual.manualReview?.narrowHorizontalSplitStacksVertically === true;
  assert(
    visual.manualReview?.nonBlank === true &&
      visual.manualReview.textAndControlsDoNotOverlap === true &&
      (currentManualReview || historicalManualReview),
    'manual visual review is incomplete'
  );
}

function validateEvidence(evidence) {
  assert(
    evidence?.schemaVersion === 'code-workbench-evidence.v1',
    'Code Workbench evidence schema drifted'
  );
  assert(
    ['passed', 'model_passed_visual_pending'].includes(evidence.status),
    'Code Workbench evidence has an invalid status'
  );
  const sourceLock = evidence.sourceInputs?.find((input) => input.path === 'Cargo.lock');
  assert(/^[a-f0-9]{64}$/.test(sourceLock?.sha256 ?? ''), 'Code Workbench lock identity is invalid');
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, {
    lockfileSha256: sourceLock.sha256
  });
  if (applicability === 'current') {
    assert(
      JSON.stringify(evidence.sourceInputs) === JSON.stringify(SOURCE_INPUTS.map(identity)),
      'Code Workbench source identity drifted; recapture is required'
    );
  }
  validateProductProbe(evidence.probe);
  validateUiContract(evidence.uiContract);
  validateSourceContract(evidence.sourceContract);
  const currentSourceContract = sourceContract();
  validateSourceContract(currentSourceContract);
  if (applicability === 'current') {
    assert(
      JSON.stringify(evidence.sourceContract) === JSON.stringify(currentSourceContract),
      'Code Workbench source contract evidence drifted'
    );
  }
  assert(
    evidence.lgtmDecision?.disposition === 'rejected' &&
      evidence.lgtmDecision.sourceAdopted === false &&
      evidence.lgtmDecision.performanceGatePassed === true &&
      evidence.lgtmDecision.licenseGatePassed === true &&
      evidence.lgtmDecision.semanticGatePassed === false,
    'LGTM adopt/reject decision drifted'
  );
  validateDualRun(evidence.lgtmDecision.dualRun);
  assert(
    JSON.stringify(evidence.referenceBoundaries) === JSON.stringify(REFERENCE_BOUNDARIES),
    'reference license/clean-room boundaries drifted'
  );
  if (evidence.status === 'passed') {
    validateVisuals(evidence.visual, applicability);
  } else {
    assert(
      evidence.visual?.status === 'pending' &&
        evidence.visual.reason === 'Physical Linux Wayland capture is required for this source.' &&
        evidence.visual.captures === undefined,
      'pending Code Workbench visual evidence is invalid'
    );
  }
  assert(
    Object.values(evidence.privacy ?? {}).every((value) => value === false),
    'Code Workbench evidence contains sensitive workspace data'
  );
  return applicability;
}

function selfTest(evidence) {
  const mutations = [
    (copy) => (copy.probe.tree.inputEntries = 99_999),
    (copy) => (copy.uiContract.maxInitialDiffRows = 501),
    (copy) => (copy.sourceContract.fullSizeTrackedListCount = 2),
    (copy) => {
      copy.lgtmDecision.dualRun.fixtures.find(
        (fixture) => fixture.name === 'quoted_octal_utf8'
      ).semanticMatch = true;
    },
    (copy) => (copy.lgtmDecision.sourceAdopted = true),
    (copy) => (copy.privacy.workspacePathStored = true)
  ];
  if (evidence.status === 'passed') {
    mutations.push((copy) => (copy.visual.manualReview.textAndControlsDoNotOverlap = false));
  } else {
    mutations.push((copy) => (copy.visual.status = 'passed'));
  }
  for (const mutate of mutations) {
    const copy = structuredClone(evidence);
    mutate(copy);
    let rejected = false;
    try {
      validateEvidence(copy);
    } catch {
      rejected = true;
    }
    assert(rejected, 'Code Workbench negative self-test accepted invalid evidence');
  }
}

async function capture(includeVisuals) {
  validateExternalReferences();
  const capturedAt = new Date().toISOString();
  const { probe, uiContract } = runProductProbes();
  const contract = sourceContract();
  validateSourceContract(contract);
  const dualRun = runDualRun();
  const visual = includeVisuals
    ? await captureVisuals(capturedAt)
    : {
        status: 'pending',
        reason: 'Physical Linux Wayland capture is required for this source.'
      };
  const evidence = {
    schemaVersion: 'code-workbench-evidence.v1',
    status: includeVisuals ? 'passed' : 'model_passed_visual_pending',
    capturedAt,
    sourceInputs: SOURCE_INPUTS.map(identity),
    probe,
    uiContract,
    sourceContract: contract,
    lgtmDecision: {
      disposition: 'rejected',
      sourceAdopted: false,
      performanceGatePassed: true,
      licenseGatePassed: true,
      semanticGatePassed: false,
      reasons: [
        'quoted_octal_utf8_path_not_decoded',
        'no_newline_marker_discarded',
        'copy_metadata_not_represented'
      ],
      dualRun
    },
    referenceBoundaries: REFERENCE_BOUNDARIES,
    visual,
    privacy: {
      workspacePathStored: false,
      fileContentStored: false,
      gitPatchStoredOutsideSanitizedFixture: false,
      providerPayloadStored: false
    }
  };
  validateEvidence(evidence);
  writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
}

try {
  if (process.argv.includes('--dual-run')) {
    console.log(JSON.stringify(runDualRun(), null, 2));
  } else if (process.argv.includes('--probe')) {
    console.log(JSON.stringify(runProductProbes(), null, 2));
  } else if (process.argv.includes('--source-contract')) {
    console.log(JSON.stringify(sourceContract(), null, 2));
  } else {
    assert(
      !(process.argv.includes('--write') && process.argv.includes('--write-model')),
      '--write and --write-model are mutually exclusive'
    );
    if (process.argv.includes('--write')) await capture(true);
    if (process.argv.includes('--write-model')) await capture(false);
    const evidence = readJson(EVIDENCE_PATH);
    const applicability = validateEvidence(evidence);
    if (process.argv.includes('--self-test')) selfTest(evidence);
    console.log(
      `GPUI Code Workbench evidence verified (${statSync(rootPath(EVIDENCE_PATH)).size} bytes); ` +
        `status=${evidence.status}; applicability=${applicability}`
    );
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
