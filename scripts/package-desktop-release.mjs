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
const CHANNELS = new Set(["preview", "rc", "stable"]);

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

function writePlatformConfig(platform, channel) {
  const channelConfig = read(`apps/desktop/Packager.${channel}.toml`);
  let config = read("apps/desktop/Packager.toml");
  for (const field of ["name", "productName", "version", "identifier", "description"]) {
    config = withField(config, field, fieldValue(channelConfig, field));
  }
  if (platform === "macos") {
    config = config.replace(/\n\[nsis\][\s\S]*$/, "\n");
  }
  const output = join(
    ROOT,
    "apps",
    "desktop",
    `.Packager.release-${process.pid}-${channel}-${platform}.toml`
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

function architecture(platform) {
  if (platform === "macos") return process.arch === "arm64" ? "aarch64" : "x86_64";
  return "x86_64";
}

function normalizedArtifacts(platform, version, packageDirectory) {
  const files = filesUnder(packageDirectory);
  if (platform === "linux") {
    const deb = exactlyOne(files.filter((path) => path.endsWith(".deb")), "Linux deb package");
    const appImage = exactlyOne(
      files.filter((path) => path.endsWith(".AppImage")),
      "Linux AppImage package"
    );
    return [
      { source: deb, name: `vibex-${version}-linux-x86_64-deb.deb`, package: "deb", os: "linux", arch: "x86_64" },
      {
        source: appImage,
        name: `vibex-${version}-linux-x86_64-appimage.AppImage`,
        package: "appimage",
        os: "linux",
        arch: "x86_64"
      }
    ];
  }
  const extension = platform === "macos" ? ".dmg" : ".exe";
  const source = exactlyOne(
    files.filter((path) => path.toLowerCase().endsWith(extension)),
    `${platform} package`
  );
  const arch = architecture(platform);
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
  const configuredVersion = fieldValue(read(`apps/desktop/Packager.${channel}.toml`), "version");
  if (configuredVersion !== version) {
    fail(`--version ${version} does not match Packager.${channel}.toml ${configuredVersion}`);
  }
  const packageDirectory = join(ROOT, "target", "release-packages", `${channel}-${platform}`);
  const artifactDirectory = join(ROOT, "target", "release-artifacts");
  rmSync(packageDirectory, { recursive: true, force: true });
  rmSync(artifactDirectory, { recursive: true, force: true });
  mkdirSync(packageDirectory, { recursive: true });
  mkdirSync(artifactDirectory, { recursive: true });

  if (platform === "linux") {
    run(process.execPath, ["scripts/prepare-pdfium-runtime.mjs"]);
  }

  const buildEnvironment = { ...process.env, VIBEX_CHANNEL: channel, NO_STRIP: "1" };
  if (process.env.VIBEX_UPDATE_SIGNING_ENABLED === "true" && !process.env.VIBEX_UPDATE_PUBLIC_KEY?.trim()) {
    fail("VIBEX_UPDATE_PUBLIC_KEY is required when updater signing is enabled");
  }
  if (process.env.VIBEX_UPDATE_SIGNING_ENABLED !== "true") {
    delete buildEnvironment.VIBEX_UPDATE_PUBLIC_KEY;
  }
  run(process.execPath, ["scripts/build-channel.mjs", channel], { env: buildEnvironment });

  const generatedConfig = platform === "linux" ? null : writePlatformConfig(platform, channel);
  try {
    const config = generatedConfig ?? join(ROOT, "apps", "desktop", `Packager.${channel}.toml`);
    const formats = { linux: "deb,appimage", macos: "dmg", windows: "nsis" }[platform];
    run(
      "cargo",
      ["packager", "--config", config, "--formats", formats, "--out-dir", packageDirectory],
      { env: buildEnvironment }
    );
  } finally {
    if (generatedConfig) rmSync(generatedConfig, { force: true });
  }

  const artifacts = normalizedArtifacts(platform, version, packageDirectory).map((artifact) => {
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
    `${JSON.stringify({ schemaVersion: "vibex-release-assets.v1", platform, channel, version, artifacts }, null, 2)}\n`
  );
  console.log(`Desktop ${platform} artifacts written to ${repositoryPath(artifactDirectory)}`);
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
