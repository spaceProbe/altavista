// tileset_manifest.js -- round 4 (question 228's own round-3-defect-5 follow-up: "the
// per-tile byte cost was a fixed 262,144-byte estimate", already fixed once by making
// it a CALLER-DECLARED number checked against real tile length -- see
// web/js/layers_stream_check.mjs's own `byteCostMismatchCount`. That is still a single
// uniform number applied to every tile of a set, never the real PER-TILE size the
// manifest itself carries -- this file closes that gap).
//
// A minimal, hand-rolled decoder for exactly the one protobuf message shape
// `./gateway_imagery_layer.js` needs: `TileSetManifest.tiles` (`repeated TileEntry`,
// `proto/altavista/v1/heavy.proto`). ADR-004's crypto rule ("system/platform crypto
// only, no bundled dependency" -- already applied in gateway_imagery_layer.js's own
// hand-written SHA-256-to-hex encoder, `hex()`) extended one step further here: no
// `protobuf.js`/`google-protobuf` dependency is added merely to read a manifest's
// `tiles` list -- this reads the wire format directly.
//
// This decoder is deliberately NOT a general protobuf reader: it decodes only
// `TileSetManifest` field 7 (`tiles`, repeated `TileEntry`) and, within each
// `TileEntry`, only fields 1/2/3/5 (`level`/`x`/`y`/`size_bytes`) -- the four this
// codebase's admission logic actually needs (`level`/`x`/`y` to address a tile the
// same way `web/js/globe_lod.js`'s own `tileKey` does, `size_bytes` as the real
// per-tile byte cost). Every OTHER field, at either message level, is skipped
// generically by wire type, never parsed -- this is forward-compatible by
// construction, not a guess: `heavy.proto`'s own module doc states an "additive-only
// rule once a round has shipped" (new field NUMBERS are only ever added, an existing
// one is never reused for a different shape), so a decoder that can skip an
// unrecognised field by its wire type alone (varint / 64-bit / length-delimited /
// 32-bit -- the four the protobuf wire format itself defines) never needs to be
// updated when `heavy.proto` gains a field this codebase does not yet care about.
//
// Values are decoded into plain JS `Number`s, not `BigInt` -- every field this
// decoder reads (`level`/`x`/`y`/`size_bytes`) is a tile-set coordinate or a tile's
// own byte length, both of which stay far below `Number.MAX_SAFE_INTEGER` (2^53) for
// any tile set this codebase's own tiler (`crates/av-jobs::tiler`) or fixtures
// produce; a `size_bytes` anywhere near 2^53 bytes would not be a real tile file in
// the first place. Documented, not silent: `readVarint` below composes a varint's
// 7-bit groups with plain floating-point arithmetic (`* 2 ** shift`), which is exact
// for every integer below 2^53.

/** Thrown when this decoder encounters a protobuf wire type it does not implement
 * (the format defines exactly four: 0 varint, 1 64-bit, 2 length-delimited, 5
 * 32-bit -- there is no fifth to add) -- this can only happen on a truly malformed
 * byte stream, never on a well-formed `TileSetManifest` however many fields it adds,
 * since every wire type the format allows is already handled (see this file's module
 * docstring). Typed and named, this codebase's binding rule for every refusal (never
 * a bare `Error` -- see gateway_imagery_layer.js's `TileHttpError`/
 * `TileEtagMismatchError` for the identical discipline). */
export class ManifestDecodeError extends Error {
  constructor(detail) {
    super(`tileset_manifest: malformed TileSetManifest bytes (${detail})`);
    this.name = 'ManifestDecodeError';
  }
}

function readVarint(bytes, pos) {
  let result = 0;
  let shift = 0;
  for (;;) {
    if (pos >= bytes.length) throw new ManifestDecodeError(`varint ran past end of buffer at byte ${pos}`);
    const b = bytes[pos];
    pos += 1;
    result += (b & 0x7f) * 2 ** shift;
    if ((b & 0x80) === 0) break;
    shift += 7;
    if (shift > 63) throw new ManifestDecodeError('varint longer than 64 bits');
  }
  return { value: result, pos };
}

/** Advances past one field's value without decoding it, by wire type alone -- see
 * this file's module docstring for why this makes every unrecognised field
 * forward-compatible rather than a decode failure. */
function skipField(bytes, pos, wireType) {
  switch (wireType) {
    case 0: return readVarint(bytes, pos).pos; // varint
    case 1: return pos + 8; // 64-bit (fixed64/double)
    case 2: { const len = readVarint(bytes, pos); return len.pos + len.value; } // length-delimited
    case 5: return pos + 4; // 32-bit (fixed32/float)
    default: throw new ManifestDecodeError(`unsupported wire type ${wireType} at byte ${pos}`);
  }
}

/** One `TileEntry` (`proto/altavista/v1/heavy.proto`) decoded from its own
 * length-delimited embedded-message bytes -- only `level`/`x`/`y`/`size_bytes` (field
 * numbers 1/2/3/5) are read; `sha256`/`uri`/`media_type`/`object_key` (4/6/7/8) are
 * skipped (this adapter does not need them: the gateway route itself already verifies
 * the tile's SHA-256 against its own `ETag`, see gateway_imagery_layer.js's module
 * docstring -- reading the manifest's copy too would be a second, redundant check of
 * the exact same hash, not a stronger one).
 * @returns {{level:number, x:number, y:number, sizeBytes:number}}
 */
function decodeTileEntry(bytes) {
  let pos = 0;
  let level = 0;
  let x = 0;
  let y = 0;
  let sizeBytes = 0;
  while (pos < bytes.length) {
    const tag = readVarint(bytes, pos);
    pos = tag.pos;
    const fieldNumber = tag.value >>> 3;
    const wireType = tag.value & 0x7;
    if (fieldNumber === 1 && wireType === 0) { const r = readVarint(bytes, pos); level = r.value; pos = r.pos; } else if (fieldNumber === 2 && wireType === 0) { const r = readVarint(bytes, pos); x = r.value; pos = r.pos; } else if (fieldNumber === 3 && wireType === 0) { const r = readVarint(bytes, pos); y = r.value; pos = r.pos; } else if (fieldNumber === 5 && wireType === 0) { const r = readVarint(bytes, pos); sizeBytes = r.value; pos = r.pos; } else {
      pos = skipField(bytes, pos, wireType);
    }
  }
  return { level, x, y, sizeBytes };
}

/** Decodes a `TileSetManifest`'s own top-level bytes (exactly what
 * `GET /api/tiles/<manifestSha256>/manifest` answers with, media type
 * `application/vnd.altavista.tileset-manifest+pb` -- `altavista/server.py`'s
 * `tiles_manifest` route, proxying `crates/av-tiles`) into `{ tiles: TileEntry[] }`.
 * Only field 7 (`tiles`, `repeated TileEntry`) is decoded; every other top-level field
 * (`kind`/`scheme`/`min_level`/`max_level`/`tile_size`/`bounds`/`source_sha256`/
 * `parameters`/`root_uri`/`job_id`/`object_key_prefix`/`root_object_key`) is skipped
 * -- see this file's module docstring for why that is safe, not merely convenient.
 * @param {ArrayBuffer|Uint8Array} buffer
 * @returns {{tiles: Array<{level:number, x:number, y:number, sizeBytes:number}>}}
 */
export function decodeTileSetManifest(buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  let pos = 0;
  const tiles = [];
  while (pos < bytes.length) {
    const tag = readVarint(bytes, pos);
    pos = tag.pos;
    const fieldNumber = tag.value >>> 3;
    const wireType = tag.value & 0x7;
    if (fieldNumber === 7 && wireType === 2) {
      const len = readVarint(bytes, pos);
      pos = len.pos;
      if (pos + len.value > bytes.length) throw new ManifestDecodeError('tiles[] entry length runs past end of buffer');
      const entryBytes = bytes.subarray(pos, pos + len.value);
      pos += len.value;
      tiles.push(decodeTileEntry(entryBytes));
    } else {
      pos = skipField(bytes, pos, wireType);
    }
  }
  return { tiles };
}

/** `(level,x,y)` -> the SAME string key `web/js/globe_lod.js`'s own `tileKey()`
 * produces (`${level}/${x}/${y}`) -- deliberately restated here, not imported, so
 * this module (which `node`-only tooling or a future non-globe consumer could reuse
 * on its own) never needs to import the globe's own LOD module just for a string
 * format; `gateway_imagery_layer.js` itself already has `tileKey` available via its
 * `ImageryLayerAdapter` base class and uses THAT for the actual lookup (see this
 * function only being used by this module's OWN tests) -- two independent producers
 * of the identical format, not one importing the other, which is exactly why the
 * format is written out literally in both places rather than composed from a shared
 * constant that could drift without either side noticing.
 */
export function manifestTileKey(entry) {
  return `${entry.level}/${entry.x}/${entry.y}`;
}
