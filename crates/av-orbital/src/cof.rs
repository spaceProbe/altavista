//! A reader for GMAT's own Earth gravity-coefficient files
//! (`$GMAT_ROOT/data/gravity/earth/{JGM2,JGM3,EGM96,EGM96low,JGM2F70}.cof`).
//!
//! # How the format was determined
//!
//! Read directly from the five files under `$GMAT_ROOT/data/gravity/earth/` with `head`,
//! `grep`, `awk` and a scratch Python script (never from memory, never from GMAT's own
//! `GravityFile`/`HarmonicField` reader source, which this crate does not open) -- see this
//! crate's N1 report for the exact commands. Every file opens with a `COMMENT` block (lines
//! whose first character is `C`) and a title line, e.g. (quoted verbatim, trailing
//! whitespace as in the file):
//!
//! ```text
//! JGM2.cof:      CCCCC  JGM-02 : [70x70]                                                    CCCCC
//! JGM3.cof:      CCCCC  JGM-03 : [70x70]                                                    CCCCC
//! EGM96.cof:     CCCCC  egm96_to360.ascii : [360x360] EGM96 using epoch of 1986             CCCCC
//! EGM96low.cof:  CCCCC  egm96_to360.ascii : [70x70] EGM96 using epoch of 1986               CCCCC
//! JGM2F70.cof:   CCCCC  JGM2F70   70X70    NORMALIZED FORCE MODEL            JULY 13, 1995  CCCCC
//! ```
//!
//! Comment lines are skipped by first-character (`'C'` or `'c'`), not by counting the
//! `COMMENT   5` record's declared count -- simpler and just as correct for these files.
//!
//! ## `POTFIELD`: one header record
//!
//! One `POTFIELD` line per file, e.g. (JGM2, `head -7`):
//! `POTFIELD 70 70  1 3.98600441500000e+14 6.37813630000000e+06 1.00000000000000e+00`
//! and (EGM96, the 360x360 file, where the degree and order collide into one run of digits
//! with no separating space): `POTFIELD360360  1 3.98600441500000E+14 6.37813630000000E+06
//! 1.00000000000000E+00`. That collision (confirmed with a byte-offset Python script, not
//! guessed) is why the degree and order fields are read at **fixed columns**, not by
//! whitespace splitting: `"POTFIELD"` (8 bytes), then a 3-byte right-justified max-degree
//! field, then a 3-byte right-justified max-order field (bytes `[8..11]` and `[11..14]`).
//! The remainder of the line -- the normalisation indicator, `mu`, the reference radius, and
//! an always-`1.0` trailing field of undetermined meaning (present and equal to `1.0` in all
//! five files; this reader refuses any file where it is not, rather than guess) -- is
//! whitespace-delimited and safe to `split_whitespace()`.
//!
//! The indicator field (`1` in every file observed) is read as "coefficients are fully
//! normalised"; a file with a different value is rejected as [`CofError::UnsupportedFormat`]
//! rather than silently mis-read.
//!
//! `mu` and the reference radius are **already SI** in the file: `3.98600441500000e+14`
//! (m^3/s^2 -- Earth's GM is 398600.4415 km^3/s^2, and `3.986004415e14 == 398600.4415 *
//! 1e9`, the km^3-to-m^3 factor) and `6.37813630000000e+06` (m -- `6378136.3` m is JGM2/EGM96's
//! standard equatorial reference radius in km times 1000). **This reader stores and returns
//! everything in SI (metres, seconds, m^3/s^2) and performs no unit conversion at all**,
//! because the file is already in those units; the km convention used elsewhere in this
//! workspace (`crates/gmat-sys`, the goldens) is a boundary the *next* N1 worker's frame/DRM
//! wiring crosses, not this reader.
//!
//! ## `RECOEF`: one coefficient record per `(n, m)`
//!
//! Fixed-width, confirmed the same way (byte-offset inspection, not whitespace splitting --
//! at `n=200, m=100` in `EGM96.cof` the degree and order collide exactly as `POTFIELD`'s do:
//! `RECOEF  200100    2.75969989559000E-11-1.11621366316000E-12`, six digits with no space
//! between them):
//!
//! | bytes | field | notes |
//! |---|---|---|
//! | `0..6` | `"RECOEF"` | literal |
//! | `6..11` | degree `n` | 5-byte right-justified integer |
//! | `11..14` | order `m` | 3-byte right-justified integer |
//! | `14..17` | (spacer) | always blank |
//! | `17..38` | `C_nm` | 21-byte signed scientific field (sign-or-blank, 1 digit, `.`, 14 digits, `e`/`E`, sign, 2-digit exponent) |
//! | `38..59` | `S_nm` | same 21-byte field, present only when the record has one (all `m >= 1` records; `JGM2F70.cof`'s `m = 0` records additionally carry an explicit `0.00000000000000E+00` here, which this reader accepts and ignores since `S_n0` is defined to be zero) |
//!
//! Every file uses CRLF line endings (confirmed with `od -c`); Rust's `str::lines()` strips
//! both `\n` and a preceding `\r`, so no special handling is needed once the file is read as
//! a `String`.
//!
//! Coefficients are read starting at `n = 2` (no file has an `n = 0` or `n = 1` `RECOEF`
//! record; `C_00 = 1`, `C_10 = C_11 = S_11 = 0` are the standard implied values for a field
//! referenced to the body's centre of mass and are simply left at `0.0` -- except `c(0, 0)`,
//! which [`GravityModel::c`] returns as `1.0` by definition rather than `0.0`, matching the
//! point-mass term every spherical-harmonic expansion starts from).
//!
//! ## How the reader was verified
//!
//! Two independent checks, both run as tests in this module and both printing their measured
//! numbers (see this crate's N1 report for the captured values):
//!
//! 1. **Content pinning.** [`GMAT_ROOT`]'s five files are hashed with `openssl::sha::sha256`
//!    (ADR-004's crypto rule) and compared against a hex digest committed in
//!    `cof_sha256_pins_all_five_files`, computed independently with the system `shasum -a
//!    256` first. A data-pack change (ADR-002's rule) cannot go unnoticed.
//! 2. **Physical cross-check.** JGM2's degree-2 zonal coefficient, read by this parser and
//!    converted from fully-normalised to the classical unnormalised `J2` via `J2 =
//!    -C_20_normalised * sqrt(5)` (the `m = 0` normalisation factor `N_n0 = sqrt(2n + 1)`,
//!    derived in `legendre.rs`'s module doc), reproduces the accepted `J2` of the Earth
//!    (~1.0826e-3) -- see `j2_cross_check_matches_accepted_value` in this module and the
//!    measured value in the N1 report.
//!
//! # Errors
//!
//! Every failure mode -- a missing file, a missing `GMAT_ROOT` with no fallback, a missing or
//! malformed `POTFIELD` record, a truncated or non-numeric `RECOEF` record, a requested
//! degree/order the file does not contain -- is a typed [`CofError`] variant. Nothing in this
//! module panics or unwraps on file content; `env::var("GMAT_ROOT")` is read once and never
//! written (question 199).

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// Every way reading a `.cof` gravity file can fail. No variant is reached by a panic or an
/// `unwrap` on file content -- every malformed input is a typed error here.
#[derive(Debug, thiserror::Error)]
pub enum CofError {
    /// `GMAT_ROOT` was not set in the environment and the repo-relative fallback
    /// (`<repo root>/GMAT R2026a`, the same fallback `crates/gmat-sys/build.rs` uses) does
    /// not exist either.
    #[error(
        "GMAT_ROOT is not set and the fallback GMAT install was not found at {fallback}; \
         set GMAT_ROOT to a GMAT R2026a install (contains data/, bin/)"
    )]
    GmatRootNotFound {
        /// The fallback path that was checked.
        fallback: PathBuf,
    },
    /// The file could not be opened or read.
    #[error("could not read gravity file {path}: {source}")]
    Io {
        /// The file that could not be read.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file's bytes are not valid UTF-8 (every `.cof` file this reader has seen is plain
    /// ASCII; anything else is rejected rather than lossily reinterpreted).
    #[error("gravity file {path} is not valid UTF-8 text")]
    Encoding {
        /// The file that failed to decode.
        path: PathBuf,
    },
    /// No `POTFIELD` record was found before end of file.
    #[error("gravity file {path} has no POTFIELD header record")]
    MissingHeader {
        /// The file that was missing its header.
        path: PathBuf,
    },
    /// The `POTFIELD` record does not match the format this reader understands (see this
    /// module's doc): a degree/order field this reader could not parse, a normalisation
    /// indicator other than `1` (fully normalised), or a trailing scale field other than
    /// `1.0`.
    #[error("gravity file {path} has an unsupported POTFIELD record: {reason}")]
    UnsupportedFormat {
        /// The file with the unsupported header.
        path: PathBuf,
        /// What about the header was not understood.
        reason: String,
    },
    /// A `RECOEF` record was truncated, non-ASCII, or had a field that did not parse as the
    /// expected integer or float.
    #[error("gravity file {path}, line {line}: malformed RECOEF record ({reason})")]
    MalformedRecord {
        /// The file containing the bad record.
        path: PathBuf,
        /// The 1-based line number of the bad record.
        line: usize,
        /// What was wrong with it.
        reason: String,
    },
    /// The caller asked for a degree or order the file does not contain.
    #[error(
        "gravity file {path} is {file_degree}x{file_order}; cannot satisfy the requested \
         degree/order {requested_degree}x{requested_order}"
    )]
    DegreeOutOfRange {
        /// The file that was too small.
        path: PathBuf,
        /// The degree the caller requested.
        requested_degree: usize,
        /// The order the caller requested.
        requested_order: usize,
        /// The file's own maximum degree.
        file_degree: usize,
        /// The file's own maximum order.
        file_order: usize,
    },
}

/// Fully-normalised Earth gravity-field coefficients read from a GMAT `.cof` file, truncated
/// to a requested degree and order.
///
/// `mu` is in m^3/s^2 and `reference_radius` is in metres (see this module's doc: the `.cof`
/// file is already SI, so nothing here is converted). `c(n, m)` / `s(n, m)` return the
/// fully-normalised `C_nm` / `S_nm` coefficients (`c(0, 0) == 1.0` by definition; every
/// coefficient this reader did not load, including every `m` beyond the requested order or
/// `n` beyond the requested degree, reads back as `0.0`).
#[derive(Debug, Clone)]
pub struct GravityModel {
    mu: f64,
    reference_radius: f64,
    max_degree: usize,
    max_order: usize,
    // Rectangular (max_degree+1) x (max_order+1) row-major layout: index(n, m) = n *
    // (max_order + 1) + m. Wastes a little space versus a packed triangular layout when
    // max_order < max_degree (never the case for these five files, all square), in exchange
    // for a trivial, panic-free index computation with no cumulative-offset arithmetic.
    c: Vec<f64>,
    s: Vec<f64>,
}

impl GravityModel {
    /// Standard gravitational parameter, m^3/s^2.
    pub fn mu(&self) -> f64 {
        self.mu
    }

    /// Reference (equatorial) radius the coefficients are normalised to, metres.
    pub fn reference_radius(&self) -> f64 {
        self.reference_radius
    }

    /// The degree this model was truncated to.
    pub fn max_degree(&self) -> usize {
        self.max_degree
    }

    /// The order this model was truncated to.
    pub fn max_order(&self) -> usize {
        self.max_order
    }

    fn index(&self, n: usize, m: usize) -> Option<usize> {
        if n > self.max_degree || m > self.max_order || m > n {
            return None;
        }
        Some(n * (self.max_order + 1) + m)
    }

    /// The fully-normalised `C_nm` coefficient. `c(0, 0)` is `1.0` by definition (the
    /// point-mass term); any `(n, m)` outside what this model loaded is `0.0`.
    pub fn c(&self, n: usize, m: usize) -> f64 {
        if n == 0 && m == 0 {
            return 1.0;
        }
        self.index(n, m).map(|i| self.c[i]).unwrap_or(0.0)
    }

    /// The fully-normalised `S_nm` coefficient (`s(n, 0)` is always `0.0`).
    pub fn s(&self, n: usize, m: usize) -> f64 {
        self.index(n, m).map(|i| self.s[i]).unwrap_or(0.0)
    }
}

/// Locates the GMAT install the same way `crates/gmat-sys/build.rs` does: `GMAT_ROOT` from
/// the environment (read only, never mutated -- question 199), falling back to `<repo
/// root>/GMAT R2026a` where the repo root is two directories up from this crate's own
/// `CARGO_MANIFEST_DIR` (`crates/av-orbital` -> `crates` -> the repo root, the identical
/// depth `crates/gmat-sys/build.rs`'s own `repo_root()` uses from `crates/gmat-sys`).
///
/// Returns [`CofError::GmatRootNotFound`], naming the exact fallback path it checked, if
/// `GMAT_ROOT` is unset and that fallback does not exist -- never a silent pass.
pub fn locate_gmat_root() -> Result<PathBuf, CofError> {
    if let Ok(from_env) = env::var("GMAT_ROOT") {
        return Ok(PathBuf::from(from_env));
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fallback = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(|repo_root| repo_root.join("GMAT R2026a"))
        .unwrap_or_else(|| PathBuf::from("GMAT R2026a"));
    if fallback.is_dir() {
        Ok(fallback)
    } else {
        Err(CofError::GmatRootNotFound { fallback })
    }
}

/// Reads and parses a GMAT `.cof` Earth gravity file, truncated to `max_degree` x
/// `max_order`. See this module's doc for the file format and how it was determined.
pub fn read_earth_gravity(
    path: &Path,
    max_degree: usize,
    max_order: usize,
) -> Result<GravityModel, CofError> {
    let bytes = std::fs::read(path).map_err(|source| CofError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let text = String::from_utf8(bytes).map_err(|_| CofError::Encoding {
        path: path.to_path_buf(),
    })?;

    let mut file_degree = None;
    let mut file_order = None;
    let mut mu = None;
    let mut reference_radius = None;

    let mut c = vec![0.0_f64; (max_degree + 1) * (max_order + 1)];
    let mut s = vec![0.0_f64; (max_degree + 1) * (max_order + 1)];

    for (line_no, raw_line) in text.lines().enumerate() {
        let line = raw_line;
        if line.is_empty() {
            continue;
        }
        if line.starts_with('C') || line.starts_with('c') {
            continue; // comment block, including the CCCC banner lines
        }
        if line.starts_with("COMMENT") || line.starts_with("END") {
            continue;
        }
        if line.starts_with("POTFIELD") {
            parse_potfield(path, line, &mut file_degree, &mut file_order, &mut mu, &mut reference_radius)?;
            continue;
        }
        if line.starts_with("RECOEF") {
            parse_recoef(path, line, line_no + 1, max_degree, max_order, &mut c, &mut s)?;
            continue;
        }
        // Anything else (a record type this reader has not seen) is silently ignored,
        // matching the tolerant, forward-compatible reading a fixed-format geophysical data
        // file warrants -- only POTFIELD and RECOEF carry the data this reader needs.
    }

    let (file_degree, file_order) = match (file_degree, file_order) {
        (Some(d), Some(o)) => (d, o),
        _ => {
            return Err(CofError::MissingHeader {
                path: path.to_path_buf(),
            })
        }
    };
    let (mu, reference_radius) = match (mu, reference_radius) {
        (Some(m), Some(r)) => (m, r),
        _ => {
            return Err(CofError::MissingHeader {
                path: path.to_path_buf(),
            })
        }
    };
    if max_degree > file_degree || max_order > file_order {
        return Err(CofError::DegreeOutOfRange {
            path: path.to_path_buf(),
            requested_degree: max_degree,
            requested_order: max_order,
            file_degree,
            file_order,
        });
    }

    Ok(GravityModel {
        mu,
        reference_radius,
        max_degree,
        max_order,
        c,
        s,
    })
}

fn parse_potfield(
    path: &Path,
    line: &str,
    file_degree: &mut Option<usize>,
    file_order: &mut Option<usize>,
    mu: &mut Option<f64>,
    reference_radius: &mut Option<f64>,
) -> Result<(), CofError> {
    if !line.is_ascii() || line.len() < 14 {
        return Err(CofError::UnsupportedFormat {
            path: path.to_path_buf(),
            reason: "POTFIELD record shorter than the fixed degree/order fields".to_string(),
        });
    }
    let n: usize = line[8..11].trim().parse().map_err(|_| CofError::UnsupportedFormat {
        path: path.to_path_buf(),
        reason: format!("POTFIELD degree field {:?} is not an integer", &line[8..11]),
    })?;
    let m: usize = line[11..14].trim().parse().map_err(|_| CofError::UnsupportedFormat {
        path: path.to_path_buf(),
        reason: format!("POTFIELD order field {:?} is not an integer", &line[11..14]),
    })?;

    let rest: Vec<&str> = line[14..].split_whitespace().collect();
    let [indicator, mu_str, radius_str, tail @ ..] = rest.as_slice() else {
        return Err(CofError::UnsupportedFormat {
            path: path.to_path_buf(),
            reason: format!("POTFIELD record has too few fields after N/M: {rest:?}"),
        });
    };
    if *indicator != "1" {
        return Err(CofError::UnsupportedFormat {
            path: path.to_path_buf(),
            reason: format!(
                "POTFIELD normalisation indicator {indicator:?} is not \"1\" (fully \
                 normalised); this reader only understands fully-normalised coefficients"
            ),
        });
    }
    let mu_val: f64 = mu_str.parse().map_err(|_| CofError::UnsupportedFormat {
        path: path.to_path_buf(),
        reason: format!("POTFIELD mu field {mu_str:?} is not a number"),
    })?;
    let radius_val: f64 = radius_str.parse().map_err(|_| CofError::UnsupportedFormat {
        path: path.to_path_buf(),
        reason: format!("POTFIELD reference-radius field {radius_str:?} is not a number"),
    })?;
    if let Some(scale_str) = tail.first() {
        let scale: f64 = scale_str.parse().map_err(|_| CofError::UnsupportedFormat {
            path: path.to_path_buf(),
            reason: format!("POTFIELD trailing field {scale_str:?} is not a number"),
        })?;
        if (scale - 1.0).abs() > 1e-12 {
            return Err(CofError::UnsupportedFormat {
                path: path.to_path_buf(),
                reason: format!(
                    "POTFIELD trailing field is {scale}, not the 1.0 this reader has only \
                     ever observed and whose meaning is otherwise undetermined"
                ),
            });
        }
    }

    *file_degree = Some(n);
    *file_order = Some(m);
    *mu = Some(mu_val);
    *reference_radius = Some(radius_val);
    Ok(())
}

fn parse_recoef(
    path: &Path,
    line: &str,
    line_no: usize,
    max_degree: usize,
    max_order: usize,
    c: &mut [f64],
    s: &mut [f64],
) -> Result<(), CofError> {
    if !line.is_ascii() || line.len() < 38 {
        return Err(CofError::MalformedRecord {
            path: path.to_path_buf(),
            line: line_no,
            reason: "record shorter than the fixed N/M/C fields".to_string(),
        });
    }
    let malformed = |reason: String| CofError::MalformedRecord {
        path: path.to_path_buf(),
        line: line_no,
        reason,
    };
    let n: usize = line[6..11]
        .trim()
        .parse()
        .map_err(|_| malformed(format!("degree field {:?} is not an integer", &line[6..11])))?;
    let m: usize = line[11..14]
        .trim()
        .parse()
        .map_err(|_| malformed(format!("order field {:?} is not an integer", &line[11..14])))?;
    if m > n {
        return Err(malformed(format!("order {m} exceeds degree {n}")));
    }
    let c_val: f64 = line[17..38]
        .trim()
        .parse()
        .map_err(|_| malformed(format!("C field {:?} is not a number", &line[17..38])))?;
    let s_val: f64 = if line.len() > 38 && !line[38..].trim().is_empty() {
        if line.len() < 59 {
            return Err(malformed("S field present but truncated".to_string()));
        }
        line[38..59]
            .trim()
            .parse()
            .map_err(|_| malformed(format!("S field {:?} is not a number", &line[38..59])))?
    } else {
        0.0
    };

    if n <= max_degree && m <= max_order {
        let idx = n * (max_order + 1) + m;
        c[idx] = c_val;
        s[idx] = s_val;
    }
    Ok(())
}

impl fmt::Display for GravityModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GravityModel(mu={} m^3/s^2, Re={} m, {}x{})",
            self.mu, self.reference_radius, self.max_degree, self.max_order
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn earth_gravity_dir() -> Result<PathBuf, CofError> {
        Ok(locate_gmat_root()?.join("data").join("gravity").join("earth"))
    }

    /// Every measured tolerance in this crate is measured, not asserted from a paper
    /// (docs/native-dynamics-plan.md's binding rule). This test pins the five gravity
    /// data-pack files' content by SHA-256 (`openssl::sha::sha256`, ADR-004's crypto rule),
    /// computed independently with the system `shasum -a 256` binary first -- both recorded
    /// in this crate's N1 report. A data-pack change now fails this test loudly instead of
    /// silently changing every downstream golden (ADR-002's data-pack rule, question 22).
    #[test]
    fn cof_sha256_pins_all_five_files() {
        let dir = earth_gravity_dir().expect(
            "GMAT_ROOT must be set (or the repo-relative fallback must exist) to run this test",
        );
        let expected: [(&str, &str); 5] = [
            ("JGM2.cof", "bf182b1208e33be3716c8292a3947c1f87d4041ad6663a0c9ae621f65cafc20d"),
            ("JGM3.cof", "299b9ca1d8e8f1600fe7d4f341793f060724f6cbc4db969a39020b4ba3d5b379"),
            ("EGM96.cof", "3e8806223250224d3c1333a010eb5c2cc5a212a56ccee4024db81b9d889db5ea"),
            ("EGM96low.cof", "0e5261dc3fd2152145404ee36ae4645b4bc1b39016e8f823c9401b94d7d4beeb"),
            ("JGM2F70.cof", "fb6b448f03a86c660e6ddd6fcc73a1abb5f94b77f6d2dae68aa722c42a47aee9"),
        ];
        for (name, want_hex) in expected {
            let path = dir.join(name);
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let digest = openssl::sha::sha256(&bytes);
            let got_hex = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
            println!("n1a-cof-sha256: {name} = {got_hex}");
            assert_eq!(got_hex, want_hex, "{name} content changed (SHA-256 mismatch)");
        }
    }

    /// Cross-checks the parser against real physics, not just against the file's own bytes:
    /// JGM2's `C_20` (fully normalised) converts to the classical unnormalised `J2` via `J2 =
    /// -C_20 * sqrt(5)` (the `m = 0` normalisation factor `N_n0 = sqrt(2n + 1)`, so `C_20 =
    /// C_20_unnormalised / sqrt(5)`, i.e. `C_20_unnormalised = C_20 * sqrt(5)`, and `J2 =
    /// -C_20_unnormalised` by definition of `J2`), and should land on the Earth's accepted
    /// `J2` of ~1.0826e-3 (e.g. Vallado, *Fundamentals of Astrodynamics*, table of Earth
    /// constants).
    #[test]
    fn j2_cross_check_matches_accepted_value() {
        let dir = earth_gravity_dir().expect("GMAT_ROOT must be set to run this test");
        let model = read_earth_gravity(&dir.join("JGM2.cof"), 8, 8).expect("parse JGM2.cof");
        let c20 = model.c(2, 0);
        let j2 = -c20 * 5.0_f64.sqrt();
        println!("n1a-j2-cross-check: C_20={c20:e} J2={j2:e}");
        let accepted_j2 = 1.0826e-3;
        let relative_error = ((j2 - accepted_j2) / accepted_j2).abs();
        println!("n1a-j2-cross-check: relative_error_vs_1.0826e-3={relative_error:e}");
        assert!(
            relative_error < 3e-5,
            "J2={j2} disagrees with the accepted 1.0826e-3 by {relative_error:e}"
        );
    }

    #[test]
    fn point_mass_term_is_one() {
        let dir = earth_gravity_dir().expect("GMAT_ROOT must be set to run this test");
        let model = read_earth_gravity(&dir.join("JGM2.cof"), 4, 4).expect("parse JGM2.cof");
        assert_eq!(model.c(0, 0), 1.0);
        assert_eq!(model.s(0, 0), 0.0);
        assert_eq!(model.c(1, 0), 0.0);
        assert_eq!(model.c(1, 1), 0.0);
        assert_eq!(model.s(1, 1), 0.0);
    }

    #[test]
    fn degree_out_of_range_is_a_typed_error() {
        let dir = earth_gravity_dir().expect("GMAT_ROOT must be set to run this test");
        let err = read_earth_gravity(&dir.join("JGM2.cof"), 71, 71).unwrap_err();
        assert!(matches!(err, CofError::DegreeOutOfRange { .. }), "{err:?}");
    }

    #[test]
    fn missing_file_is_a_typed_error() {
        let err = read_earth_gravity(Path::new("/no/such/file.cof"), 4, 4).unwrap_err();
        assert!(matches!(err, CofError::Io { .. }), "{err:?}");
    }

    #[test]
    fn malformed_recoef_record_is_a_typed_error() {
        // A truncated record: the C field is cut off mid-number. Exercises the "no panic,
        // no unwrap on input data" rule directly, independent of any real .cof file.
        let bad = "POTFIELD  4  4  1 3.986e+14 6.378e+06 1.0\nRECOEF    2  0   -4.84x\nEND\n";
        let path = std::env::temp_dir().join("av_orbital_malformed_test.cof");
        std::fs::write(&path, bad).expect("write scratch file");
        let err = read_earth_gravity(&path, 4, 4).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(matches!(err, CofError::MalformedRecord { .. }), "{err:?}");
    }

    #[test]
    fn jgm2f70_explicit_zero_s_on_m0_is_accepted() {
        let dir = earth_gravity_dir().expect("GMAT_ROOT must be set to run this test");
        let model = read_earth_gravity(&dir.join("JGM2F70.cof"), 8, 8).expect("parse JGM2F70.cof");
        assert_eq!(model.s(2, 0), 0.0);
        assert!(model.c(2, 0) < 0.0);
    }

    #[test]
    fn egm96_360x360_header_collision_is_parsed() {
        let dir = earth_gravity_dir().expect("GMAT_ROOT must be set to run this test");
        let model = read_earth_gravity(&dir.join("EGM96.cof"), 360, 360).expect("parse EGM96.cof");
        assert_eq!(model.max_degree(), 360);
        assert_eq!(model.max_order(), 360);
        // n=200, m=100 is exactly the byte-offset collision case found during format
        // discovery ("RECOEF  200100 ..."); confirm it was not silently dropped or misread.
        assert!(model.c(200, 100) != 0.0 || model.s(200, 100) != 0.0);
    }
}
