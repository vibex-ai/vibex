import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE = "crates/vibex-ui/theme/tokens.json";
const OUTPUT = "crates/vibex-ui/src/generated_tokens.rs";
const SCHEMA_VERSION = "vibex-design-tokens.v2";
const PRODUCT_VISUAL_SOURCE = "apps/desktop";
const THEME_MODES = ["light", "dark"];
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

function parseThemes(value) {
  const raw = object(value, "themes");
  const ids = Object.keys(raw);
  if (!ids.length) fail("themes must contain at least one theme");

  const themes = {};
  const counts = { light: 0, dark: 0 };
  let referenceNames = null;

  for (const id of ids) {
    if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(id)) fail(`themes.${id} is not a valid theme id`);
    const entry = object(raw[id], `themes.${id}`);
    exactKeys(entry, ["name", "mode", "semanticColors", "syntaxHighlight"], `themes.${id}`);
    if (typeof entry.name !== "string" || !entry.name.trim()) {
      fail(`themes.${id}.name must be a non-empty string`);
    }
    if (!THEME_MODES.includes(entry.mode)) {
      fail(`themes.${id}.mode must be one of ${THEME_MODES.join(", ")}`);
    }
    counts[entry.mode] += 1;

    const semanticColors = semanticTokens(entry.semanticColors, `themes.${id}.semanticColors`);
    const names = semanticColors.map((token) => token.name);
    if (referenceNames === null) {
      referenceNames = names;
    } else if (JSON.stringify(names) !== JSON.stringify(referenceNames)) {
      fail(`themes.${id} semantic color names or order differ from themes.${ids[0]}`);
    }

    themes[id] = {
      id,
      name: entry.name.trim(),
      mode: entry.mode,
      semanticColors,
      syntaxHighlight: validateSyntaxHighlight(
        entry.syntaxHighlight,
        `themes.${id}.syntaxHighlight`
      )
    };
  }

  for (const mode of THEME_MODES) {
    if (!counts[mode]) fail(`themes must contain at least one ${mode} theme`);
  }
  return { themes, ids, counts };
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
      "typography",
      "radiiPx",
      "spacingPx",
      "bordersPx",
      "shadows",
      "defaultTheme",
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

  const { themes, ids, counts } = parseThemes(source.themes);

  exactKeys(source.defaultTheme, THEME_MODES, "defaultTheme");
  for (const mode of THEME_MODES) {
    const id = source.defaultTheme[mode];
    if (typeof id !== "string" || !themes[id]) {
      fail(`defaultTheme.${mode} must name a theme in themes`);
    }
    if (themes[id].mode !== mode) {
      fail(`defaultTheme.${mode} must name a ${mode} theme`);
    }
  }

  return { source, themes, ids, counts };
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

function themeConstSuffix(id) {
  return id.toUpperCase().replace(/-/g, "_");
}

function generate(raw) {
  const { source, themes, ids, counts } = parseSource(raw);
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
    "/// Which appearance a theme variant is authored for.",
    "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]",
    "pub enum GpuiThemeMode {",
    "    Light,",
    "    Dark,",
    "}",
    "",
    "impl GpuiThemeMode {",
    "    pub const ALL: [Self; 2] = [Self::Light, Self::Dark];",
    "",
    "    pub fn is_dark(self) -> bool {",
    "        matches!(self, Self::Dark)",
    "    }",
    "}",
    "",
    "/// One completely resolved theme variant from the shared token source.",
    "#[derive(Debug, Clone, Copy, PartialEq)]",
    "pub struct GpuiThemeDefinition {",
    "    pub id: &'static str,",
    "    pub name: &'static str,",
    "    pub mode: GpuiThemeMode,",
    "    pub tokens: &'static [GpuiColorToken],",
    "    pub highlight_json: &'static str,",
    "}",
    "",
    `pub const TOKEN_SCHEMA_VERSION: &str = ${rustString(source.schemaVersion)};`,
    `pub const TOKEN_PRODUCT_VISUAL_SOURCE: &str = ${rustString(source.productVisualSource)};`,
    `pub const TOKEN_SOURCE_PATH: &str = ${rustString(SOURCE)};`,
    `pub const TOKEN_SOURCE_SHA256: &str = ${rustString(hash)};`,
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
    `pub const DEFAULT_LIGHT_THEME_ID: &str = ${rustString(source.defaultTheme.light)};`,
    `pub const DEFAULT_DARK_THEME_ID: &str = ${rustString(source.defaultTheme.dark)};`,
    ""
  ];

  for (const id of ids) {
    const theme = themes[id];
    const suffix = themeConstSuffix(id);
    lines.push(`const THEME_${suffix}_TOKENS: &[GpuiColorToken] = &[`);
    lines.push(tokenRows(theme.semanticColors));
    lines.push("];");
    lines.push(
      `const THEME_${suffix}_HIGHLIGHT_JSON: &str = ${rustRawString(
        JSON.stringify(theme.syntaxHighlight)
      )};`
    );
    lines.push("");
  }

  lines.push("/// Every theme variant from the shared token source, in authoring order.");
  lines.push("pub const THEMES: &[GpuiThemeDefinition] = &[");
  for (const id of ids) {
    const theme = themes[id];
    const suffix = themeConstSuffix(id);
    lines.push("    GpuiThemeDefinition {");
    lines.push(`        id: ${rustString(id)},`);
    lines.push(`        name: ${rustString(theme.name)},`);
    lines.push(`        mode: GpuiThemeMode::${theme.mode === "dark" ? "Dark" : "Light"},`);
    lines.push(`        tokens: THEME_${suffix}_TOKENS,`);
    lines.push(`        highlight_json: THEME_${suffix}_HIGHLIGHT_JSON,`);
    lines.push("    },");
  }
  lines.push("];");

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
    themeCount: ids.length,
    lightCount: counts.light,
    darkCount: counts.dark,
    tokenCount: themes[ids[0]].semanticColors.length
  };
}

function selfTest(raw) {
  const mutations = [
    ["schema drift", (copy) => (copy.schemaVersion = "vibex-design-tokens.v1")],
    [
      "legacy source reference",
      (copy) => (copy.typography.code.fallbackStack = "apps/desktop/src/styles.css")
    ],
    ["theme token drift", (copy) => delete copy.themes["vibex-dark"].semanticColors.background],
    [
      "invalid syntax weight",
      (copy) => (copy.themes["vibex-light"].syntaxHighlight.syntax.title.font_weight = 650)
    ],
    ["unknown default theme", (copy) => (copy.defaultTheme.dark = "missing-theme")],
    ["default theme mode mismatch", (copy) => (copy.defaultTheme.light = "vibex-dark")],
    ["missing theme name", (copy) => delete copy.themes.nord.name],
    ["invalid theme mode", (copy) => (copy.themes.nord.mode = "sepia")],
    ["token order drift", (copy) => {
      const colors = copy.themes.nord.semanticColors;
      const reordered = Object.fromEntries(Object.entries(colors).reverse());
      copy.themes.nord.semanticColors = reordered;
    }]
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
    `GPUI tokens verified: ${generated.themeCount} themes ` +
      `(${generated.lightCount} light, ${generated.darkCount} dark), ` +
      `${generated.tokenCount} tokens each, ${generated.hash}`
  );
}
