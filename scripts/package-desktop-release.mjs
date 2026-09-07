import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PLATFORMS = new Set(["linux", "macos", "windows"]);
// Cross-packaged architectures are released as deb (Linux), dmg (macOS), and
// NSIS (Windows); cargo-packager 0.11.8 supports the deb, dmg, and nsis
// bundlers for cross targets, while AppImage (linuxdeploy) is host-arch only.
const ARCHITECTURES = new Set(["x86_64", "aarch64"]);
const CHANNELS = new Set(["preview", "rc", "stable"]);
const TARGET_TRIPLES = {
  "linux-x86_64": "x86_64-unknown-linux-gnu",
  "linux-aarch64": "aarch64-unknown-linux-gnu",
  "macos-x86_64": "x86_64-apple-darwin",
  "macos-aarch64": "aarch64-apple-darwin",
  "windows-x86_64": "x86_64-pc-windows-msvc",
  "windows-aarch64": "aarch64-pc-windows-msvc"
};
// Keep macOS input dimensions within cargo-packager's supported ICNS types.
const MACOS_ICON_INPUTS = [
  "assets/app-icons/icon-16.png",
  "assets/app-icons/icon-32.png",
  "assets/app-icons/icon-48.png",
  "assets/app-icons/icon-128.png",
  "assets/app-icons/icon-256.png"
];

function fail(message) {
  throw new Error(message);
}

function parseArguments() {
  const values = process.argv.slice(2);
  const argumentsByName = {};
  for (let index = 0; index < values.length; index += 1) {
    const flag = values[index];
    if (!flag.startsWith("--")) fail(`unexpected argument ${flag}`);
    const value = values[++index];
    if (!value || value.startsWith("--")) fail(`${flag} requires a value`);
    argumentsByName[flag.slice(2)] = value;
  }
  for (const name of ["platform", "channel", "version"]) {
    if (!argumentsByName[name]) fail(`--${name} is required`);
  }
  if (!PLATFORMS.has(argumentsByName.platform)) {
    fail(`unsupported desktop platform: ${argumentsByName.platform}`);
  }
  if (!CHANNELS.has(argumentsByName.channel)) {
    fail(`unsupported release channel: ${argumentsByName.channel}`);
  }
  if (argumentsByName.arch && !ARCHITECTURES.has(argumentsByName.arch)) {
    fail(`unsupported desktop architecture: ${argumentsByName.arch}`);
  }
  return argumentsByName;
}

function repositoryPath(value) {
  return relative(ROOT, value).split("\\").join("/");
}

function read(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function run(command, args, options = {}) {
  console.log(`> ${command} ${args.join(" ")}`);
  const result = spawnSync(command, args, {
    cwd: ROOT,
    env: options.env ?? process.env,
    stdio: "inherit",
    windowsHide: true
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} exited with status ${result.status ?? "unknown"}`);
}

function fieldValue(source, field) {
  const match = source.match(new RegExp(`^${field}\\s*=\\s*"((?:[^"\\\\]|\\\\.)*)"$`, "m"));
  if (!match) fail(`Packager configuration is missing ${field}`);
  return match[1];
}

function withField(source, field, value) {
  const line = `${field} = ${JSON.stringify(value)}`;
  const pattern = new RegExp(`^${field}\\s*=.*$`, "m");
  if (!pattern.test(source)) fail(`generic Packager configuration is missing ${field}`);
  return source.replace(pattern, line);
}

function withIcons(source, icons) {
  const pattern = /^icons\s*=\s*\[[\s\S]*?^\]\n/m;
  if (!pattern.test(source)) fail("generic Packager configuration is missing icons");
  const replacement = [
    "icons = [",
    ...icons.map((icon) => `  ${JSON.stringify(icon)},`),
    "]",
    ""
  ].join("\n");
  return source.replace(pattern, replacement);
}

function withoutResources(source) {
  const pattern = /^resources\s*=\s*\[[\s\S]*?^\]\n/m;
  if (!pattern.test(source)) fail("channel Packager configuration is missing resources");
  return source.replace(pattern, "");
}

function writePlatformConfig(platform, arch, channel, triple, cross) {
  let config;
  if (platform === "linux") {
    // The Linux ARM64 deb omits the PDFium resource payload: its distribution
    // approval covers linux-x86_64 only, mirroring the macOS/Windows packages.
    config = withoutResources(read(`apps/desktop/Packager.${channel}.toml`));
  } else {
    const channelConfig = read(`apps/desktop/Packager.${channel}.toml`);
    config = read("apps/desktop/Packager.toml");
    for (const field of ["name", "productName", "version", "identifier", "description"]) {
      config = withField(config, field, fieldValue(channelConfig, field));
    }
    if (platform === "macos") {
      config = withIcons(config, MACOS_ICON_INPUTS);
      config = config.replace(/\n\[nsis\][\s\S]*$/, "\n");
    }
  }
  if (cross) {
    config = withField(config, "binariesDir", `../../target/${triple}/release`);
  }
  const output = join(
    ROOT,
    "apps",
    "desktop",
    `.Packager.release-${process.pid}-${channel}-${platform}-${arch}.toml`
  );
  writeFileSync(output, config);
  return output;
}

function filesUnder(directory) {
  if (!existsSync(directory)) return [];
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...filesUnder(path));
    else if (entry.isFile()) files.push(path);
  }
  return files;
}

function exactlyOne(values, label) {
  if (values.length !== 1) {
    fail(`expected exactly one ${label}, found ${values.length}`);
  }
  return values[0];
}

function hostArchitecture() {
  if (process.arch === "x64") return "x86_64";
  if (process.arch === "arm64") return "aarch64";
  return fail(`unsupported host architecture: ${process.arch}`);
}

function normalizedArtifacts(platform, arch, version, packageDirectory) {
  const files = filesUnder(packageDirectory);
  if (platform === "linux") {
    const deb = exactlyOne(files.filter((path) => path.endsWith(".deb")), "Linux deb package");
    const artifacts = [
      {
        source: deb,
        name: `vibex-${version}-linux-${arch}-deb.deb`,
        package: "deb",
        os: "linux",
        arch
      }
    ];
    if (arch === "x86_64") {
      const appImage = exactlyOne(
        files.filter((path) => path.endsWith(".AppImage")),
        "Linux AppImage package"
      );
      artifacts.push({
        source: appImage,
        name: `vibex-${version}-linux-x86_64-appimage.AppImage`,
        package: "appimage",
        os: "linux",
        arch: "x86_64"
      });
    }
    return artifacts;
  }
  const extension = platform === "macos" ? ".dmg" : ".exe";
  const source = exactlyOne(
    files.filter((path) => path.toLowerCase().endsWith(extension)),
    `${platform} package`
  );
  const packageName = platform === "macos" ? "app" : "nsis";
  return [
    {
      source,
      name: `vibex-${version}-${platform}-${arch}-${packageName}.${platform === "macos" ? "dmg" : "exe"}`,
      package: packageName,
      os: platform,
      arch
    }
  ];
}

function digest(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function main() {
  const { platform, channel, version } = parseArguments();
  const arch = process.argv.includes("--arch")
    ? process.argv[process.argv.indexOf("--arch") + 1]
    : hostArchitecture();
  const triple = TARGET_TRIPLES[`${platform}-${arch}`];
  if (!triple) fail(`unsupported platform/architecture combination: ${platform}-${arch}`);
  const cross = arch !== hostArchitecture();
  const configuredVersion = fieldValue(read(`apps/desktop/Packager.${channel}.toml`), "version");
  if (configuredVersion !== version) {
    fail(`--version ${version} does not match Packager.${channel}.toml ${configuredVersion}`);
  }
  const packageDirectory = join(ROOT, "target", "release-packages", `${channel}-${platform}-${arch}`);
  const artifactDirectory = join(ROOT, "target", "release-artifacts");
  rmSync(packageDirectory, { recursive: true, force: true });
  rmSync(artifactDirectory, { recursive: true, force: true });
  mkdirSync(packageDirectory, { recursive: true });
  mkdirSync(artifactDirectory, { recursive: true });

  if (platform === "linux" && arch === "x86_64") {
    run(process.execPath, ["scripts/prepare-pdfium-runtime.mjs"]);
  }

  const buildEnvironment = { ...process.env, VIBEX_CHANNEL: channel, NO_STRIP: "1" };
  if (process.env.VIBEX_UPDATE_SIGNING_ENABLED === "true" && !process.env.VIBEX_UPDATE_PUBLIC_KEY?.trim()) {
    fail("VIBEX_UPDATE_PUBLIC_KEY is required when updater signing is enabled");
  }
  if (process.env.VIBEX_UPDATE_SIGNING_ENABLED !== "true") {
    delete buildEnvironment.VIBEX_UPDATE_PUBLIC_KEY;
  }
  run(
    process.execPath,
    cross
      ? ["scripts/build-channel.mjs", channel, "--target", triple]
      : ["scripts/build-channel.mjs", channel],
    { env: buildEnvironment }
  );

  const generatedConfig =
    platform === "linux" && arch === "x86_64"
      ? null
      : writePlatformConfig(platform, arch, channel, triple, cross);
  try {
    const config = generatedConfig ?? join(ROOT, "apps", "desktop", `Packager.${channel}.toml`);
    const formats =
      platform === "linux" && arch === "aarch64"
        ? "deb"
        : { linux: "deb,appimage", macos: "dmg", windows: "nsis" }[platform];
    run(
      "cargo",
      [
        "packager",
        "--config",
        config,
        "--formats",
        formats,
        "--out-dir",
        packageDirectory,
        ...(cross ? ["--target", triple] : [])
      ],
      { env: buildEnvironment }
    );
  } finally {
    if (generatedConfig) rmSync(generatedConfig, { force: true });
  }

  const artifacts = normalizedArtifacts(platform, arch, version, packageDirectory).map((artifact) => {
    const destination = join(artifactDirectory, artifact.name);
    const bytes = readFileSync(artifact.source);
    writeFileSync(destination, bytes);
    const sha256 = digest(destination);
    writeFileSync(`${destination}.sha256`, `${sha256}  ${artifact.name}\n`);
    return {
      ...artifact,
      source: repositoryPath(destination),
      size: statSync(destination).size,
      sha256
    };
  });
  writeFileSync(
    join(artifactDirectory, "release-assets.json"),
    `${JSON.stringify({ schemaVersion: "vibex-release-assets.v1", platform, arch, channel, version, artifacts }, null, 2)}\n`
  );
  console.log(`Desktop ${platform}-${arch} artifacts written to ${repositoryPath(artifactDirectory)}`);
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
