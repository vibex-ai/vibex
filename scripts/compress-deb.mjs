import { spawnSync } from "node:child_process";
import {
  readFileSync,
  readdirSync,
  renameSync,
  statSync,
  writeFileSync
} from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { gunzipSync } from "node:zlib";

// cargo-packager 0.11.8 always gzips the deb payload (`control.tar.gz` and
// `data.tar.gz` in a raw ar archive) and exposes no compression setting. The
// payload is dominated by the two GPUI ELFs, whose code and relocation tables
// compress far better with xz than with gzip: rebuilding both tar members
// takes the published package down by more than a third without changing a
// byte of the installed tree. Preset 9 is worth another ~10% over the Debian
// default of 6 for this payload; the release build already spends minutes
// linking, so the extra minute here is not the bottleneck.

const AR_MAGIC = "!<arch>\n";
const AR_HEADER_BYTES = 60;
const AR_NAME_BYTES = 16;
const MAX_ARCHIVE_BYTES = 1024 * 1024 * 1024;
const XZ_PRESET = "-9";

function isDeb(path) {
  return path.toLowerCase().endsWith(".deb");
}

function findDebFiles(target) {
  const stats = statSync(target, { throwIfNoEntry: false });
  if (!stats) throw new Error(`no such package: ${target}`);
  if (stats.isFile()) {
    if (!isDeb(target)) throw new Error(`not a .deb archive: ${target}`);
    return [target];
  }
  if (!stats.isDirectory()) throw new Error(`not a file or directory: ${target}`);
  const found = [];
  for (const entry of readdirSync(target, { withFileTypes: true })) {
    const path = join(target, entry.name);
    if (entry.isDirectory()) found.push(...findDebFiles(path));
    else if (entry.isFile() && isDeb(entry.name)) found.push(path);
  }
  return found.sort();
}

// A .deb is an ar archive whose members are `debian-binary`, `control.tar.*`,
// and `data.tar.*`. Only the header layout matters here; the member contents
// are recompressed and written back with their original metadata.
export function parseAr(bytes) {
  if (bytes.subarray(0, AR_MAGIC.length).toString("latin1") !== AR_MAGIC) {
    throw new Error("archive does not start with the ar magic");
  }
  const members = [];
  let offset = AR_MAGIC.length;
  while (offset < bytes.length) {
    if (offset + AR_HEADER_BYTES > bytes.length) throw new Error("truncated ar header");
    const header = bytes.subarray(offset, offset + AR_HEADER_BYTES).toString("latin1");
    const size = Number.parseInt(header.slice(48, 58).trim(), 10);
    if (!Number.isSafeInteger(size) || size < 0) throw new Error("invalid ar member size");
    const start = offset + AR_HEADER_BYTES;
    const end = start + size;
    if (end > bytes.length) throw new Error("truncated ar member");
    members.push({
      name: header.slice(0, AR_NAME_BYTES).trim().replace(/\/+$/, ""),
      mtime: header.slice(16, 28).trim(),
      uid: header.slice(28, 34).trim(),
      gid: header.slice(34, 40).trim(),
      mode: header.slice(40, 48).trim(),
      data: bytes.subarray(start, end)
    });
    // ar pads every member with a newline to an even byte offset.
    offset = end + (size % 2);
  }
  return members;
}

function arHeader(member) {
  if (member.name.length > AR_NAME_BYTES) {
    throw new Error(`ar member name is too long: ${member.name}`);
  }
  return [
    member.name.padEnd(AR_NAME_BYTES),
    member.mtime.padEnd(12),
    member.uid.padEnd(6),
    member.gid.padEnd(6),
    member.mode.padEnd(8),
    String(member.data.length).padEnd(10),
    "`\n"
  ].join("");
}

function writeAr(members) {
  const chunks = [Buffer.from(AR_MAGIC, "latin1")];
  for (const member of members) {
    chunks.push(Buffer.from(arHeader(member), "latin1"), member.data);
    if (member.data.length % 2 === 1) chunks.push(Buffer.from("\n", "latin1"));
  }
  return Buffer.concat(chunks);
}

function xzCompress(data) {
  const result = spawnSync("xz", ["--compress", "--stdout", XZ_PRESET, "--threads=0"], {
    input: data,
    maxBuffer: MAX_ARCHIVE_BYTES
  });
  if (result.error) {
    throw new Error(`xz is required to repack the deb payload: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`xz exited with status ${result.status}: ${result.stderr.toString().trim()}`);
  }
  return result.stdout;
}

function formatBytes(bytes) {
  return `${(bytes / 1_000_000).toFixed(1)} MB`;
}

/**
 * Replaces the gzipped tar members of one .deb with xz-compressed equivalents.
 * Returns `null` when the archive is already xz-compressed, so packaging steps
 * that run the script twice stay idempotent.
 */
export function compressDebPackage(path) {
  const original = readFileSync(path);
  const members = parseAr(original);
  const replacements = new Map([
    ["control.tar.gz", "control.tar.xz"],
    ["data.tar.gz", "data.tar.xz"]
  ]);
  let changed = false;
  for (const member of members) {
    const replacement = replacements.get(member.name);
    if (!replacement) continue;
    const tar = gunzipSync(member.data);
    member.data = xzCompress(tar);
    member.name = replacement;
    changed = true;
  }
  if (!changed) return null;
  const compressed = writeAr(members);
  const temporary = `${path}.tmp`;
  writeFileSync(temporary, compressed);
  renameSync(temporary, path);
  return { before: original.length, after: compressed.length };
}

function main() {
  const targets = process.argv.slice(2);
  if (targets.length === 0) {
    console.error("usage: node scripts/compress-deb.mjs <package.deb|directory>...");
    process.exitCode = 2;
    return;
  }
  try {
    const packages = targets.flatMap((target) => findDebFiles(resolve(target)));
    if (packages.length === 0) throw new Error("no .deb archives found");
    for (const path of packages) {
      const result = compressDebPackage(path);
      if (!result) {
        console.log(`${path}: already xz-compressed`);
        continue;
      }
      const change = ((result.after - result.before) / result.before) * 100;
      const sign = change > 0 ? "+" : "-";
      console.log(
        `${path}: ${formatBytes(result.before)} -> ${formatBytes(result.after)} ` +
          `(${sign}${Math.abs(change).toFixed(1)}%)`
      );
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
