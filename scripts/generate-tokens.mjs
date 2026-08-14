import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE = "crates/vibex-ui/theme/tokens.json";
const OUTPUT = "crates/vibex-ui/src/generated_tokens.rs";
const SCHEMA_VERSION = "vibex-design-tokens.v1";
const PRODUCT_VISUAL_SOURCE = "apps/desktop";
const DEPENDENCY_SOURCE_POLICY = "fork_submodule_root_cargo_lock";
const FORBIDDEN_SOURCE_REFERENCES = [
  "apps/web",
  "apps/mobile-wasm",
  "@vibex/ui",
  "react",
  "tailwind",
  "shadcn",
  "apps/desktop/src/styles.css"
];

function fail(message) {
  throw new Error(message);
}

function object(value, label) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    fail(`${label} must be an object`);
  }
  return value;
}

function exactKeys(value, expected, label) {
  const actual = Object.keys(object(value, label)).sort();
  const wanted = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
    fail(`${label} keys drifted: ${JSON.stringify(actual)}`);
  }
}

function finiteNumber(value, label, { minimum = 0 } = {}) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < minimum) {
    fail(`${label} must be a finite number >= ${minimum}`);
  }
  return value;
}

function positiveInteger(value, label) {
  if (!Number.isInteger(value) || value <= 0) fail(`${label} must be a positive integer`);
  return value;
}

function revision(value, label) {
  if (typeof value !== "string" || !/^[a-f0-9]{40}$/.test(value)) {
    fail(`${label} must be a full Git revision`);
  }
  return value;
}

function parseAlpha(value) {
  const alpha = value.endsWith("%") ? Number(value.slice(0, -1)) / 100 : Number(value);
  if (!Number.isFinite(alpha) || alpha < 0 || alpha > 1) {
    fail(`Unsupported OKLCH alpha: ${value}`);
  }
  return alpha;
}

function convertOklch(value) {
  if (typeof value !== "string") fail(`OKLCH token must be a string: ${value}`);
  const [partsText, alphaText, ...rest] = value.split("/").map((part) => part.trim());
  if (rest.length) fail(`Unsupported OKLCH value: ${value}`);
  const parts = partsText.split(/\s+/).map(Number);
  if (parts.length !== 3 || parts.some((part) => Number.isNaN(part))) {
    fail(`Unsupported OKLCH value: ${value}`);
  }
  const [l, c, hDegrees] = parts;
  if (l < 0 || l > 1 || c < 0) fail(`Out-of-range OKLCH value: ${value}`);
  const h = (hDegrees * Math.PI) / 180;
  const a = c * Math.cos(h);
  const b = c * Math.sin(h);
  const lPrime = l + 0.3963377774 * a + 0.2158037573 * b;
  const mPrime = l - 0.1055613458 * a - 0.0638541728 * b;
  const sPrime = l - 0.0894841775 * a - 1.291485548 * b;
  const lCubed = lPrime ** 3;
  const mCubed = mPrime ** 3;
  const sCubed = sPrime ** 3;
  const linear = [
    4.0767416621 * lCubed - 3.3077115913 * mCubed + 0.2309699292 * sCubed,
    -1.2684380046 * lCubed + 2.6097574011 * mCubed - 0.3413193965 * sCubed,
    -0.0041960863 * lCubed - 0.7034186147 * mCubed + 1.707614701 * sCubed
  ];
  const srgb = linear.map((channel) => {
    const encoded =
      channel <= 0.0031308 ? 12.92 * channel : 1.055 * Math.pow(channel, 1 / 2.4) - 0.055;
    return Math.round(Math.min(1, Math.max(0, encoded)) * 255);
  });
  const hex = srgb.map((channel) => channel.toString(16).padStart(2, "0")).join("");
  return {
    hex: `#${hex}`,
    rgb: `0x${hex}`,
    alpha: alphaText ? parseAlpha(alphaText) : 1
  };
}

function semanticTokens(value, label) {
  const colors = object(value, label);
  const tokens = Object.entries(colors).map(([name, oklch]) => {
    if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(name)) fail(`${label}.${name} has an invalid name`);
    return { name, oklch, ...convertOklch(oklch) };
  });
  if (tokens.length < 40) fail(`${label} contains too few semantic colors`);
  return tokens;
}

function validateHexColor(value, label) {
  if (typeof value !== "string" || !/^#[a-f0-9]{6}(?:[a-f0-9]{2})?$/i.test(value)) {
    fail(`${label} must be a six- or eight-digit hex color`);
  }
}

function validateSyntaxHighlight(value, label) {
  const highlight = object(value, label);
  const requiredEditorColors = [
    "editor.foreground",
    "editor.background",
    "editor.active_line.background",
    "editor.line_number",
    "editor.active_line_number",
    "editor.invisible"
  ];
  for (const key of requiredEditorColors) {
    validateHexColor(highlight[key], `${label}.${key}`);
  }
  const syntax = object(highlight.syntax, `${label}.syntax`);
  if (Object.keys(syntax).length < 20) fail(`${label}.syntax contains too few styles`);
  for (const [name, rawStyle] of Object.entries(syntax)) {
    if (!/^[a-z_]+(?:\.[a-z_]+)*$/.test(name)) fail(`${label}.syntax.${name} has an invalid name`);
    const style = object(rawStyle, `${label}.syntax.${name}`);
    const keys = Object.keys(style);
    if (!keys.length || keys.some((key) => !["color", "font_style", "font_weight"].includes(key))) {
      fail(`${label}.syntax.${name} has an unsupported style shape`);
    }
    if (style.color !== undefined) validateHexColor(style.color, `${label}.syntax.${name}.color`);
    if (
      style.font_style !== undefined &&
      !["normal", "italic", "underline"].includes(style.font_style)
    ) {
      fail(`${label}.syntax.${name}.font_style is invalid`);
    }
    if (style.font_weight !== undefined) {
      const weight = positiveInteger(style.font_weight, `${label}.syntax.${name}.font_weight`);
      if (weight < 100 || weight > 900 || weight % 100 !== 0) {
        fail(`${label}.syntax.${name}.font_weight is invalid`);
      }
    }
  }
  for (const [name, color] of Object.entries(highlight)) {
    if (name !== "syntax") validateHexColor(color, `${label}.${name}`);
  }
  return highlight;
}

function parseSource(raw) {
  let source;
  try {
    source = JSON.parse(raw);
  } catch (error) {
    fail(`${SOURCE} is not valid JSON: ${error.message}`);
  }
  exactKeys(
    source,
    [
      "schemaVersion",
      "productVisualSource",
      "frozenAt",
      "dependencySource",
      "typography",
      "radiiPx",
      "spacingPx",
      "bordersPx",
      "shadows",
      "themes"
    ],
    "token source"
  );
  if (source.schemaVersion !== SCHEMA_VERSION) fail(`token schema must be ${SCHEMA_VERSION}`);
  if (source.productVisualSource !== PRODUCT_VISUAL_SOURCE) {
    fail(`product visual source must be ${PRODUCT_VISUAL_SOURCE}`);
  }
  if (typeof source.frozenAt !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(source.frozenAt)) {
    fail("token frozenAt must be an ISO date");
  }
  for (const reference of FORBIDDEN_SOURCE_REFERENCES) {
    if (raw.toLowerCase().includes(reference.toLowerCase())) {
      fail(`${SOURCE} references frozen UI input ${reference}`);
    }
  }

  exactKeys(
    source.dependencySource,
    ["policy", "gpuiRevision", "gpuiComponentRevision", "gpuiComponentTheme"],
    "dependencySource"
  );
  if (source.dependencySource.policy !== DEPENDENCY_SOURCE_POLICY) {
    fail(`dependency source policy must be ${DEPENDENCY_SOURCE_POLICY}`);
  }
  revision(source.dependencySource.gpuiRevision, "dependencySource.gpuiRevision");
  revision(source.dependencySource.gpuiComponentRevision, "dependencySource.gpuiComponentRevision");
  if (source.dependencySource.gpuiComponentTheme !== "Default") {
    fail("dependencySource.gpuiComponentTheme must preserve the Default theme");
  }

  exactKeys(source.typography, ["interface", "code"], "typography");
  exactKeys(source.typography.interface, ["family", "sizePx", "weight"], "typography.interface");
  if (typeof source.typography.interface.family !== "string" || !source.typography.interface.family) {
    fail("typography.interface.family must be non-empty");
  }
  finiteNumber(source.typography.interface.sizePx, "typography.interface.sizePx", { minimum: 1 });
  positiveInteger(source.typography.interface.weight, "typography.interface.weight");
  exactKeys(
    source.typography.code,
    ["familyPolicy", "fallbackStack", "sizePx", "weight"],
    "typography.code"
  );
  if (source.typography.code.familyPolicy !== "platform_monospace") {
    fail("typography.code.familyPolicy must be platform_monospace");
  }
  if (typeof source.typography.code.fallbackStack !== "string" || !source.typography.code.fallbackStack) {
    fail("typography.code.fallbackStack must be non-empty");
  }
  finiteNumber(source.typography.code.sizePx, "typography.code.sizePx", { minimum: 1 });
  positiveInteger(source.typography.code.weight, "typography.code.weight");

  exactKeys(source.radiiPx, ["control", "large"], "radiiPx");
  finiteNumber(source.radiiPx.control, "radiiPx.control");
  finiteNumber(source.radiiPx.large, "radiiPx.large");
  if (source.radiiPx.large < source.radiiPx.control) fail("radiiPx.large must be >= radiiPx.control");
  if (!Array.isArray(source.spacingPx) || source.spacingPx.length < 6) {
    fail("spacingPx must contain a useful scale");
  }
  let previousSpacing = -1;
  for (const [index, spacing] of source.spacingPx.entries()) {
    finiteNumber(spacing, `spacingPx[${index}]`);
    if (spacing <= previousSpacing) fail("spacingPx must be strictly increasing");
    previousSpacing = spacing;
  }
  exactKeys(source.bordersPx, ["default", "focus"], "bordersPx");
  finiteNumber(source.bordersPx.default, "bordersPx.default");
  finiteNumber(source.bordersPx.focus, "bordersPx.focus");
  if (source.bordersPx.focus < source.bordersPx.default) {
    fail("bordersPx.focus must be >= bordersPx.default");
  }
  exactKeys(source.shadows, ["enabled"], "shadows");
  if (typeof source.shadows.enabled !== "boolean") fail("shadows.enabled must be boolean");

  exactKeys(source.themes, ["light", "dark"], "themes");
  const themes = {};
  for (const mode of ["light", "dark"]) {
    exactKeys(source.themes[mode], ["semanticColors", "syntaxHighlight"], `themes.${mode}`);
    themes[mode] = {
      semanticColors: semanticTokens(source.themes[mode].semanticColors, `themes.${mode}.semanticColors`),
      syntaxHighlight: validateSyntaxHighlight(
        source.themes[mode].syntaxHighlight,
        `themes.${mode}.syntaxHighlight`
      )
    };
  }
  const lightNames = themes.light.semanticColors.map((token) => token.name);
  const darkNames = themes.dark.semanticColors.map((token) => token.name);
  if (JSON.stringify(lightNames) !== JSON.stringify(darkNames)) {
    fail("light and dark semantic color names or order differ");
  }
  return { source, themes };
}

function rustFloat(value) {
  return Number.isInteger(value) ? `${value}.0` : String(value);
}

function rustString(value) {
  return JSON.stringify(value);
}

function rustRawString(value) {
  if (value.includes('"###')) fail("highlight JSON cannot be represented by the selected raw string");
  return `r###"${value}"###`;
}

function tokenRows(tokens) {
  return tokens
    .map(
      (token) =>
        `    GpuiColorToken { name: ${rustString(token.name)}, oklch: ${rustString(token.oklch)}, ` +
        `hex: ${rustString(token.hex)}, rgb: ${token.rgb}, alpha: ${rustFloat(token.alpha)} },`
    )
    .join("\n");
}

function generate(raw) {
  const { source, themes } = parseSource(raw);
  const hash = createHash("sha256").update(raw).digest("hex");
  const code = source.typography.code;
  const lines = [
    `// Generated by scripts/generate-tokens.mjs from ${SOURCE}.`,
    "// Do not edit by hand.",
    "",
    "#[derive(Debug, Clone, Copy, PartialEq)]",
    "pub struct GpuiColorToken {",
    "    pub name: &'static str,",
    "    pub oklch: &'static str,",
    "    pub hex: &'static str,",
    "    pub rgb: u32,",
    "    pub alpha: f32,",
    "}",
    "",
    "#[derive(Debug, Clone, Copy, PartialEq)]",
    "pub struct GpuiTypographyToken {",
    "    pub family: &'static str,",
    "    pub size_px: f32,",
    "    pub weight: u16,",
    "}",
    "",
    "#[derive(Debug, Clone, Copy, PartialEq)]",
    "pub struct GpuiRadiusTokens {",
    "    pub control_px: f32,",
    "    pub large_px: f32,",
    "}",
    "",
    "#[derive(Debug, Clone, Copy, PartialEq)]",
    "pub struct GpuiBorderTokens {",
    "    pub default_px: f32,",
    "    pub focus_px: f32,",
    "}",
    "",
    `pub const TOKEN_SCHEMA_VERSION: &str = ${rustString(source.schemaVersion)};`,
    `pub const TOKEN_PRODUCT_VISUAL_SOURCE: &str = ${rustString(source.productVisualSource)};`,
    `pub const TOKEN_SOURCE_PATH: &str = ${rustString(SOURCE)};`,
    `pub const TOKEN_SOURCE_SHA256: &str = ${rustString(hash)};`,
    `pub const GPUI_REVISION: &str = ${rustString(source.dependencySource.gpuiRevision)};`,
    `pub const GPUI_COMPONENT_REVISION: &str = ${rustString(source.dependencySource.gpuiComponentRevision)};`,
    "",
    "pub const INTERFACE_TYPOGRAPHY: GpuiTypographyToken = GpuiTypographyToken {",
    `    family: ${rustString(source.typography.interface.family)},`,
    `    size_px: ${rustFloat(source.typography.interface.sizePx)},`,
    `    weight: ${source.typography.interface.weight},`,
    "};",
    "pub const CODE_TYPOGRAPHY: GpuiTypographyToken = GpuiTypographyToken {",
    `    family: ${rustString(code.familyPolicy)},`,
    `    size_px: ${rustFloat(code.sizePx)},`,
    `    weight: ${code.weight},`,
    "};",
    `pub const CODE_FONT_FALLBACK_STACK: &str = ${rustString(code.fallbackStack)};`,
    "pub const RADII: GpuiRadiusTokens = GpuiRadiusTokens {",
    `    control_px: ${rustFloat(source.radiiPx.control)},`,
    `    large_px: ${rustFloat(source.radiiPx.large)},`,
    "};",
    `pub const SPACING_PX: &[f32] = &[${source.spacingPx.map(rustFloat).join(", ")}];`,
    "pub const BORDERS: GpuiBorderTokens = GpuiBorderTokens {",
    `    default_px: ${rustFloat(source.bordersPx.default)},`,
    `    focus_px: ${rustFloat(source.bordersPx.focus)},`,
    "};",
    `pub const SHADOWS_ENABLED: bool = ${source.shadows.enabled};`,
    "",
    `pub const LIGHT_HIGHLIGHT_THEME_JSON: &str = ${rustRawString(JSON.stringify(themes.light.syntaxHighlight))};`,
    `pub const DARK_HIGHLIGHT_THEME_JSON: &str = ${rustRawString(JSON.stringify(themes.dark.syntaxHighlight))};`,
    "",
    "pub const LIGHT_TOKENS: &[GpuiColorToken] = &[",
    tokenRows(themes.light.semanticColors),
    "];",
    "",
    "pub const DARK_TOKENS: &[GpuiColorToken] = &[",
    tokenRows(themes.dark.semanticColors),
    "];"
  ];
  const unformatted = `${lines.join("\n")}\n`;
  const formatted = spawnSync("rustfmt", ["--edition", "2024"], {
    input: unformatted,
    encoding: "utf8"
  });
  if (formatted.status !== 0) {
    fail(`rustfmt failed while generating GPUI tokens: ${formatted.stderr.trim()}`);
  }
  return {
    content: formatted.stdout,
    hash,
    lightCount: themes.light.semanticColors.length,
    darkCount: themes.dark.semanticColors.length
  };
}

function selfTest(raw) {
  const mutations = [
    ["schema drift", (copy) => (copy.schemaVersion = "vibex-design-tokens.v0")],
    [
      "legacy source reference",
      (copy) => (copy.typography.code.fallbackStack = "apps/desktop/src/styles.css")
    ],
    ["theme name drift", (copy) => delete copy.themes.dark.semanticColors.background],
    [
      "invalid syntax weight",
      (copy) => (copy.themes.light.syntaxHighlight.syntax.title.font_weight = 650)
    ]
  ];
  for (const [label, mutate] of mutations) {
    const copy = JSON.parse(raw);
    mutate(copy);
    let rejected = false;
    try {
      parseSource(JSON.stringify(copy));
    } catch {
      rejected = true;
    }
    if (!rejected) fail(`token generator self-test accepted ${label}`);
  }
  console.log("GPUI token generator negative-case self-test passed");
}

const raw = readFileSync(resolve(ROOT, SOURCE), "utf8");
if (process.argv.includes("--self-test")) {
  parseSource(raw);
  selfTest(raw);
} else if (process.argv.includes("--write")) {
  const generated = generate(raw);
  writeFileSync(resolve(ROOT, OUTPUT), generated.content);
  console.log(`Wrote ${OUTPUT} from ${SOURCE} (${generated.hash})`);
} else {
  const generated = generate(raw);
  let actual;
  try {
    actual = readFileSync(resolve(ROOT, OUTPUT), "utf8");
  } catch {
    fail(`${OUTPUT} is missing; run node scripts/generate-tokens.mjs --write`);
  }
  if (actual !== generated.content) {
    fail(`${OUTPUT} is stale; run node scripts/generate-tokens.mjs --write`);
  }
  console.log(
    `GPUI tokens verified: ${generated.lightCount} light, ${generated.darkCount} dark, ${generated.hash}`
  );
}
