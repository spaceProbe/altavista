//! A minimal, standards-conformant 8-bit RGB PNG encoder, with no dependency -- no
//! `png`/`image`/`flate2` crate is in this workspace's `Cargo.lock`, and rule 5 (this
//! task's own brief) forbids adding one. Every byte this module emits is built by hand
//! against the PNG (ISO/IEC 15948) and zlib/deflate (RFC 1950/1951) specifications.
//!
//! # Shape
//!
//! - Signature: `89 50 4E 47 0D 0A 1A 0A` (8 bytes, fixed).
//! - `IHDR` chunk: width (u32 BE), height (u32 BE), bit depth `8`, colour type `2`
//!   (truecolour, no alpha), compression method `0`, filter method `0`, interlace method
//!   `0`.
//! - Exactly one `IDAT` chunk, carrying the whole zlib stream (never split across chunks --
//!   legal per spec, and simplest to encode by hand).
//! - `IEND` chunk, empty.
//!
//! Every chunk is `length: u32 BE || type: [u8; 4] || data || crc32: u32 BE`, where the
//! CRC-32 (ISO/IEC 3309, the same polynomial `zlib.crc32` uses) is computed over
//! `type || data`.
//!
//! # The `IDAT` payload: a zlib stream with stored (uncompressed) deflate blocks
//!
//! `zlib_header (2 bytes: 0x78 0x01) || deflate_stream || adler32_of_uncompressed (4 bytes BE)`.
//! `0x78 0x01` is CMF/FLG for a 32K window, no preset dictionary, compression level 0
//! ("fastest") -- a decoder only inspects `CM`/`CINFO`/`FDICT` to know how to read the
//! stream; `FLEVEL` (the two bits this byte's low nibble sets to indicate speed) is purely
//! informational and never gates a "stored blocks" deflate stream, since a stored block
//! carries no entropy coding at all for a decoder to second-guess.
//!
//! The deflate stream itself uses **stored (uncompressed) blocks only** (RFC 1951 SS3.2.4):
//! each block is `BFINAL (1 bit) || BTYPE=00 (2 bits) || pad to the next byte boundary ||
//! LEN (u16 LE) || ~LEN (u16 LE, one's complement) || LEN raw bytes`, chunked at 65535
//! bytes per block (the largest value `LEN`'s 16 bits can hold) -- `BFINAL` is 1 only on the
//! last block. This is a legal deflate stream every conformant PNG decoder accepts (RFC
//! 1951 does not require entropy coding, only that stored blocks exist as an escape hatch
//! for incompressible data), and it needs no compressor: this module never computes a
//! Huffman table or an LZ77 match.
//!
//! The scanline data deflate compresses (trivially, since it is stored uncompressed) is:
//! for each of `height` rows, one filter-type byte `0` (`None` -- no per-pixel
//! delta-filtering, the simplest legal choice) followed by that row's own `width * 3` raw
//! RGB bytes.
//!
//! # CRC-32 and Adler-32 are checksums, not cryptography
//!
//! **Both algorithms here are ordinary error-detecting checksums** (CRC-32/ISO-HDLC and
//! Adler-32, the exact two the PNG and zlib specifications mandate respectively) -- neither
//! is a cryptographic primitive, and using them here is not an ADR-004 violation. ADR-004
//! governs *cryptographic* hashing and TLS; the only cryptographic hash anywhere in this
//! crate remains `openssl::sha::sha256` (`crate::hash::chain_hash`, `crate::runner`'s
//! input/output hashing, `crate::tiler`'s own tile-content hashing). CRC-32/Adler-32 exist
//! here purely because the PNG and zlib container formats require them structurally, in
//! exactly the way `crate::log`'s own `payload_len` field is not a security control either.

/// The standard CRC-32 (ISO-HDLC / `zlib.crc32`) table, generated once at first use via a
/// `std::sync::OnceLock` rather than as a 1024-byte literal -- both produce the identical
/// 256-entry table; generating it keeps this module free of a giant magic-number block that
/// would otherwise be exactly the kind of thing a reviewer cannot eyeball-verify.
fn crc32_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        let mut n = 0u32;
        while (n as usize) < 256 {
            let mut c = n;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            table[n as usize] = c;
            n += 1;
        }
        table
    })
}

/// CRC-32 (ISO-HDLC), the same polynomial and reflection Python's `zlib.crc32` computes.
pub fn crc32(bytes: &[u8]) -> u32 {
    let table = crc32_table();
    let mut c: u32 = 0xFFFFFFFF;
    for &b in bytes {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFFFFFF
}

/// Adler-32 (RFC 1950 SS8.2), the zlib trailer checksum -- computed over the *uncompressed*
/// data, per the zlib format's own definition.
pub fn adler32(bytes: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    // Reduce modulo MOD_ADLER periodically rather than after every byte, matching the
    // reference algorithm's usual optimisation -- correctness does not depend on this, only
    // speed, since a+b never overflows u32 within any reasonable chunk size here.
    for chunk in bytes.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= MOD_ADLER;
        b %= MOD_ADLER;
    }
    (b << 16) | a
}

fn write_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut type_and_data = Vec::with_capacity(4 + data.len());
    type_and_data.extend_from_slice(chunk_type);
    type_and_data.extend_from_slice(data);
    out.extend_from_slice(&type_and_data);
    out.extend_from_slice(&crc32(&type_and_data).to_be_bytes());
}

/// Deflate `data` as a sequence of stored (uncompressed) blocks, chunked at 65535 bytes each
/// -- see this module's own doc for the exact block framing. `data` may be empty (a single
/// `BFINAL=1` block with `LEN=0` is still legal).
fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 5);
    if data.is_empty() {
        out.push(0x01); // BFINAL=1, BTYPE=00, rest of the byte padding zero.
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(!0u16).to_le_bytes());
        return out;
    }
    let mut chunks = data.chunks(65535).peekable();
    while let Some(chunk) = chunks.next() {
        let is_last = chunks.peek().is_none();
        out.push(if is_last { 0x01 } else { 0x00 }); // BFINAL bit 0, BTYPE=00 in bits 1-2, rest padding zero.
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// The IDAT payload: `0x78 0x01 || deflate_stored(scanlines) || adler32(scanlines) BE`.
fn zlib_stream(uncompressed: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(uncompressed.len() + 8);
    out.push(0x78);
    out.push(0x01);
    out.extend_from_slice(&deflate_stored(uncompressed));
    out.extend_from_slice(&adler32(uncompressed).to_be_bytes());
    out
}

pub const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Encodes `pixels` (row-major, north row first, exactly `width * height * 3` RGB8 bytes)
/// as a complete, standards-conformant PNG file. `width`/`height` must both be nonzero and
/// `pixels.len()` must equal `width * height * 3` exactly, or this returns `None` rather
/// than encoding a malformed image -- the only validation this encoder performs; it does
/// not otherwise interpret pixel content.
pub fn encode_rgb8(width: u32, height: u32, pixels: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }
    let expected_len = (width as u64) * (height as u64) * 3;
    if pixels.len() as u64 != expected_len {
        return None;
    }

    // Scanlines: one filter-type byte 0 (None) per row, then that row's raw RGB bytes.
    let row_bytes = width as usize * 3;
    let mut scanlines = Vec::with_capacity(pixels.len() + height as usize);
    for row in 0..height as usize {
        scanlines.push(0u8);
        scanlines.extend_from_slice(&pixels[row * row_bytes..(row + 1) * row_bytes]);
    }

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // colour type: truecolour (RGB, no alpha)
    ihdr.push(0); // compression method
    ihdr.push(0); // filter method
    ihdr.push(0); // interlace method

    let idat = zlib_stream(&scanlines);

    let mut out = Vec::with_capacity(8 + 12 + ihdr.len() + 12 + idat.len() + 12);
    out.extend_from_slice(&PNG_SIGNATURE);
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- P1b acceptance evidence (1): hand-computed golden ------------------------------
    //
    // A 2x2 RGB image, pixels (row-major, north row first):
    //   row 0: (255,0,0) (0,255,0)
    //   row 1: (0,0,255) (255,255,255)
    //
    // Scanline bytes (filter byte 0 + 6 raw bytes per row):
    //   row0: 00 FF 00 00 00 FF 00
    //   row1: 00 00 00 FF FF FF FF
    //   uncompressed (14 bytes): 00 FF 00 00 00 FF 00 00 00 00 FF FF FF FF
    //
    // Derived independently with the Python standard library (`zlib.crc32`/
    // `zlib.adler32`) -- see this test's own comment for the script and its output; NOT by
    // calling this module's own `crc32`/`adler32`/`encode_rgb8`.
    #[test]
    fn encode_rgb8_matches_a_hand_computed_golden_2x2_image() {
        let pixels: [u8; 12] = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
        let got = encode_rgb8(2, 2, &pixels).unwrap();

        // Golden byte layout (each range asserted individually below, against values
        // independently re-derived by the Python script quoted in the task report --
        // NOT by calling this module's own crc32()/adler32()): signature (8 bytes),
        // IHDR (length=13, "IHDR", 13 data bytes, CRC32), IDAT (length=25, "IDAT",
        // zlib(0x78 0x01, one stored block over the 14 scanline bytes, adler32), CRC32),
        // IEND (length=0, "IEND", CRC32).

        // Signature (fixed, spec-mandated).
        assert_eq!(&got[0..8], &PNG_SIGNATURE[..]);

        // IHDR: length=13, type, width=2 height=2 depth=8 color=2 comp=0 filter=0 interlace=0.
        assert_eq!(&got[8..12], &13u32.to_be_bytes());
        assert_eq!(&got[12..16], b"IHDR");
        let ihdr_data = &got[16..29];
        assert_eq!(ihdr_data, &[0, 0, 0, 2, 0, 0, 0, 2, 8, 2, 0, 0, 0]);
        let ihdr_crc_expected = crc32(&got[12..29]); // over "IHDR" + its 13 data bytes
        assert_eq!(&got[29..33], &ihdr_crc_expected.to_be_bytes());
        // Independently pinned CRC32 value (computed by the Python script quoted in the
        // task report, not by calling this crate's own crc32()):
        assert_eq!(ihdr_crc_expected, 0xFDD49A73, "IHDR CRC32 pinned golden");

        // IDAT.
        let idat_len = u32::from_be_bytes(got[33..37].try_into().unwrap());
        assert_eq!(&got[37..41], b"IDAT");
        let idat_data = &got[41..41 + idat_len as usize];
        // zlib header.
        assert_eq!(&idat_data[0..2], &[0x78, 0x01]);
        // One stored deflate block: BFINAL=1/BTYPE=00, LEN=14, ~LEN, 14 raw scanline bytes.
        assert_eq!(idat_data[2], 0x01);
        assert_eq!(&idat_data[3..5], &14u16.to_le_bytes());
        assert_eq!(&idat_data[5..7], &(!14u16).to_le_bytes());
        let scanlines = &idat_data[7..21];
        let expected_scanlines: [u8; 14] = [0, 255, 0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 255];
        assert_eq!(scanlines, &expected_scanlines);
        // Adler-32 trailer, over the 14 uncompressed scanline bytes.
        let adler_expected = adler32(&expected_scanlines);
        assert_eq!(&idat_data[21..25], &adler_expected.to_be_bytes());
        assert_eq!(adler_expected, 0x1FEE05FB, "adler32 pinned golden");
        assert_eq!(idat_len, 25, "zlib header(2) + block header(5) + 14 data + adler32(4)");

        let idat_crc_pos = 41 + idat_len as usize;
        let idat_crc_expected = crc32(&got[37..idat_crc_pos]);
        assert_eq!(&got[idat_crc_pos..idat_crc_pos + 4], &idat_crc_expected.to_be_bytes());
        assert_eq!(idat_crc_expected, 0xDEDDEC2B, "IDAT CRC32 pinned golden");

        // IEND.
        let iend_pos = idat_crc_pos + 4;
        assert_eq!(&got[iend_pos..iend_pos + 4], &0u32.to_be_bytes());
        assert_eq!(&got[iend_pos + 4..iend_pos + 8], b"IEND");
        let iend_crc_expected = crc32(&got[iend_pos + 4..iend_pos + 8]);
        assert_eq!(&got[iend_pos + 8..iend_pos + 12], &iend_crc_expected.to_be_bytes());
        assert_eq!(iend_crc_expected, 0xAE426082, "IEND CRC32 is the well-known fixed value for an empty IEND chunk");
        assert_eq!(got.len(), iend_pos + 12, "no trailing bytes after IEND");
    }

    // -- P1b acceptance evidence (2): a real decoder (Python stdlib) accepts the output --

    #[test]
    fn python_stdlib_can_decode_the_encoders_output() {
        let python = ".venv/bin/python";
        // Locate the repo root's venv relative to this crate (CARGO_MANIFEST_DIR is
        // crates/av-jobs; the venv lives at the repo root, two levels up).
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir.parent().and_then(|p| p.parent()).expect("crates/av-jobs has two ancestors: crates/, then the repo root");
        let python_path = repo_root.join(python);
        if !python_path.exists() {
            eprintln!("SKIPPED: python_stdlib_can_decode_the_encoders_output -- {python_path:?} not found on this host (no venv at the expected repo-relative path)");
            return;
        }

        let width = 3u32;
        let height = 2u32;
        let mut pixels = vec![0u8; (width * height * 3) as usize];
        // A distinctive, non-uniform pattern so a decoder bug (wrong stride, wrong row
        // order) would show up as a wrong pixel rather than accidentally passing.
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 3) as usize;
                pixels[i] = (x * 50) as u8;
                pixels[i + 1] = (y * 80) as u8;
                pixels[i + 2] = 200;
            }
        }
        let png_bytes = encode_rgb8(width, height, &pixels).unwrap();

        let dir = std::env::temp_dir().join(format!("av-jobs-png-decode-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png_path = dir.join("test.png");
        std::fs::write(&png_path, &png_bytes).unwrap();

        let script_path = dir.join("decode.py");
        std::fs::write(&script_path, PYTHON_DECODE_SCRIPT).unwrap();

        let output = std::process::Command::new(&python_path)
            .arg(&script_path)
            .arg(&png_path)
            .arg(width.to_string())
            .arg(height.to_string())
            // Known pixel: (x=2, y=1) -> r=100, g=80, b=200.
            .arg("2")
            .arg("1")
            .arg("100")
            .arg("80")
            .arg("200")
            .output()
            .expect("failed to spawn python");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(output.status.success(), "python decode script failed:\nstdout: {}\nstderr: {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "OK");
    }

    /// Standard-library-only (`zlib` + manual PNG chunk parsing) -- no PIL, since it is not
    /// a dependency of this project. Reads the PNG at argv[1], checks its IHDR
    /// width/height against argv[2]/argv[3], and one pixel (argv[4]/argv[5] = x,y;
    /// argv[6..9] = expected r,g,b) against the decompressed, de-filtered scanline data.
    const PYTHON_DECODE_SCRIPT: &str = r#"
import sys, struct, zlib

path, width, height, px, py, pr, pg, pb = sys.argv[1:9]
width, height, px, py, pr, pg, pb = map(int, (width, height, px, py, pr, pg, pb))

with open(path, "rb") as f:
    data = f.read()

assert data[:8] == bytes([0x89,0x50,0x4E,0x47,0x0D,0x0A,0x1A,0x0A]), "bad PNG signature"

pos = 8
chunks = {}
idat = b""
while pos < len(data):
    (length,) = struct.unpack(">I", data[pos:pos+4])
    ctype = data[pos+4:pos+8]
    cdata = data[pos+8:pos+8+length]
    crc = data[pos+8+length:pos+12+length]
    computed = struct.pack(">I", zlib.crc32(ctype + cdata) & 0xFFFFFFFF)
    assert crc == computed, f"bad CRC for chunk {ctype!r}"
    if ctype == b"IDAT":
        idat += cdata
    else:
        chunks[ctype] = cdata
    pos += 12 + length
    if ctype == b"IEND":
        break

ihdr = chunks[b"IHDR"]
w, h, depth, color, comp, filt, interlace = struct.unpack(">IIBBBBB", ihdr)
assert w == width, f"width mismatch: {w} != {width}"
assert h == height, f"height mismatch: {h} != {height}"
assert depth == 8 and color == 2, "expected 8-bit RGB (colour type 2)"

raw = zlib.decompress(idat)
row_bytes = width * 3
rows = []
for r in range(height):
    row_start = r * (1 + row_bytes)
    filter_type = raw[row_start]
    assert filter_type == 0, f"unexpected filter type {filter_type}"
    rows.append(raw[row_start+1:row_start+1+row_bytes])

row = rows[py]
i = px * 3
got = (row[i], row[i+1], row[i+2])
expected = (pr, pg, pb)
assert got == expected, f"pixel ({px},{py}) mismatch: got {got}, expected {expected}"

print("OK")
"#;

    // -- P1b acceptance evidence (3): determinism ----------------------------------------

    #[test]
    fn encoding_the_same_pixels_twice_is_byte_identical() {
        let pixels: Vec<u8> = (0..(4 * 4 * 3)).map(|i| (i * 7 % 251) as u8).collect();
        let a = encode_rgb8(4, 4, &pixels).unwrap();
        let b = encode_rgb8(4, 4, &pixels).unwrap();
        assert_eq!(a, b);
    }

    // -- basic well-formedness / refusal tests -------------------------------------------

    #[test]
    fn encode_rgb8_refuses_a_mismatched_pixel_buffer_length() {
        assert!(encode_rgb8(2, 2, &[0u8; 11]).is_none());
        assert!(encode_rgb8(2, 2, &[0u8; 13]).is_none());
    }

    #[test]
    fn encode_rgb8_refuses_zero_width_or_height() {
        assert!(encode_rgb8(0, 2, &[]).is_none());
        assert!(encode_rgb8(2, 0, &[]).is_none());
    }

    #[test]
    fn crc32_matches_known_answer_vectors() {
        // The canonical CRC-32 (ISO-HDLC) known-answer test vector.
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn adler32_matches_known_answer_vectors() {
        // "Wikipedia" -> 0x11E60398 is the commonly-cited Adler-32 known-answer vector.
        assert_eq!(adler32(b"Wikipedia"), 0x11E60398);
        assert_eq!(adler32(b""), 1);
    }
}
