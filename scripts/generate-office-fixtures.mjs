import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUTPUT = resolve(ROOT, "docs/platform/fixtures/office-interaction.docx");
const temporary = mkdtempSync(join(tmpdir(), "vibex-office-fixture-"));
const epoch = new Date("2020-01-01T00:00:00Z");

function write(path, value) {
  const target = join(temporary, path);
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, value);
  utimesSync(target, epoch, epoch);
}

try {
  write("[Content_Types].xml", `<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>\n`);
  write("_rels/.rels", `<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>\n`);
  write("word/document.xml", `<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:r><w:t>GPUI Office physical fixture</w:t></w:r></w:p>
  <w:p><w:r><w:t>Bounded read-only DOCX structure</w:t></w:r></w:p>
  <w:p><w:r><w:t>世界 · interaction-ready</w:t></w:r></w:p>
</w:body></w:document>\n`);
  mkdirSync(dirname(OUTPUT), { recursive: true });
  rmSync(OUTPUT, { force: true });
  const result = spawnSync("zip", ["-X", "-q", "-r", OUTPUT, "[Content_Types].xml", "_rels", "word"], {
    cwd: temporary,
    encoding: "utf8"
  });
  if (result.status !== 0) throw new Error(result.stderr || "zip failed");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
