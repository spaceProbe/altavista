// CLI harness for tests/test_web_js_control_bytes.py: `node web/js/control_bytes_check.mjs [root]`.
//
// Refuses raw control bytes in the viewer's source tree (docs/open-questions.md question 235,
// round-7 decision 11 / defect 2). A raw NUL or \x01 inside a string literal is invisible in
// every editor and diff, makes `grep` report "Binary file matches", and gets mangled by tools
// that treat NUL as a terminator -- web/js/layers/layer.js once joined layer id and local key on
// a raw NUL byte for that reason. The fix in source is always the escape (`\u0000`, `\u0001`)
// inside the same literal: the runtime string is unchanged, only the file is now plain text.
//
// Refused bytes: 0x00-0x08, 0x0B, 0x0C, 0x0E-0x1F and 0x7F. Allowed: TAB (0x09), LF (0x0A) and
// CR (0x0D). Every hit is printed as `file:line:col: 0xNN` (line and column are 1-based; the
// column counts BYTES from the last LF, so it matches `grep -b`-style tooling, and a CR counts
// as a column), then the process exits 1. On success it prints the number of files scanned and
// exits 0.
//
// Which files are read: the extension decides, never content sniffing (a sniffer that skips a
// file because it "looks binary" would silently skip exactly the file this check exists to
// catch -- one containing a NUL).
//   TEXT (scanned): .js .mjs .json .gltf .html .css, plus .md and .py, which also live under
//     web/js (VIEWER-style notes and small helper scripts) and are plain text too.
//   BINARY (skipped by name, never read): .png .jpg .jpeg .gif .webp .ico .glb .bin .ktx2 .b3dm
//     .woff .woff2 .ttf .otf .wasm .gz .zip .pdf. (.gltf may reference a .bin buffer; the .bin is
//     binary by design.)
//   ANYTHING ELSE fails the check as "unclassified": a new file kind must be put in one of the
//   two lists above on purpose rather than skipped or scanned by accident. `.DS_Store` is the
//   one named exception (macOS litter, not source).
//
// The root defaults to the directory this script lives in (resolved from import.meta.url, not
// cwd) so `node` can be run from anywhere; an explicit first argument overrides it so the test
// can point the check at a scratch tree.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, extname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const TEXT_EXTENSIONS = new Set(['.js', '.mjs', '.json', '.gltf', '.html', '.css', '.md', '.py']);
const BINARY_EXTENSIONS = new Set([
  '.png', '.jpg', '.jpeg', '.gif', '.webp', '.ico', '.glb', '.bin', '.ktx2', '.b3dm',
  '.woff', '.woff2', '.ttf', '.otf', '.wasm', '.gz', '.zip', '.pdf',
]);
const IGNORED_NAMES = new Set(['.DS_Store']);

/** True for the refused bytes: 0x00-0x08, 0x0B, 0x0C, 0x0E-0x1F, 0x7F. */
function isForbiddenByte(b) {
  return (b <= 0x08) || b === 0x0b || b === 0x0c || (b >= 0x0e && b <= 0x1f) || b === 0x7f;
}

/** Every forbidden byte of `buf` as `{line, col, byte}` (1-based line/col, col in bytes). */
function findControlBytes(buf) {
  const hits = [];
  let line = 1;
  let col = 1;
  for (let i = 0; i < buf.length; i += 1) {
    const b = buf[i];
    if (isForbiddenByte(b)) hits.push({ line, col, byte: b });
    if (b === 0x0a) { line += 1; col = 1; } else { col += 1; }
  }
  return hits;
}

function* walk(dir) {
  for (const entry of readdirSync(dir).sort()) {
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) yield* walk(full);
    else if (st.isFile()) yield full;
  }
}

/** Scan `root`; returns `{scanned, skipped, problems}` where problems are printable strings. */
function scanTree(root) {
  const problems = [];
  let scanned = 0;
  let skipped = 0;
  for (const file of walk(root)) {
    const rel = relative(root, file);
    const name = file.slice(file.lastIndexOf('/') + 1);
    if (IGNORED_NAMES.has(name)) continue;
    const ext = extname(file).toLowerCase();
    if (BINARY_EXTENSIONS.has(ext)) { skipped += 1; continue; }
    if (!TEXT_EXTENSIONS.has(ext)) {
      problems.push(`${rel}: unclassified file extension '${ext}' (add it to TEXT_EXTENSIONS or BINARY_EXTENSIONS in control_bytes_check.mjs)`);
      continue;
    }
    scanned += 1;
    for (const h of findControlBytes(readFileSync(file))) {
      problems.push(`${rel}:${h.line}:${h.col}: 0x${h.byte.toString(16).toUpperCase().padStart(2, '0')}`);
    }
  }
  return { scanned, skipped, problems };
}

function main() {
  const here = dirname(fileURLToPath(import.meta.url));
  const root = process.argv[2] ? resolve(process.argv[2]) : here;
  let result;
  try {
    result = scanTree(root);
  } catch (err) {
    console.error(`control_bytes_check: cannot scan ${root}: ${err.message}`);
    process.exit(1);
  }
  if (result.problems.length > 0) {
    for (const p of result.problems) console.log(p);
    console.log(`control_bytes_check: FAILED, ${result.problems.length} problem(s) under ${root}`);
    process.exit(1);
  }
  if (result.scanned === 0) {
    console.log(`control_bytes_check: FAILED, no text source files found under ${root}`);
    process.exit(1);
  }
  console.log(`control_bytes_check: OK, ${result.scanned} files scanned (${result.skipped} binary skipped) under ${root}`);
}

main();
