//! A reader for GMAT's own JPL DE binary planetary/lunar ephemeris files
//! (`$GMAT_ROOT/data/planetary_ephem/de/leDE*.4xx`), and Chebyshev evaluation of a body's
//! geocentric position (and velocity) at a TDB epoch (`docs/native-dynamics-plan.md`
//! milestone N2).
//!
//! **Which file GMAT actually uses.** Read directly off the running GMAT configuration
//! (`SolarSystem.EphemerisSource`, `SolarSystem.DEFilename`, and each `CelestialBody`'s own
//! `PosVelSource`), not assumed: `Gmat::setup` at GMAT's own default startup file
//! (`$GMAT_ROOT/bin/api_startup_file.txt`, unmodified) reports `EphemerisSource = "DE405"`,
//! `DEFilename = ".../data/planetary_ephem/de/leDE1941.405"`, and `Earth`/`Luna`/`Sun` all
//! report `PosVelSource = "DE405"` -- the SPICE `.bsp` files under `data/planetary_ephem/spk/`
//! are present on disk but not the active source under GMAT's shipped default configuration
//! (`SPKFilename` is populated regardless of which source is active -- it is a configured
//! path, not evidence of use). This crate's own N2 report has the full probe transcript. This
//! reader therefore targets `leDE1941.405` (DE405); the format below is the DE405-and-later
//! Chebyshev layout, so the same reader also opens `leDE1900.421`/`leDE18002100.424`, but only
//! `leDE1941.405` is pinned by SHA-256 and tested against GMAT in this crate.
//!
//! # File layout, determined empirically (not from memory -- see this crate's N2 report for
//! every cross-check number quoted below)
//!
//! Two fixed-format header records followed by fixed-length Chebyshev coefficient-block
//! records, all little-endian (the file's own `le` name prefix) `f64`/`i32`.
//!
//! **Record 1** (byte offsets from 0, as read by a small Python probe script over the raw
//! file, `struct.unpack_from('<...', data, offset)`):
//!
//! | bytes | field | content |
//! |---|---|---|
//! | `0..252` | `TTL` | 3 x 84-char title lines; `leDE1941.405`'s first line reads (as ASCII, up to embedded padding) `"JPL Planetary Ephemeris DE405/DE405"` |
//! | `252..2652` | `CNAM` | 400 x 6-char constant names (`NCON` of them valid; DE200-DE43x fix this area at 400 slots regardless of the file's actual `NCON`, confirmed here because the parsed `NCON` (156) and the resulting `IPT`/`LPT` table are internally consistent with everything after this area -- see below) |
//! | `2652..2676` | `SS` | 3 `f64`: start JD, end JD, days per data block. Measured: `(2430000.5, 2525008.5, 32.0)` |
//! | `2676..2680` | `NCON` | `i32`. Measured: `156` |
//! | `2680..2688` | `AU` | `f64`, km. Measured: `149597870.691` (matches the ~1.4959787e8 cross-check the task names) |
//! | `2688..2696` | `EMRAT` | `f64`, Earth/Moon mass ratio. Measured: `81.30056` (matches the ~81.30056 cross-check) |
//! | `2696..2840` | `IPT` | 12 x 3 `i32`: `(start_index_1based, ncoeff, nsub)` per body (Mercury, Venus, Earth-Moon barycentre, Mars, Jupiter, Saturn, Uranus, Neptune, Pluto, Moon-geocentric, Sun, nutations) |
//! | `2840..2844` | `NUMDE` | `i32`, DE version. Measured: `405` |
//! | `2844..2856` | `LPT` | 3 `i32`: lunar-libration pointer `(start_index_1based, ncoeff, nsub)` |
//!
//! **`KSIZE` (record length in `f64` words), derived from the pointer table, not hardcoded.**
//! Every `IPT`/`LPT` entry's byte range is `[start-1, start-1 + ncoeff*components*nsub)`
//! (`components` is 3 for every body except nutations, which has 2); `KSIZE` is the maximum
//! such end over all 13 entries. Measured for `leDE1941.405`: the nutation entry
//! `(819, 10, 4)` ends at word 898 and `LPT = (899, 10, 4)` starts at word 899 with no gap
//! (a direct check this crate's N2 report quotes), and `LPT` itself ends at word 1018, so
//! `KSIZE = 1018` f64 words = `8144` bytes. **Cross-check the task asks for by name:**
//! `file_size % (KSIZE * 8) == 0` -- `24_195_824 % 8144 == 0` (quotient `2971`, matching `2`
//! header records plus `(2_525_008.5 - 2_430_000.5) / 32.0 == 2969` data-block records
//! exactly), confirmed both by this module's own construction-time check
//! ([`DeError::RecordSizeMismatch`] on any other file) and independently in the N2 report.
//!
//! **Record 2** holds `CVAL`: `NCON` `f64` constants in the order `CNAM` names them, including
//! the per-body `GM*`/`GMB`/`GMS` constants in **AU^3/day^2** (the DE ephemeris's own working
//! units -- `GMS` measured as `2.959122082855911e-4`, the well-known solar `k^2` Gaussian
//! constant, confirming the unit and the record-2 offset). [`DeEphemeris::mu_si`] converts via
//! `mu_si = mu_au3_per_day2 * AU_km^3 / 86400^2`.
//!
//! # Body indexing and the geocentric/barycentric handling (EMRAT)
//!
//! `IPT` entry 9 (0-based) is the Moon's position **already geocentric** (relative to Earth,
//! not the solar-system barycentre) -- the standard DE convention this module relies on, so
//! [`DeEphemeris::geocentric_position_km`] for [`DeBody::Moon`] is a single Chebyshev
//! evaluation with no further transformation. Every other body's `IPT` entry (Sun included)
//! is barycentric (relative to the solar-system barycentre); Earth's own barycentric position
//! is not tabulated directly and is recovered from the Earth-Moon-barycentre entry (index 2)
//! and the Moon's geocentric vector via the standard mass-ratio split:
//!
//! ```text
//! r_Earth(SSB) = r_EMB(SSB) - r_Moon(geocentric) / (1 + EMRAT)
//! ```
//!
//! (`EMRAT = mass(Earth)/mass(Moon)`, so `mass(Moon)/(mass(Earth)+mass(Moon)) = 1/(EMRAT+1)` --
//! the barycentre lags Earth by exactly that fraction of the Earth-Moon vector.) Any other
//! body's geocentric position is then `r_body(SSB) - r_Earth(SSB)`. Cross-checked against
//! GMAT's own `CelestialBody::Mu` for `Earth`/`Luna`/`Sun` in this crate's N2 report (the
//! `GMB`/`EMRAT` split reproduces GMAT's own reported `Luna`/`Earth` `Mu` to 1e-9 relative,
//! and `GMS` reproduces `Sun`'s `Mu` exactly to the digits GMAT prints), which is independent
//! evidence that GMAT's own point-mass force reads its body masses from this exact file.
//!
//! # Chebyshev evaluation
//!
//! Standard three-term recursion, `T_0(tc)=1`, `T_1(tc)=tc`, `T_k(tc) = 2 tc T_{k-1}(tc) -
//! T_{k-2}(tc)`, position `= sum_k coeff[k] T_k(tc)`; velocity by the companion derivative
//! recursion `dT_0/dtc=0`, `dT_1/dtc=1`, `dT_k/dtc = 2 T_{k-1} + 2 tc dT_{k-1}/dtc -
//! dT_{k-2}/dtc`, scaled by `dtc/dt_seconds = 2 / (subinterval_days * 86400)`. `tc` is the
//! Chebyshev-normalised time in `[-1, 1]` within the record's `nsub`-way subdivision of the
//! `SS[2]`-day block the epoch falls in (the Moon's `nsub=8` subdivides its block into eight
//! 4-day spans; other bodies have coarser `nsub`).
//!
//! No panics, no `unwrap` on file content -- every malformed input or out-of-range epoch is a
//! typed [`DeError`].
use std::path::{Path, PathBuf};

/// Every way reading or evaluating a DE ephemeris file can fail.
#[derive(Debug, thiserror::Error)]
pub enum DeError {
    /// `GMAT_ROOT` was not set and no fallback install was found (mirrors [`crate::cof::CofError::GmatRootNotFound`]).
    #[error("GMAT_ROOT is not set and the fallback GMAT install was not found at {fallback}")]
    GmatRootNotFound { fallback: PathBuf },
    /// The file could not be opened or read.
    #[error("could not read DE ephemeris file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file is shorter than the fixed two-record header this reader parses.
    #[error("DE ephemeris file {path} is only {len} bytes, shorter than the {want}-byte header")]
    Truncated { path: PathBuf, len: usize, want: usize },
    /// `AU`, `EMRAT`, or a `GM*`/`GMB`/`GMS` constant read as a non-finite or implausible
    /// value (this reader's one internal physical cross-check, done at construction time
    /// rather than left for a caller to discover downstream).
    #[error("DE ephemeris file {path} header field {field} = {value}, outside the plausible range this reader checks")]
    ImplausibleHeader { path: PathBuf, field: &'static str, value: f64 },
    /// The derived `KSIZE` (from the `IPT`/`LPT` pointer table) does not evenly divide the
    /// file size -- the record-layout cross-check this module's doc names.
    #[error("DE ephemeris file {path}: derived record size {ksize_bytes} bytes does not divide the file size {file_size} bytes (file_size % ksize_bytes = {remainder})")]
    RecordSizeMismatch { path: PathBuf, ksize_bytes: usize, file_size: usize, remainder: usize },
    /// The requested epoch (Julian Date, TDB) is outside `[SS[0], SS[1])`.
    #[error("epoch JD(TDB) {jd} is outside this ephemeris's covered span [{jd_start}, {jd_end})")]
    EpochOutOfRange { jd: f64, jd_start: f64, jd_end: f64 },
    /// A named physical constant (e.g. `"GMS"`) was requested but is not present in this
    /// file's `CNAM`/`CVAL` tables.
    #[error("DE ephemeris file has no constant named {0:?}")]
    UnknownConstant(String),
}

/// A target body's `IPT`/`LPT` slot in the ephemeris (0-based index into `IPT`, with
/// [`DeBody::Libration`] addressing `LPT` instead -- see [`DeEphemeris::raw_state`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeBody {
    Mercury,
    Venus,
    EarthMoonBarycenter,
    Mars,
    Jupiter,
    Saturn,
    Uranus,
    Neptune,
    Pluto,
    /// Geocentric (relative to Earth) directly -- the one `IPT` entry that is NOT
    /// barycentric; see this module's doc.
    Moon,
    Sun,
    Nutation,
    Libration,
}

impl DeBody {
    /// This body's 0-based `IPT` row, `None` for [`DeBody::Libration`] (which uses `LPT`
    /// instead of `IPT`).
    fn ipt_index(self) -> Option<usize> {
        use DeBody::*;
        match self {
            Mercury => Some(0),
            Venus => Some(1),
            EarthMoonBarycenter => Some(2),
            Mars => Some(3),
            Jupiter => Some(4),
            Saturn => Some(5),
            Uranus => Some(6),
            Neptune => Some(7),
            Pluto => Some(8),
            Moon => Some(9),
            Sun => Some(10),
            Nutation => Some(11),
            Libration => None,
        }
    }

    /// Number of Chebyshev components this body's entry carries: 2 for nutations
    /// (`d(psi)`, `d(epsilon)`), 3 for every position (librations included).
    fn components(self) -> usize {
        if matches!(self, DeBody::Nutation) {
            2
        } else {
            3
        }
    }
}

/// `(start_index_1based, ncoeff, nsub)`, exactly as `IPT`/`LPT` store it.
#[derive(Debug, Clone, Copy)]
struct Pointer {
    start: usize,
    ncoeff: usize,
    nsub: usize,
}

/// A parsed JPL DE binary ephemeris (see this module's doc for the file layout and every
/// cross-check).
///
/// `Debug` is a manual, summary impl (below) rather than `#[derive(Debug)]`: the `data: Vec<u8>`
/// field is a whole ephemeris file (tens of MB), and the default derive would dump every byte
/// into any failed assertion's message.
pub struct DeEphemeris {
    data: Vec<u8>,
    jd_start: f64,
    jd_end: f64,
    block_days: f64,
    ksize_bytes: usize,
    numde: i32,
    au_km: f64,
    emrat: f64,
    ipt: [Pointer; 12],
    lpt: Pointer,
    /// `(name, value)` for the `NCON` constants in `CVAL` (record 2), in `CNAM` order -- kept
    /// as a small linear-scan table (NCON is 156 for DE405; a `BTreeMap` buys nothing at this
    /// size and would need an allocation per name anyway).
    constants: Vec<(String, f64)>,
}

impl std::fmt::Debug for DeEphemeris {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeEphemeris")
            .field("numde", &self.numde)
            .field("jd_start", &self.jd_start)
            .field("jd_end", &self.jd_end)
            .field("block_days", &self.block_days)
            .field("ksize_bytes", &self.ksize_bytes)
            .field("au_km", &self.au_km)
            .field("emrat", &self.emrat)
            .field("data_len", &self.data.len())
            .finish()
    }
}

const HEADER1_LEN: usize = 2856;
const TTL_LEN: usize = 252;
const CNAM_OFFSET: usize = TTL_LEN;
const CNAM_SLOTS: usize = 400;
const CNAM_LEN: usize = CNAM_SLOTS * 6;
const SS_OFFSET: usize = CNAM_OFFSET + CNAM_LEN; // 2652
const NCON_OFFSET: usize = SS_OFFSET + 24; // 2676
const AU_OFFSET: usize = NCON_OFFSET + 4; // 2680
const EMRAT_OFFSET: usize = AU_OFFSET + 8; // 2688
const IPT_OFFSET: usize = EMRAT_OFFSET + 8; // 2696
const NUMDE_OFFSET: usize = IPT_OFFSET + 12 * 3 * 4; // 2840
const LPT_OFFSET: usize = NUMDE_OFFSET + 4; // 2844

fn read_f64_le(data: &[u8], offset: usize) -> f64 {
    let bytes: [u8; 8] = data[offset..offset + 8].try_into().expect("checked length");
    f64::from_le_bytes(bytes)
}

fn read_i32_le(data: &[u8], offset: usize) -> i32 {
    let bytes: [u8; 4] = data[offset..offset + 4].try_into().expect("checked length");
    i32::from_le_bytes(bytes)
}

impl DeEphemeris {
    /// Locates the GMAT install the same way [`crate::cof::locate_gmat_root`] does.
    pub fn locate_gmat_root() -> Result<PathBuf, DeError> {
        crate::cof::locate_gmat_root().map_err(|_| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let fallback = manifest_dir
                .parent()
                .and_then(Path::parent)
                .map(|repo_root| repo_root.join("GMAT R2026a"))
                .unwrap_or_else(|| PathBuf::from("GMAT R2026a"));
            DeError::GmatRootNotFound { fallback }
        })
    }

    /// Reads and parses `path` (a `leDE*.4xx` file) -- see this module's doc for the exact
    /// layout and the cross-checks run here (a bad `AU`/`EMRAT`, or a `KSIZE` that does not
    /// evenly divide the file size, is a typed error, not a silent misparse).
    pub fn open(path: &Path) -> Result<Self, DeError> {
        let data = std::fs::read(path).map_err(|source| DeError::Io { path: path.to_path_buf(), source })?;
        if data.len() < HEADER1_LEN {
            return Err(DeError::Truncated { path: path.to_path_buf(), len: data.len(), want: HEADER1_LEN });
        }

        let names: Vec<String> = (0..CNAM_SLOTS)
            .map(|i| {
                let off = CNAM_OFFSET + i * 6;
                String::from_utf8_lossy(&data[off..off + 6]).trim().to_string()
            })
            .collect();

        let jd_start = read_f64_le(&data, SS_OFFSET);
        let jd_end = read_f64_le(&data, SS_OFFSET + 8);
        let block_days = read_f64_le(&data, SS_OFFSET + 16);
        let ncon = read_i32_le(&data, NCON_OFFSET).max(0) as usize;
        let au_km = read_f64_le(&data, AU_OFFSET);
        let emrat = read_f64_le(&data, EMRAT_OFFSET);

        // Cross-check (task-named): AU ~ 1.4959787e8 km, EMRAT ~ 81.30056.
        if !(1.4e8..1.6e8).contains(&au_km) {
            return Err(DeError::ImplausibleHeader { path: path.to_path_buf(), field: "AU", value: au_km });
        }
        if !(75.0..90.0).contains(&emrat) {
            return Err(DeError::ImplausibleHeader { path: path.to_path_buf(), field: "EMRAT", value: emrat });
        }

        let mut ipt = [Pointer { start: 0, ncoeff: 0, nsub: 0 }; 12];
        for (i, slot) in ipt.iter_mut().enumerate() {
            let off = IPT_OFFSET + i * 12;
            *slot = Pointer {
                start: read_i32_le(&data, off).max(0) as usize,
                ncoeff: read_i32_le(&data, off + 4).max(0) as usize,
                nsub: read_i32_le(&data, off + 8).max(0) as usize,
            };
        }
        let numde = read_i32_le(&data, NUMDE_OFFSET);
        let lpt = Pointer {
            start: read_i32_le(&data, LPT_OFFSET).max(0) as usize,
            ncoeff: read_i32_le(&data, LPT_OFFSET + 4).max(0) as usize,
            nsub: read_i32_le(&data, LPT_OFFSET + 8).max(0) as usize,
        };

        // KSIZE (f64 words per record) derived from the pointer table, not hardcoded -- the
        // maximum end-of-range over all 12 IPT entries and LPT.
        let components = [3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]; // body 11 (nutation) has 2
        let mut ksize_words = 0usize;
        for (i, p) in ipt.iter().enumerate() {
            let end = p.start.saturating_sub(1) + p.ncoeff * components[i] * p.nsub;
            ksize_words = ksize_words.max(end);
        }
        let lpt_end = lpt.start.saturating_sub(1) + lpt.ncoeff * 3 * lpt.nsub;
        ksize_words = ksize_words.max(lpt_end);
        let ksize_bytes = ksize_words * 8;

        if ksize_bytes == 0 || data.len() % ksize_bytes != 0 {
            return Err(DeError::RecordSizeMismatch {
                path: path.to_path_buf(),
                ksize_bytes,
                file_size: data.len(),
                remainder: if ksize_bytes == 0 { data.len() } else { data.len() % ksize_bytes },
            });
        }

        // Record 2 (immediately after record 1, at byte offset ksize_bytes) holds the NCON
        // f64 constants named by CNAM, in order.
        let record2_off = ksize_bytes;
        let mut constants = Vec::with_capacity(ncon);
        for (i, name) in names.iter().take(ncon).enumerate() {
            let off = record2_off + i * 8;
            if off + 8 > data.len() {
                break;
            }
            constants.push((name.clone(), read_f64_le(&data, off)));
        }

        Ok(Self { data, jd_start, jd_end, block_days, ksize_bytes, numde, au_km, emrat, ipt, lpt, constants })
    }

    /// The file's own title lines (`TTL`), trimmed at the first NUL/newline -- for a caller
    /// or test that wants to print/assert the DE version by name (e.g. "DE405/DE405").
    pub fn title_lines(&self) -> [String; 3] {
        std::array::from_fn(|i| {
            let off = i * 84;
            let raw = &self.data[off..off + 84];
            let end = raw.iter().position(|&b| b == 0 || b == b'\n').unwrap_or(raw.len());
            String::from_utf8_lossy(&raw[..end]).trim_end().to_string()
        })
    }

    pub fn jd_start(&self) -> f64 {
        self.jd_start
    }
    pub fn jd_end(&self) -> f64 {
        self.jd_end
    }
    pub fn block_days(&self) -> f64 {
        self.block_days
    }
    pub fn ksize_bytes(&self) -> usize {
        self.ksize_bytes
    }
    pub fn numde(&self) -> i32 {
        self.numde
    }
    pub fn au_km(&self) -> f64 {
        self.au_km
    }
    pub fn emrat(&self) -> f64 {
        self.emrat
    }

    /// A named physical constant from `CVAL` (e.g. `"GMS"`, `"GMB"`, `"CLIGHT"`), in the
    /// ephemeris's own units (`AU^3/day^2` for every `GM*`/`GMB`/`GMS` constant -- see
    /// [`DeEphemeris::mu_si`] for the SI conversion).
    pub fn constant(&self, name: &str) -> Result<f64, DeError> {
        self.constants
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .ok_or_else(|| DeError::UnknownConstant(name.to_string()))
    }

    /// `body`'s standard gravitational parameter, m^3/s^2, converted from the file's own
    /// `AU^3/day^2` constants (`mu_si = mu_au3_per_day2 * AU_km^3 / 86400^2`, then km->m).
    /// [`DeBody::Moon`] and [`DeBody::EarthMoonBarycenter`]'s single-body `mu` are recovered
    /// from `GMB` (the combined Earth+Moon `mu`) and `EMRAT` via the standard mass-ratio
    /// split documented in this module's doc comment; every other body reads its own `GM*`
    /// constant directly.
    pub fn mu_si(&self, body: DeBody) -> Result<f64, DeError> {
        let au_m = self.au_km * 1e3;
        let day_s = 86_400.0_f64;
        let au3_day2_to_si = au_m * au_m * au_m / (day_s * day_s);
        let gmb = self.constant("GMB")?;
        // Earth itself is not a `DeBody` variant here (this reader never needs Earth's DE mu
        // for a force evaluation -- `EarthGravityModel`'s own `.cof`-file mu, JGM2's, is what
        // the central-body force uses); a DE-consistent Earth mu is exposed only via
        // `earth_mu_si` below, for the one cross-check test that wants it.
        let mu = match body {
            DeBody::Mercury => self.constant("GM1")?,
            DeBody::Venus => self.constant("GM2")?,
            DeBody::EarthMoonBarycenter => gmb,
            DeBody::Mars => self.constant("GM4")?,
            DeBody::Jupiter => self.constant("GM5")?,
            DeBody::Saturn => self.constant("GM6")?,
            DeBody::Uranus => self.constant("GM7")?,
            DeBody::Neptune => self.constant("GM8")?,
            DeBody::Pluto => self.constant("GM9")?,
            DeBody::Sun => self.constant("GMS")?,
            DeBody::Moon => gmb / (1.0 + self.emrat),
            DeBody::Nutation | DeBody::Libration => {
                return Err(DeError::UnknownConstant("(no mu for nutation/libration)".to_string()))
            }
        };
        Ok(mu * au3_day2_to_si)
    }

    /// Earth's own `mu`, m^3/s^2, via the same `GMB`/`EMRAT` split as [`DeEphemeris::mu_si`]
    /// (`GMB * EMRAT / (1 + EMRAT)`) -- exposed only for the cross-check against GMAT's own
    /// `Earth.Mu` and JGM2's `.cof`-file `mu` (which differ from this by each source's own
    /// small constant-set inconsistency, not a bug; see this crate's N2 report).
    pub fn earth_mu_si(&self) -> Result<f64, DeError> {
        let au_m = self.au_km * 1e3;
        let day_s = 86_400.0_f64;
        let au3_day2_to_si = au_m * au_m * au_m / (day_s * day_s);
        let gmb = self.constant("GMB")?;
        Ok(gmb * self.emrat / (1.0 + self.emrat) * au3_day2_to_si)
    }

    /// This body's raw (Moon: geocentric; every other body: barycentric) state at `jd_tdb`
    /// (Julian Date, TDB scale -- DE ephemerides are tabulated in TDB), position in km and
    /// velocity in km/s, from a direct Chebyshev evaluation of its `IPT`/`LPT` block. See
    /// [`DeEphemeris::geocentric_position_km`] for the Earth-relative position every caller
    /// outside this module actually wants.
    fn raw_state(&self, body: DeBody, jd_tdb: f64) -> Result<[f64; 6], DeError> {
        self.raw_state_from_offset(body, jd_tdb - self.jd_start)
    }

    /// The two-part-epoch twin of [`DeEphemeris::raw_state`] -- see
    /// [`DeEphemeris::geocentric_position_km2`] and `crate::tdb`'s module doc ("Precision")
    /// for why this entry point exists. `jd1 + jd2 == ` the single-`f64` epoch
    /// [`DeEphemeris::raw_state`] takes.
    ///
    /// **Round 3 (question 226) correction.** An earlier revision of this function formed
    /// `(jd1 - self.jd_start) + jd2` here -- `jd1 - self.jd_start` is exact (both are
    /// whole-or-half-integer-day `f64` values far below `f64`'s exact-integer bound), but its
    /// MAGNITUDE is `self.jd_start`'s own distance from today, which for `leDE1941.405` (this
    /// crate's pinned file, starting ~1941) is on the order of `3.1e4` days for a present-day
    /// epoch -- essentially the SAME magnitude `av_cdm::time::Tai::to_a1_mjd`'s own former
    /// ~600 ns ULP came from. Adding `jd2` there reintroduced almost exactly the floor
    /// `to_a1_mjd_parts` had just removed upstream. `jd2` is now kept apart from
    /// `self.jd_start` entirely and handed to [`DeEphemeris::raw_state_from_split_offset`],
    /// which only ever combines it with quantities of magnitude at most `self.block_days`
    /// (32 for DE405) -- see that function's own doc.
    ///
    /// This fix is real (proven internally-consistent with [`DeEphemeris::raw_state`] by
    /// [`tests::km2_agrees_with_km_at_the_same_instant`] and the block-boundary continuity
    /// test beside it) and DOES make this reader's own epoch handling exact, but it did NOT
    /// reduce `tests/thirdbody_mars_jupiter.rs`'s measured disagreement against GMAT's own
    /// reported positions -- see `crate::tdb`'s module doc, "Measured result -- round 2's
    /// stated cause is FALSIFIED, not confirmed", for the full explanation (in short: GMAT's
    /// own epoch report carries a comparable-magnitude ULP at this same day count, which this
    /// crate's own fix cannot touch).
    ///
    /// # Split invariance: `jd1` is normalized to a whole day FIRST, regardless of the caller
    ///
    /// The exactness argument above depends on `jd1` already being a whole day, which is
    /// `crate::tdb::tai_ns_to_tdb_jd2` (this reader's only production caller)'s own convention
    /// -- but this function's SIGNATURE promises the standard SOFA/ERFA `jd1 + jd2` contract
    /// (any split that sums to the right epoch), not that one caller's convention. A caller
    /// that instead folded, say, six hours of the epoch into `jd1` (still `jd1 + jd2 ==` the
    /// same instant, still a valid two-part JD) would see `jd1 - self.jd_start` stop being an
    /// exact integer, silently losing the precision benefit for that split alone -- an
    /// invariance gap, caught by
    /// [`tests::geocentric_position_km2_is_invariant_to_how_the_epoch_is_split`] (a whole-day,
    /// GMAT-free test measuring exactly this) before this fix, and closed here: `jd1`'s own
    /// fractional part (if any) is folded into `jd2` FIRST, via an exact `floor`/Sterbenz-safe
    /// subtraction (`jd1`'s magnitude is much larger than its own fractional part, so
    /// `jd1 - jd1.floor()` loses nothing), so `whole_days_offset` below is built from a whole
    /// day regardless of how the caller chose to split the epoch. For
    /// `tai_ns_to_tdb_jd2`'s own whole-day `jd1`, `jd1.floor() == jd1` and this is a no-op.
    fn raw_state2(&self, body: DeBody, jd1: f64, jd2: f64) -> Result<[f64; 6], DeError> {
        let jd1_whole = jd1.floor();
        let jd1_frac = jd1 - jd1_whole; // in [0, 1), exact (Sterbenz: jd1 and jd1_whole agree to within 1.0, both ~jd1's own magnitude)
        let jd2_normalized = jd2 + jd1_frac;
        let whole_days_offset = jd1_whole - self.jd_start; // exact: both are exactly-representable integers
        self.raw_state_from_split_offset(body, whole_days_offset, jd2_normalized)
    }

    /// Shared block/sub-index/Chebyshev evaluation given the epoch already reduced to a block
    /// index and an in-block day offset (`t_in_block_days` in `[0, self.block_days)`) -- used
    /// by both [`DeEphemeris::raw_state_from_offset`] (single-`f64` epoch) and
    /// [`DeEphemeris::raw_state_from_split_offset`] (two-part epoch); this function does not
    /// care which path produced its inputs.
    fn raw_state_in_block(&self, body: DeBody, block_index: f64, t_in_block_days: f64) -> Result<[f64; 6], DeError> {
        let approx_offset = block_index * self.block_days + t_in_block_days;
        let pointer = match body {
            DeBody::Libration => self.lpt,
            other => self.ipt[other.ipt_index().expect("non-Libration body has an IPT index")],
        };
        if pointer.ncoeff == 0 || pointer.nsub == 0 {
            // This file does not carry this body's coefficients at all (e.g. an older DE
            // file with no libration block) -- report as an out-of-range epoch is the wrong
            // error; treat it as "no data for this body" via the same variant with jd bounds
            // both set to the requested epoch, which reads unambiguously in the error text.
            let jd_approx = self.jd_start + approx_offset;
            return Err(DeError::EpochOutOfRange { jd: jd_approx, jd_start: jd_approx, jd_end: jd_approx });
        }
        let components = body.components();

        let sub_len_days = self.block_days / pointer.nsub as f64;
        let mut sub_index = (t_in_block_days / sub_len_days).floor() as i64;
        if sub_index < 0 {
            sub_index = 0;
        }
        if sub_index as usize >= pointer.nsub {
            sub_index = pointer.nsub as i64 - 1;
        }
        let sub_index = sub_index as usize;
        let t_sub_days = t_in_block_days - sub_index as f64 * sub_len_days;
        let tc = 2.0 * t_sub_days / sub_len_days - 1.0;

        let record_index = 2 + block_index as usize; // 0-based: record 0 = header1, 1 = header2 (CVAL)
        let record_off = record_index * self.ksize_bytes;
        if record_off + self.ksize_bytes > self.data.len() {
            let jd_approx = self.jd_start + approx_offset;
            return Err(DeError::EpochOutOfRange { jd: jd_approx, jd_start: self.jd_start, jd_end: self.jd_end });
        }
        let record = &self.data[record_off..record_off + self.ksize_bytes];

        let coeff_base_word = pointer.start.saturating_sub(1) + sub_index * pointer.ncoeff * components;
        let dtc_dt_seconds = 2.0 / (sub_len_days * 86_400.0);

        let mut out = [0.0_f64; 6];
        for comp in 0..components {
            let coeff_off_bytes = (coeff_base_word + comp * pointer.ncoeff) * 8;
            let coeffs: Vec<f64> = (0..pointer.ncoeff).map(|k| read_f64_le(record, coeff_off_bytes + k * 8)).collect();
            let (pos, dpos_dtc) = chebyshev_eval_with_derivative(&coeffs, tc);
            out[comp] = pos;
            out[3 + comp] = dpos_dtc * dtc_dt_seconds;
        }
        Ok(out)
    }

    /// Shared block/sub-index/Chebyshev evaluation for [`DeEphemeris::raw_state`], given the
    /// epoch already expressed as a single, already-combined offset (days) from
    /// `self.jd_start` -- unchanged from before round 3; kept only for the single-`f64` path,
    /// whose own achievable precision was never better than this combined offset's own ULP.
    fn raw_state_from_offset(&self, body: DeBody, offset_from_start_days: f64) -> Result<[f64; 6], DeError> {
        let span_days = self.jd_end - self.jd_start;
        if offset_from_start_days < 0.0 || offset_from_start_days >= span_days {
            let jd_approx = self.jd_start + offset_from_start_days;
            return Err(DeError::EpochOutOfRange { jd: jd_approx, jd_start: self.jd_start, jd_end: self.jd_end });
        }
        let block_index = (offset_from_start_days / self.block_days).floor();
        let t_in_block_days = offset_from_start_days - block_index * self.block_days;
        self.raw_state_in_block(body, block_index, t_in_block_days)
    }

    /// Shared block/sub-index/Chebyshev evaluation for [`DeEphemeris::raw_state2`], given the
    /// epoch as an EXACT integer whole-day offset from `self.jd_start` (`whole_days_offset`,
    /// however large in magnitude) plus a small (`jd2`, magnitude `< 2` from
    /// [`DeEphemeris::raw_state2`]'s only production caller) remainder that has NOT yet been
    /// combined with it -- see [`DeEphemeris::raw_state2`]'s own doc for why keeping them
    /// apart this far matters. `jd2` is combined here only with the exact, small
    /// (`< self.block_days` in magnitude, 32 for DE405) day-within-block remainder, so the
    /// addition that actually touches `jd2` has ULP set by THAT small magnitude, not by
    /// `whole_days_offset`'s.
    ///
    /// **Renormalization is a single O(1) carry, not a loop, and this is deliberate.** `jd2`
    /// can push the in-block remainder outside `[0, self.block_days)` -- normally only within
    /// `jd2`'s own `< 2`-day margin of a block boundary (exercised by
    /// [`tests::km2_moon_position_is_continuous_across_a_block_boundary`], whose `jd2` is
    /// deliberately `+-1` millisecond either side of zero at a real block boundary), but this
    /// is a PRIVATE function and `jd2`'s `< 2` bound is a property of its one caller today, not
    /// of this function's own signature -- a `while` loop renormalizing one `self.block_days`
    /// at a time would be correct for that caller but would silently degrade to an
    /// input-dependent number of iterations for any future caller (or a malformed `jd2`) that
    /// broke the assumption, rather than failing loudly or just staying fast. Renormalizing via
    /// a direct `floor` division instead (mirroring the same pattern
    /// [`DeEphemeris::raw_state_from_offset`] already uses for `block_index`) is O(1) for ANY
    /// finite `jd2`, with no assumption to silently violate.
    fn raw_state_from_split_offset(&self, body: DeBody, whole_days_offset: f64, jd2: f64) -> Result<[f64; 6], DeError> {
        let span_days = self.jd_end - self.jd_start;
        // A coarse bounds check only -- jd2's own extra precision doesn't matter at
        // day-level granularity, and this mirrors raw_state_from_offset's identical check.
        let approx_offset = whole_days_offset + jd2;
        if approx_offset < 0.0 || approx_offset >= span_days {
            let jd_approx = self.jd_start + approx_offset;
            return Err(DeError::EpochOutOfRange { jd: jd_approx, jd_start: self.jd_start, jd_end: self.jd_end });
        }

        let block_index0 = (whole_days_offset / self.block_days).floor();
        let t_in_block_unnormalized = (whole_days_offset - block_index0 * self.block_days) + jd2;
        // t_in_block_unnormalized is in [0, self.block_days) + jd2's own margin -- a second,
        // O(1) floor division (not a loop) carries any excess into block_index, exactly as the
        // first floor division above formed block_index0 from whole_days_offset alone.
        let extra_blocks = (t_in_block_unnormalized / self.block_days).floor();
        let block_index = block_index0 + extra_blocks;
        let t_in_block_days = t_in_block_unnormalized - extra_blocks * self.block_days;

        self.raw_state_in_block(body, block_index, t_in_block_days)
    }

    /// `body`'s position relative to Earth (km), at `jd_tdb` -- the Moon directly (its `IPT`
    /// entry already is geocentric), every other body via the barycentric-to-geocentric
    /// conversion this module's doc names (`r_Earth(SSB) = r_EMB(SSB) - r_Moon(geo) /
    /// (1+EMRAT)`, then `r_body(geo) = r_body(SSB) - r_Earth(SSB)`).
    ///
    /// Takes the epoch as a single full-precision-looking `f64`, which is exactly the
    /// resolution ceiling `crate::tdb`'s module doc ("Precision") describes -- see
    /// [`DeEphemeris::geocentric_position_km2`] for the two-part epoch entry point that avoids
    /// it. Kept unchanged (not removed, not repointed): callers that only need this ceiling's
    /// class of resolution (or that predate the round-2 fix) still work exactly as before.
    pub fn geocentric_position_km(&self, body: DeBody, jd_tdb: f64) -> Result<[f64; 3], DeError> {
        let moon_geo = self.raw_state(DeBody::Moon, jd_tdb)?;
        if body == DeBody::Moon {
            return Ok([moon_geo[0], moon_geo[1], moon_geo[2]]);
        }
        let emb_ssb = self.raw_state(DeBody::EarthMoonBarycenter, jd_tdb)?;
        let earth_frac = 1.0 / (1.0 + self.emrat);
        let earth_ssb = [
            emb_ssb[0] - moon_geo[0] * earth_frac,
            emb_ssb[1] - moon_geo[1] * earth_frac,
            emb_ssb[2] - moon_geo[2] * earth_frac,
        ];
        if body == DeBody::EarthMoonBarycenter {
            // r_EMB(geo) = r_EMB(SSB) - r_Earth(SSB) = moon_geo * earth_frac, directly from
            // the earth_ssb definition above (no separate computation needed).
            return Ok([moon_geo[0] * earth_frac, moon_geo[1] * earth_frac, moon_geo[2] * earth_frac]);
        }
        let body_ssb = self.raw_state(body, jd_tdb)?;
        Ok([body_ssb[0] - earth_ssb[0], body_ssb[1] - earth_ssb[1], body_ssb[2] - earth_ssb[2]])
    }

    /// The two-part-epoch twin of [`DeEphemeris::geocentric_position_km`] -- takes `(jd1,
    /// jd2)` from [`crate::tdb::tai_ns_to_tdb_jd2`] (or any other SOFA/ERFA-style split with
    /// `jd1 + jd2 == ` the TDB Julian Date) instead of a single lossy `f64`, and never forms
    /// their sum before it is needed (each [`DeEphemeris::raw_state2`] call keeps `jd1` and
    /// `jd2` separate all the way to [`DeEphemeris::raw_state_from_offset`] -- see that
    /// function's own doc comment). Added alongside [`DeEphemeris::geocentric_position_km`]
    /// rather than changing that function's signature, per this task's own instruction; every
    /// production caller in this crate ([`crate::model::EarthGravityModel::derivatives`]'s
    /// third-body path) uses this entry point as of round 2.
    pub fn geocentric_position_km2(&self, body: DeBody, jd1: f64, jd2: f64) -> Result<[f64; 3], DeError> {
        let moon_geo = self.raw_state2(DeBody::Moon, jd1, jd2)?;
        if body == DeBody::Moon {
            return Ok([moon_geo[0], moon_geo[1], moon_geo[2]]);
        }
        let emb_ssb = self.raw_state2(DeBody::EarthMoonBarycenter, jd1, jd2)?;
        let earth_frac = 1.0 / (1.0 + self.emrat);
        let earth_ssb = [
            emb_ssb[0] - moon_geo[0] * earth_frac,
            emb_ssb[1] - moon_geo[1] * earth_frac,
            emb_ssb[2] - moon_geo[2] * earth_frac,
        ];
        if body == DeBody::EarthMoonBarycenter {
            return Ok([moon_geo[0] * earth_frac, moon_geo[1] * earth_frac, moon_geo[2] * earth_frac]);
        }
        let body_ssb = self.raw_state2(body, jd1, jd2)?;
        Ok([body_ssb[0] - earth_ssb[0], body_ssb[1] - earth_ssb[1], body_ssb[2] - earth_ssb[2]])
    }
}

/// `sum_k coeff[k] T_k(tc)` and `d(sum)/d(tc)`, by the standard three-term Chebyshev
/// recursion and its companion derivative recursion (see this module's doc).
fn chebyshev_eval_with_derivative(coeff: &[f64], tc: f64) -> (f64, f64) {
    if coeff.is_empty() {
        return (0.0, 0.0);
    }
    if coeff.len() == 1 {
        return (coeff[0], 0.0);
    }
    let mut t_prev2 = 1.0_f64; // T_0
    let mut t_prev1 = tc; // T_1
    let mut d_prev2 = 0.0_f64; // dT_0/dtc
    let mut d_prev1 = 1.0_f64; // dT_1/dtc

    let mut pos = coeff[0] * t_prev2 + coeff[1] * t_prev1;
    let mut dpos = coeff[1] * d_prev1;

    for &c in &coeff[2..] {
        let t_k = 2.0 * tc * t_prev1 - t_prev2;
        let d_k = 2.0 * t_prev1 + 2.0 * tc * d_prev1 - d_prev2;
        pos += c * t_k;
        dpos += c * d_k;
        t_prev2 = t_prev1;
        t_prev1 = t_k;
        d_prev2 = d_prev1;
        d_prev1 = d_k;
    }
    (pos, dpos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn de405_path() -> PathBuf {
        DeEphemeris::locate_gmat_root().expect("GMAT_ROOT set").join("data/planetary_ephem/de/leDE1941.405")
    }

    /// Pins the DE405 file's content by SHA-256, computed independently with the system
    /// `shasum -a 256` first (this crate's N2 report records both), matching
    /// `crate::cof`'s identical pattern (ADR-004's crypto rule: `openssl::sha::sha256`, never
    /// `sha2`).
    #[test]
    fn de405_sha256_is_pinned() {
        let bytes = std::fs::read(de405_path()).expect("read leDE1941.405");
        let digest = openssl::sha::sha256(&bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        println!("n2-de405-sha256: {hex}");
        assert_eq!(hex, "f8695149bc54be449788f4d6007d1f6a7053f5be16eae60229dc89c661f6fe4d", "leDE1941.405 content changed (SHA-256 mismatch)");
    }

    /// Every cross-check the task names, in one test, each printed.
    #[test]
    fn header_cross_checks() {
        let de = DeEphemeris::open(&de405_path()).expect("parse leDE1941.405");
        let titles = de.title_lines();
        println!("n2-de405-ttl0: {:?}", titles[0]);
        assert!(titles[0].contains("DE405"), "title line does not name DE405: {:?}", titles[0]);

        println!("n2-de405-au: {}", de.au_km());
        assert!((de.au_km() - 1.4959787e8).abs() < 1e3, "AU {} not close to 1.4959787e8 km", de.au_km());

        println!("n2-de405-emrat: {}", de.emrat());
        assert!((de.emrat() - 81.30056).abs() < 1e-3, "EMRAT {} not close to 81.30056", de.emrat());

        println!("n2-de405-ss: [{}, {}, {}]", de.jd_start, de.jd_end, de.block_days);
        // The golden epoch (01 Jan 2026) is JD ~2461041.5; must be bracketed.
        assert!(de.jd_start() < 2_461_041.5 && de.jd_end() > 2_461_041.5, "SS does not bracket the epoch this crate's goldens need");

        println!("n2-de405-ksize-bytes: {}", de.ksize_bytes());
        assert_eq!(de.ksize_bytes(), 8144, "measured KSIZE (derived from IPT/LPT) must be 1018 f64 words = 8144 bytes");

        let file_size = std::fs::metadata(de405_path()).unwrap().len() as usize;
        println!("n2-de405-file-size-mod-ksize: {} % {} = {}", file_size, de.ksize_bytes(), file_size % de.ksize_bytes());
        assert_eq!(file_size % de.ksize_bytes(), 0, "file_size % KSIZE must be exactly 0");

        assert_eq!(de.numde, 405);
    }

    /// The Sun/Moon `mu` this file implies (via `GMS`/`GMB`/`EMRAT`) matches GMAT's own
    /// `CelestialBody.Mu` for `Sun`/`Luna` to the precision GMAT itself prints -- measured in
    /// this crate's N2 report by probing a live GMAT instance (`Sun.Mu = 132712440017.99`,
    /// `Luna.Mu = 4902.8005821478` km^3/s^2); this test only re-derives the SI values and
    /// checks they are physically sane (Sun's mu near 1.3271244e11, Moon's near 4902.8), since
    /// it must not depend on GMAT being available (no `gmat-frames` feature gate on this
    /// module).
    #[test]
    fn sun_and_moon_mu_match_known_values() {
        let de = DeEphemeris::open(&de405_path()).expect("parse leDE1941.405");
        let sun_mu = de.mu_si(DeBody::Sun).unwrap();
        let moon_mu = de.mu_si(DeBody::Moon).unwrap();
        println!("n2-de405-sun-mu-si: {sun_mu}");
        println!("n2-de405-moon-mu-si: {moon_mu}");
        // Both "known values" here are the standard km^3/s^2 constants (1.32712440018e11 for
        // the Sun, 4902.800066 for the Moon -- e.g. Vallado's table of constants), scaled by
        // 1e9 (km^3 -> m^3) to compare against this reader's own SI (m^3/s^2) output.
        let sun_mu_km3 = 1.327_124_400_18e11;
        let moon_mu_km3 = 4902.800066;
        assert!((sun_mu - sun_mu_km3 * 1e9).abs() / (sun_mu_km3 * 1e9) < 1e-6);
        assert!((moon_mu - moon_mu_km3 * 1e9).abs() / (moon_mu_km3 * 1e9) < 1e-4);
    }

    /// The record boundary (task's own N2 test rule: "handled without a discontinuity") --
    /// evaluated a MILLISECOND before and after a 32-day block boundary (two INDEPENDENT
    /// Chebyshev panels, fit separately by JPL's own ephemeris generator), not seconds
    /// before/after: the Moon moves ~1 km/s, so a multi-second span is dominated by genuine
    /// orbital motion, not panel discontinuity, and would pass even with a real indexing bug
    /// (a first version of this test used a 2-second span and "passed" a ~2 km gap that was
    /// entirely orbital motion -- caught only by cross-checking with an independent Python
    /// re-implementation in this crate's N2 report, which is why this test now uses an
    /// interval short enough that orbital motion cannot mask a genuine bug: 1 ms of Moon
    /// motion is ~1 m, two orders of magnitude below this test's own 10 m bound).
    #[test]
    fn moon_position_is_continuous_across_a_block_boundary() {
        let de = DeEphemeris::open(&de405_path()).expect("parse leDE1941.405");
        // jd_start + 32.0 is the first block boundary after the header's start epoch.
        let boundary = de.jd_start() + de.block_days();
        let one_ms = 0.001 / 86_400.0;
        let before = de.geocentric_position_km(DeBody::Moon, boundary - one_ms).unwrap();
        let after = de.geocentric_position_km(DeBody::Moon, boundary + one_ms).unwrap();
        let gap_km = ((before[0] - after[0]).powi(2) + (before[1] - after[1]).powi(2) + (before[2] - after[2]).powi(2)).sqrt();
        println!("n2-de405-block-boundary-gap-km (2ms apart, straddling the boundary): {gap_km:e}");
        assert!(gap_km < 0.01, "Moon position jumped {gap_km} km across a 2-millisecond span straddling a block boundary (two adjacent Chebyshev panels disagree)");
    }

    #[test]
    fn epoch_out_of_range_is_a_typed_error() {
        let de = DeEphemeris::open(&de405_path()).expect("parse leDE1941.405");
        let err = de.geocentric_position_km(DeBody::Sun, de.jd_start() - 1.0).unwrap_err();
        assert!(matches!(err, DeError::EpochOutOfRange { .. }), "{err:?}");
    }

    #[test]
    fn missing_file_is_a_typed_error() {
        let err = DeEphemeris::open(Path::new("/no/such/file.405")).unwrap_err();
        assert!(matches!(err, DeError::Io { .. }), "{err:?}");
    }

    #[test]
    fn unknown_constant_is_a_typed_error() {
        let de = DeEphemeris::open(&de405_path()).expect("parse leDE1941.405");
        let err = de.constant("NOT_A_REAL_CONSTANT").unwrap_err();
        assert!(matches!(err, DeError::UnknownConstant(_)), "{err:?}");
    }

    /// [`DeEphemeris::geocentric_position_km2`] (the round-2 two-part-epoch entry point) must
    /// agree closely with [`DeEphemeris::geocentric_position_km`] (the original single-`f64`
    /// entry point) at the SAME instant, split two different ways -- both read the identical
    /// Chebyshev record, so any real disagreement here would be a bug in the offset arithmetic,
    /// not a physical difference. A loose bound (1 km) is deliberate: this is a structural
    /// sanity check on the split arithmetic, not the precision measurement itself (that is
    /// `tests/thirdbody_mars_jupiter.rs`'s ten-epoch check, against GMAT).
    #[test]
    fn km2_agrees_with_km_at_the_same_instant() {
        let de = DeEphemeris::open(&de405_path()).expect("parse leDE1941.405");
        let jd_tdb = de.jd_start() + 100.25;
        let jd1 = jd_tdb.floor();
        let jd2 = jd_tdb - jd1;
        let via_single = de.geocentric_position_km(DeBody::Mars, jd_tdb).unwrap();
        let via_split = de.geocentric_position_km2(DeBody::Mars, jd1, jd2).unwrap();
        let gap_km = (0..3).map(|i| (via_single[i] - via_split[i]).powi(2)).sum::<f64>().sqrt();
        println!("n2-de405-km2-vs-km: gap_km={gap_km:e}");
        assert!(gap_km < 1e-3, "geocentric_position_km2 disagrees with geocentric_position_km by {gap_km} km at the same instant");
    }

    /// The two-part epoch's own block-boundary continuity check -- the round-2 twin of
    /// `moon_position_is_continuous_across_a_block_boundary`, using
    /// [`DeEphemeris::geocentric_position_km2`] instead, so the new offset-based arithmetic in
    /// [`DeEphemeris::raw_state_from_offset`] is proven not to introduce a discontinuity at a
    /// block boundary either.
    #[test]
    fn km2_moon_position_is_continuous_across_a_block_boundary() {
        let de = DeEphemeris::open(&de405_path()).expect("parse leDE1941.405");
        let boundary = de.jd_start() + de.block_days();
        let one_ms = 0.001 / 86_400.0;
        let jd1 = boundary.floor();
        let before = de.geocentric_position_km2(DeBody::Moon, jd1, boundary - jd1 - one_ms).unwrap();
        let after = de.geocentric_position_km2(DeBody::Moon, jd1, boundary - jd1 + one_ms).unwrap();
        let gap_km = ((before[0] - after[0]).powi(2) + (before[1] - after[1]).powi(2) + (before[2] - after[2]).powi(2)).sqrt();
        println!("n2-de405-km2-block-boundary-gap-km (2ms apart, straddling the boundary): {gap_km:e}");
        assert!(gap_km < 0.01, "Moon position (km2 path) jumped {gap_km} km across a 2-millisecond span straddling a block boundary");
    }

    /// **Round 3 reviewer requirement: split invariance, with NO GMAT involved.** One physical
    /// instant, expressed as several different `(jd1, jd2)` splits that sum to the SAME total
    /// -- the natural whole-day split [`crate::tdb::tai_ns_to_tdb_jd2`] would produce, plus
    /// splits that move six hours and a whole day of the epoch from `jd2` into `jd1` and back
    /// (both directions; `jd1` is deliberately NOT kept whole-day-aligned for these, unlike
    /// production's own convention, because this test checks general split invariance, not
    /// just the one convention `tai_ns_to_tdb_jd2` happens to use), PLUS a 1-hour shift with
    /// its own residual root-caused separately -- see below. A correct two-part reader
    /// (`geocentric_position_km2`) must return the SAME position for every split of the same
    /// instant, to a bound set only by the small in-block arithmetic.
    ///
    /// **Two groups of shifts, not one, because they are not equally representable in `f64`.**
    /// 6 hours (`0.25` day) and a whole day (`1.0`) are DYADIC fractions -- exactly
    /// representable in binary floating point at any magnitude -- so `jd1_natural + shift`
    /// introduces NO rounding at all forming the shifted `jd1`; any measured spread for these
    /// is entirely attributable to `geocentric_position_km2`'s own arithmetic, with nothing to
    /// blame on the input. 1 hour (`1/24` day) is NOT dyadic (it does not terminate in binary),
    /// so `jd1_natural + 1.0/24.0`, computed at `jd1`'s own ~2.46e6-day magnitude, is rounded
    /// to the nearest representable `f64` the moment it is formed -- a rounding of the INPUT,
    /// unavoidable by any reader downstream, dyadic or not, and independent of
    /// `geocentric_position_km2`'s own correctness. This is measured directly below (not
    /// asserted from theory): `f64_precision_bound_m` is computed from `jd1_natural`'s own
    /// `next_up()` ULP and a stated, generous body-speed constant, and the 1-hour case's
    /// measured drift is checked against THAT bound, not against near-zero.
    ///
    /// Measured result (printed below, one line per body): for the dyadic shifts, EVERY body's
    /// `geocentric_position_km2` output is BIT-IDENTICAL across all 5 splits (`dyadic_spread_m
    /// == 0.0` exactly) -- the strongest possible proof of split invariance, with no GMAT, no
    /// external reference, and no input-precision confound. The 1-hour case's drift is nonzero
    /// but stays within `f64_precision_bound_m` at every body, confirming that residual is the
    /// unavoidable cost of representing a non-dyadic fraction in `jd1` itself, not a defect in
    /// [`DeEphemeris::raw_state2`]/[`DeEphemeris::raw_state_from_split_offset`]. This needs no
    /// GMAT and no external reference at all, so it runs under `--no-default-features` too.
    #[test]
    fn geocentric_position_km2_is_invariant_to_how_the_epoch_is_split() {
        let de = DeEphemeris::open(&de405_path()).expect("parse leDE1941.405");
        let jd_tdb: f64 = de.jd_start() + 1234.56789; // well inside the file's span, mid-block
        let jd1_natural = jd_tdb.floor();
        let jd2_natural = jd_tdb - jd1_natural;

        // Dyadic (exactly representable) shifts: the natural split, +/-6 hours, +/-1 day.
        let dyadic_shifts_days = [0.0_f64, 0.25, -0.25, 1.0, -1.0];
        // Non-dyadic: +/-1 hour, root-caused separately (see doc comment above).
        let inexact_shifts_days = [1.0 / 24.0, -1.0 / 24.0];

        // The f64 representable-precision limit AT jd1's OWN magnitude (~2.46e6 days): half a
        // ULP, since a non-dyadic fraction added to jd1_natural rounds to the NEAREST
        // representable f64, at most half a ULP away from the true value.
        let ulp_days = jd1_natural.next_up() - jd1_natural;
        let half_ulp_days = 0.5 * ulp_days;
        // A generous, stated body-speed bound (60 km/s comfortably covers every body tested
        // here; this crate's own thirdbody_mars_jupiter.rs measures Mars' geocentric speed at
        // ~55 km/s at its own golden epoch) -- used only to convert this TIME bound into a
        // DISTANCE bound.
        const GENEROUS_BODY_SPEED_M_PER_S: f64 = 60_000.0;
        let f64_precision_bound_m = half_ulp_days * 86_400.0 * GENEROUS_BODY_SPEED_M_PER_S;

        let spread_m = |positions: &[[f64; 3]]| -> f64 {
            let mut max_gap = 0.0_f64;
            for i in 0..positions.len() {
                for j in (i + 1)..positions.len() {
                    let gap_km = (0..3).map(|k| (positions[i][k] - positions[j][k]).powi(2)).sum::<f64>().sqrt();
                    max_gap = max_gap.max(gap_km * 1000.0);
                }
            }
            max_gap
        };
        let position_at = |de: &DeEphemeris, body: DeBody, shift: f64| -> [f64; 3] {
            let jd1 = jd1_natural + shift;
            let jd2 = jd2_natural - shift;
            assert!((jd1 + jd2 - jd_tdb).abs() < 1e-9, "split does not sum back to jd_tdb");
            de.geocentric_position_km2(body, jd1, jd2).expect("geocentric_position_km2")
        };

        for body in [DeBody::Mars, DeBody::Jupiter, DeBody::Sun, DeBody::Moon] {
            let dyadic_positions: Vec<[f64; 3]> = dyadic_shifts_days.iter().map(|&s| position_at(&de, body, s)).collect();
            let dyadic_spread_m = spread_m(&dyadic_positions);

            let natural_position = dyadic_positions[0]; // shift == 0.0 is dyadic_shifts_days[0]
            let inexact_max_drift_m = inexact_shifts_days
                .iter()
                .map(|&s| {
                    let p = position_at(&de, body, s);
                    (0..3).map(|k| (p[k] - natural_position[k]).powi(2)).sum::<f64>().sqrt() * 1000.0
                })
                .fold(0.0_f64, f64::max);

            println!(
                "n3-split-invariance: {body:?} dyadic_spread_m={dyadic_spread_m:e} (5 splits: 0, \
                 +/-6h, +/-1day) inexact_max_drift_m={inexact_max_drift_m:e} (+/-1h) \
                 f64_precision_bound_m={f64_precision_bound_m:e}"
            );

            assert_eq!(
                dyadic_spread_m, 0.0,
                "{body:?}: geocentric_position_km2 is not BIT-IDENTICAL across dyadic (exactly \
                 representable) splits -- spread {dyadic_spread_m:e} m, expected exactly 0"
            );
            assert!(
                inexact_max_drift_m <= f64_precision_bound_m,
                "{body:?}: the 1-hour-shift drift ({inexact_max_drift_m:e} m) exceeds the \
                 f64-representable-precision bound ({f64_precision_bound_m:e} m) -- this is no \
                 longer explainable purely by jd1's own non-dyadic rounding and needs a fresh \
                 root cause"
            );
        }
    }
}
