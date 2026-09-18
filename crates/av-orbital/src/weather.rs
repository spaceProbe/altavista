//! A reader for GMAT's own CSSI space-weather file (`docs/native-dynamics-plan.md` milestone
//! N3, task 3b: "densities from GMAT's own space-weather files"), and the constant-flux
//! configuration [`JacchiaRobertsAtmosphere`](crate::jacchia_roberts) actually needs when a
//! DRM does not want file-based weather.
//!
//! # Which source GMAT's `DragForce` actually uses by default -- read, not assumed
//!
//! Read from `third_party/gmat-src/src/base/forcemodel/DragForce.hpp`/`.cpp` and
//! `third_party/gmat-src/src/base/solarsys/AtmosphereModel.hpp`/`.cpp` (this crate's own task
//! brief: "GMAT's own source ... is the authority for what GMAT actually computes"), then
//! confirmed live (a small probe script under this task's own report, constructing a
//! `DragForce` + `JacchiaRoberts` atmosphere, calling `Initialize()`, and reading back every
//! field named below):
//!
//! `DragForce`'s own constructor defaults `historicWSource`/`predictedWSource` to the literal
//! string `"ConstantFluxAndGeoMag"` (NOT a file), `fluxF107 = 150.0`, `fluxF107A = 150.0`,
//! `kp = 3.0` -- and at `Initialize()` these flow, unconditionally, into
//! `atmos->SetRealParameter(F107ID, fluxF107)` etc. (field names `F107`, `F107A`,
//! `MagneticIndex`, confirmed against `DragForce::PARAMETER_TEXT`) and
//! `atmos->SetInputSource(historicWSource, predictedWSource)`
//! (`AtmosphereModel::SetInputSource`, which maps `"ConstantFluxAndGeoMag"` to
//! `historicalDataSource = 0` / `predictedDataSource = 0` -- the CONSTANT branch).
//! `AtmosphereModel`'s own constructor independently defaults `constantF107`/`nominalF107` to
//! the identical `150.0` / `150.0` / `3.0` triple. **So GMAT's `DragForce`, left at its own
//! defaults, never reads the CSSI file's values at all** -- it validates the file EXISTS at
//! `Initialize()` time (`FileManager::DoesFileExist`, an unconditional check regardless of
//! source -- a missing file is a hard error even in constant mode), but the density
//! computation itself uses the constant F10.7/F10.7A/Kp triple. The live probe confirms this
//! exactly: a freshly constructed `DragForce` + `JacchiaRoberts`, `Initialize()`d with no
//! field set, reports back (via `GetField`/`GetRealParameter`) `HistoricWeatherSource =
//! "ConstantFluxAndGeoMag"`, `PredictedWeatherSource = "ConstantFluxAndGeoMag"`, `F107 =
//! 150.0`, `F107A = 150.0`, `MagneticIndex = 3.0`, `CSSISpaceWeatherFile =
//! "SpaceWeather-All-v1.2.txt"` (the file this module reads).
//!
//! `goldens/leo_1day_jgm2_8x8_sunmoon_drag_srp.json` (the existing M4.3 golden, ADR-002's
//! third amendment) was generated at these SAME defaults -- it never called
//! `SetField("HistoricWeatherSource", ...)` -- so it is, and always was, a constant-flux
//! arc, not a file-driven one; this module's doc corrects no prior claim, only makes explicit
//! what was previously implicit.
//!
//! **GMAT CAN be made to read the file headlessly.** A second live probe set
//! `HistoricWeatherSource = PredictedWeatherSource = "CSSISpaceWeatherFile"` on a fresh
//! `DragForce` + `JacchiaRoberts` and called `Initialize()`: it succeeded (no exception), and
//! `GetDerivativesForSpacecraft` at epoch 01 Jan 2026 (a date in the file's
//! `MONTHLY_PREDICTED`/`MONTHLY_FIT` section, past the `OBSERVED` and `DAILY_PREDICTED`
//! sections' own coverage) produced an acceleration that differs from the constant-mode run
//! at the 5th significant digit -- small at that epoch (the file's Marshall Mean Cycle
//! projection for that date happens to sit near 150), but genuinely nonzero, confirming the
//! file path is actually exercised, not silently falling back to the constant triple.
//!
//! # The file format, and which quantities this reader parses
//!
//! `$GMAT_ROOT/data/atmosphere/earth/SpaceWeather-All-v1.2.txt` (the CSSI/CelesTrak format,
//! `DATATYPE CssiSpaceWeather`, `VERSION 1.2` -- the file's own header names
//! <https://celestrak.org/SpaceData/SpaceWx-format.php>). Four sections, `BEGIN
//! OBSERVED`/`END OBSERVED`, `BEGIN DAILY_PREDICTED`/`END`, `BEGIN
//! MONTHLY_PREDICTED`/`END`, `BEGIN MONTHLY_FIT`/`END`, each holding rows in the IDENTICAL
//! 33-whitespace-field layout (verified directly against the file's own header comment,
//! `FORMAT(I4,I3,I3,I5,I3,8I3,I4,8I4,I4,F4.1,I2,I4,F6.1,I2,5F6.1)`, and by parsing every
//! section with one row parser and checking the field count):
//!
//! ```text
//! yy mm dd BSRN ND Kp1..Kp8 SumKp Ap1..Ap8 AvgAp Cp C9 ISN F10.7adj Q Ctr81adj Lst81adj F10.7obs Ctr81obs Lst81obs
//! ```
//!
//! This reader parses: the calendar date (`yy mm dd`, four-digit year), the eight 3-hourly
//! `Kp` values (`Kp*10` in the file, so this reader's own [`SpaceWeatherRecord::kp`] divides
//! by 10 to the standard 0-9 thirds scale) and the eight corresponding `Ap` values (already
//! integer amplitudes, no scaling), and the three F10.7-family columns GMAT's atmospheres
//! consume: `F10.7obs` (the observed daily flux, NOT the 1 AU-adjusted `F10.7adj` column --
//! `JacchiaRobertsAtmosphere.cpp`'s own `fD.obsF107`/`fD.obsCtrF107a` variable names, read off
//! `SolarFluxReader::GetInputs`'s result, name the OBSERVED columns, and `AtmosphereModel`'s
//! constant-mode fallback fields are likewise named `constantF107`/`nominalF107` with no
//! "adjusted" qualifier) and `Ctr81obs` (the 81-day-CENTRED running average of `F10.7obs`,
//! GMAT's own `F107A` -- confirmed against `AtmosphereModel::nominalF107a`'s doc comment, "3
//! month average of the F10.7 data", which is exactly what an 81-day centred window is).
//! `Lst81obs` (the trailing, non-centred 81-day average) is parsed but not used by this
//! crate's own atmosphere code -- kept on [`SpaceWeatherRecord`] for a future consumer, not
//! dead-ended.
//!
//! # What this reader could NOT verify (named precisely, per this task's own rule)
//!
//! `SolarFluxReader.cpp`/`.hpp` -- the GMAT class that actually implements
//! `LoadFluxData`/`GetInputs`/`PrepareKpData`, i.e. exactly WHICH of a day's eight 3-hourly
//! `Kp` values `JacchiaRobertsAtmosphere::JacchiaRoberts`'s `geo.tkp = fD.kp[0]` resolves to
//! for an epoch that falls partway through a UTC day, and whether GMAT applies any
//! processing-latency lag when selecting a day's `F10.7`/`Kp` record for a given epoch (the
//! literature convention this task names, "the previous day's observed value") -- **is not
//! present in this repository's mirrored `third_party/gmat-src` tree** (`find
//! third_party/gmat-src -iname "*SolarFlux*"` returns nothing). This reader therefore exposes
//! the FULL record for a calendar day (all eight `Kp`/`Ap` values, never collapsed to one)
//! and leaves day/slot selection to the caller, documented on
//! [`SpaceWeatherFile::record_for_date`] and [`SpaceWeatherFile::first_kp_for_date`] rather
//! than guessed. [`crate::jacchia_roberts`]'s own doc comment states exactly which convention
//! this crate's goldens use (GMAT's own CONSTANT triple, matched bit-for-bit against the
//! verified default above) and reports, as a measurement rather than a claim, how well a
//! same-calendar-day, first-Kp-slot reading of this file agrees with GMAT's own file-mode
//! `GetDerivatives` at one epoch -- see that module's own report section.
//!
//! # Content pinning and typed errors
//!
//! [`SpaceWeatherFile::open`] hashes the file's own bytes with `openssl::sha::sha256`
//! (ADR-004's crypto rule, `crate::cof`'s own identical pattern) and every generator this
//! task adds records that hash in its golden (N3's own explicit requirement). Every failure
//! mode -- a missing file, non-UTF-8 content, a malformed row, an unknown section keyword, a
//! date with no matching record -- is a typed [`WeatherError`], never a panic or an `unwrap`
//! on file content (this crate's own rule, `crate::cof`'s and `crate::de`'s identical
//! precedent). GMAT-free: builds and unit-tests under `cargo test -p av-orbital
//! --no-default-features` (no `gmat-sys` dependency anywhere in this module).

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// Every way reading a CSSI space-weather file can fail. No variant is reached by a panic or
/// an `unwrap` on file content.
#[derive(Debug, thiserror::Error)]
pub enum WeatherError {
    /// `GMAT_ROOT` was not set and the repo-relative fallback does not exist either --
    /// mirrors [`crate::cof::CofError::GmatRootNotFound`] exactly.
    #[error(
        "GMAT_ROOT is not set and the fallback GMAT install was not found at {fallback}; \
         set GMAT_ROOT to a GMAT R2026a install (contains data/, bin/)"
    )]
    GmatRootNotFound {
        /// The fallback path that was checked.
        fallback: PathBuf,
    },
    /// The file could not be opened or read.
    #[error("could not read space-weather file {path}: {source}")]
    Io {
        /// The file that could not be read.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file's bytes are not valid UTF-8.
    #[error("space-weather file {path} is not valid UTF-8 text")]
    Encoding {
        /// The file that failed to decode.
        path: PathBuf,
    },
    /// A data row (inside a `BEGIN .../END ...` section) did not have the 33 whitespace
    /// fields this reader's own module doc names, or one of those fields did not parse as
    /// the expected integer or float.
    #[error("space-weather file {path}, line {line}: malformed data row ({reason})")]
    MalformedRow {
        /// The file containing the bad row.
        path: PathBuf,
        /// The 1-based line number of the bad row.
        line: usize,
        /// What was wrong with it.
        reason: String,
    },
    /// No record for the requested calendar date exists in any section of the file.
    #[error("space-weather file {path} has no record for {year:04}-{month:02}-{day:02}")]
    DateNotFound {
        /// The file that was searched.
        path: PathBuf,
        year: i32,
        month: u32,
        day: u32,
    },
}

/// One day's record from the CSSI file -- see this module's own doc comment for exactly
/// which columns feed which field, and in what units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpaceWeatherRecord {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    /// The eight 3-hourly planetary `Kp` indices for this UTC day, standard 0-9 thirds scale
    /// (the file's own `Kp*10` integer columns, divided by 10 here).
    pub kp: [f64; 8],
    /// The eight corresponding 3-hourly planetary `Ap` amplitudes (already the file's own
    /// units, no scaling).
    pub ap: [f64; 8],
    /// Observed daily F10.7 solar flux (10^-22 W/m^2/Hz), the file's `F10.7obs` column --
    /// GMAT's own `nominalF107`/`constantF107` in constant mode.
    pub f107_obs: f64,
    /// Observed 81-day CENTRED running average of F10.7, the file's `Ctr81obs` column --
    /// GMAT's own `nominalF107a`/`constantF107a`.
    pub f107_obs_ctr81: f64,
    /// Observed 81-day TRAILING (non-centred) running average of F10.7, the file's
    /// `Lst81obs` column -- parsed but not consumed by this crate's own atmosphere code (see
    /// this module's own doc comment).
    pub f107_obs_lst81: f64,
}

impl SpaceWeatherRecord {
    /// The first of this day's eight 3-hourly `Kp` values -- the slot
    /// `JacchiaRobertsAtmosphere.cpp`'s own `geo.tkp = fD.kp[0]` reads in file mode, AS FAR AS
    /// THIS READER CAN CONFIRM WITHOUT `SolarFluxReader.cpp`'s source (see this module's own
    /// doc comment, "What this reader could NOT verify") -- i.e. this is a documented
    /// approximation of GMAT's own per-epoch slot selection, not a verified reproduction of
    /// it, and callers that need the exact GMAT convention should treat this as a starting
    /// point to measure against, not an assumed ground truth.
    pub fn first_kp(&self) -> f64 {
        self.kp[0]
    }
}

/// Which section of the file a record was read from (observed history, a daily-resolution
/// prediction, or a monthly-resolution prediction/fit) -- carried alongside each record so a
/// caller can tell how far from real observation a given day's numbers are, never silently
/// treating a monthly-repeated projection as an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherSection {
    Observed,
    DailyPredicted,
    MonthlyPredicted,
    MonthlyFit,
}

impl fmt::Display for WeatherSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            WeatherSection::Observed => "OBSERVED",
            WeatherSection::DailyPredicted => "DAILY_PREDICTED",
            WeatherSection::MonthlyPredicted => "MONTHLY_PREDICTED",
            WeatherSection::MonthlyFit => "MONTHLY_FIT",
        };
        f.write_str(s)
    }
}

#[derive(Debug)]
struct SectionedRecord {
    section: WeatherSection,
    record: SpaceWeatherRecord,
}

/// A parsed CSSI space-weather file, indexed by calendar date, plus the file's own SHA-256
/// digest (ADR-004's crypto rule; N3's own explicit requirement that every generator records
/// this hash).
#[derive(Debug)]
pub struct SpaceWeatherFile {
    file_name: String,
    sha256: String,
    records: Vec<SectionedRecord>,
}

fn parse_field<T: std::str::FromStr>(fields: &[&str], idx: usize, path: &Path, line: usize, name: &str) -> Result<T, WeatherError> {
    fields
        .get(idx)
        .ok_or_else(|| WeatherError::MalformedRow { path: path.to_path_buf(), line, reason: format!("missing field {name} (index {idx})") })?
        .parse::<T>()
        .map_err(|_| WeatherError::MalformedRow { path: path.to_path_buf(), line, reason: format!("field {name} (index {idx}) did not parse") })
}

fn parse_row(line_no: usize, line: &str, path: &Path) -> Result<SpaceWeatherRecord, WeatherError> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() != 33 {
        return Err(WeatherError::MalformedRow { path: path.to_path_buf(), line: line_no, reason: format!("expected 33 whitespace-separated fields, got {}", fields.len()) });
    }
    let year: i32 = parse_field(&fields, 0, path, line_no, "yy")?;
    let month: u32 = parse_field(&fields, 1, path, line_no, "mm")?;
    let day: u32 = parse_field(&fields, 2, path, line_no, "dd")?;
    let mut kp = [0.0_f64; 8];
    for (i, kp_i) in kp.iter_mut().enumerate() {
        let raw: f64 = parse_field(&fields, 5 + i, path, line_no, "Kp")?;
        *kp_i = raw / 10.0;
    }
    let mut ap = [0.0_f64; 8];
    for (i, ap_i) in ap.iter_mut().enumerate() {
        *ap_i = parse_field(&fields, 14 + i, path, line_no, "Ap")?;
    }
    let f107_obs: f64 = parse_field(&fields, 30, path, line_no, "F10.7obs")?;
    let f107_obs_ctr81: f64 = parse_field(&fields, 31, path, line_no, "Ctr81obs")?;
    let f107_obs_lst81: f64 = parse_field(&fields, 32, path, line_no, "Lst81obs")?;
    Ok(SpaceWeatherRecord { year, month, day, kp, ap, f107_obs, f107_obs_ctr81, f107_obs_lst81 })
}

impl SpaceWeatherFile {
    /// Reads and parses `path` (a CSSI-format space-weather file, e.g.
    /// `$GMAT_ROOT/data/atmosphere/earth/SpaceWeather-All-v1.2.txt`), hashing its bytes with
    /// `openssl::sha::sha256` before any parsing (so the SHA-256 pins EXACTLY the bytes that
    /// were parsed, not a separately-read copy).
    pub fn open(path: &Path) -> Result<Self, WeatherError> {
        let bytes = std::fs::read(path).map_err(|source| WeatherError::Io { path: path.to_path_buf(), source })?;
        let digest = openssl::sha::sha256(&bytes);
        let mut sha256 = String::with_capacity(64);
        for b in digest {
            sha256.push_str(&format!("{b:02x}"));
        }
        let text = String::from_utf8(bytes).map_err(|_| WeatherError::Encoding { path: path.to_path_buf() })?;

        let mut records = Vec::new();
        let mut section: Option<WeatherSection> = None;
        for (i, raw_line) in text.lines().enumerate() {
            let line_no = i + 1;
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("BEGIN ") {
                section = match rest.trim() {
                    "OBSERVED" => Some(WeatherSection::Observed),
                    "DAILY_PREDICTED" => Some(WeatherSection::DailyPredicted),
                    "MONTHLY_PREDICTED" => Some(WeatherSection::MonthlyPredicted),
                    "MONTHLY_FIT" => Some(WeatherSection::MonthlyFit),
                    _ => None, // an unrecognised section is simply not indexed (forward-compatible)
                };
                continue;
            }
            if line.starts_with("END ") {
                section = None;
                continue;
            }
            if line.starts_with("DATATYPE") || line.starts_with("VERSION") || line.starts_with("UPDATED") || line.starts_with("NUM_") {
                continue;
            }
            if let Some(sec) = section {
                let record = parse_row(line_no, line, path)?;
                records.push(SectionedRecord { section: sec, record });
            }
        }

        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        Ok(Self { file_name, sha256, records })
    }

    /// The file's own name (e.g. `"SpaceWeather-All-v1.2.txt"`) -- for a caller building a
    /// golden's own recorded fields.
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// The file's SHA-256 digest, lowercase hex (ADR-004's crypto rule; N3's own explicit
    /// requirement that every generator records this).
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The record for the given UTC calendar date, and which section it came from, across
    /// every section (observed, daily-predicted, monthly-predicted, monthly-fit) -- whichever
    /// section actually names that exact `(year, month, day)` (the sections do not overlap in
    /// practice, since each covers a disjoint date range, but this searches all of them rather
    /// than assuming which one a given date falls in).
    pub fn record_for_date(&self, year: i32, month: u32, day: u32) -> Result<(WeatherSection, &SpaceWeatherRecord), WeatherError> {
        self.records
            .iter()
            .find(|r| r.record.year == year && r.record.month == month && r.record.day == day)
            .map(|r| (r.section, &r.record))
            .ok_or_else(|| WeatherError::DateNotFound { path: PathBuf::from(&self.file_name), year, month, day })
    }

    /// Convenience: [`SpaceWeatherRecord::first_kp`] for the given date, via
    /// [`SpaceWeatherFile::record_for_date`] -- see that method's own doc comment for exactly
    /// what "first" means and its documented uncertainty against GMAT's own selection.
    pub fn first_kp_for_date(&self, year: i32, month: u32, day: u32) -> Result<f64, WeatherError> {
        self.record_for_date(year, month, day).map(|(_, r)| r.first_kp())
    }

    /// Total records parsed, across every section -- for a test asserting the file was
    /// actually read, not silently empty.
    pub fn record_count(&self) -> usize {
        self.records.len()
    }
}

/// Locates the GMAT install the same way [`crate::cof::locate_gmat_root`] does -- see that
/// function's own doc comment.
pub fn locate_gmat_root() -> Result<PathBuf, WeatherError> {
    if let Ok(from_env) = env::var("GMAT_ROOT") {
        return Ok(PathBuf::from(from_env));
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fallback = manifest_dir.parent().and_then(Path::parent).map(|repo_root| repo_root.join("GMAT R2026a")).unwrap_or_else(|| PathBuf::from("GMAT R2026a"));
    if fallback.is_dir() {
        Ok(fallback)
    } else {
        Err(WeatherError::GmatRootNotFound { fallback })
    }
}

/// GMAT's own `DragForce`/`AtmosphereModel` CONSTANT-flux defaults -- see this module's own
/// doc comment for exactly where each was read (`DragForce`'s own C++ constructor, confirmed
/// live). A DRM that does not configure weather explicitly gets these, bit-for-bit what a
/// freshly-constructed GMAT `DragForce` uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConstantWeather {
    pub f107: f64,
    pub f107a: f64,
    pub kp: f64,
}

/// `F107 = 150.0`, `F107A = 150.0`, `MagneticIndex (Kp) = 3.0` -- `DragForce::DragForce`'s own
/// member-initializer defaults, `third_party/gmat-src/src/base/forcemodel/DragForce.cpp`,
/// confirmed live (see this module's own doc comment).
pub const GMAT_DRAG_FORCE_DEFAULT_F107: f64 = 150.0;
pub const GMAT_DRAG_FORCE_DEFAULT_F107A: f64 = 150.0;
pub const GMAT_DRAG_FORCE_DEFAULT_KP: f64 = 3.0;

impl ConstantWeather {
    /// GMAT's own `DragForce` defaults -- see this module's own doc comment and the
    /// `GMAT_DRAG_FORCE_DEFAULT_*` constants above.
    pub fn gmat_defaults() -> Self {
        Self { f107: GMAT_DRAG_FORCE_DEFAULT_F107, f107a: GMAT_DRAG_FORCE_DEFAULT_F107A, kp: GMAT_DRAG_FORCE_DEFAULT_KP }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_path() -> PathBuf {
        locate_gmat_root().expect("GMAT_ROOT set (this task's own environment rule)").join("data/atmosphere/earth/SpaceWeather-All-v1.2.txt")
    }

    #[test]
    fn opens_and_hashes_the_real_file() {
        let f = SpaceWeatherFile::open(&file_path()).expect("open CSSI file");
        assert_eq!(f.file_name(), "SpaceWeather-All-v1.2.txt");
        assert_eq!(f.sha256().len(), 64, "hex-encoded SHA-256 is 64 chars");
        assert!(f.record_count() > 20_000, "expected tens of thousands of records, got {}", f.record_count());
        println!("n3-weather-sha256: {}", f.sha256());
    }

    /// Pins the CSSI file's own SHA-256, mirroring `crate::de`'s `de405_sha256_is_pinned` and
    /// `crate::cof`'s identical pattern (ADR-004's crypto rule) -- measured by this same
    /// `openssl::sha::sha256` call and cross-checked against `shasum -a 256` on this host
    /// (this task's own report records both).
    #[test]
    fn cssi_weather_file_sha256_is_pinned() {
        let f = SpaceWeatherFile::open(&file_path()).expect("open CSSI file");
        println!("n3-weather-sha256-pinned: {}", f.sha256());
        assert_eq!(f.sha256(), "47aef46938f4b639965cd73d9406c9580f4042fbd938467b40a26ff0d4b354cf", "SpaceWeather-All-v1.2.txt content changed (SHA-256 mismatch)");
    }

    /// The very first row of `BEGIN OBSERVED` in the file (checked by hand against the raw
    /// text this module's own doc comment quotes): `1957 10 01 1700 19 43 40 30 20 37 23 43
    /// 37 273 32 27 15 7 22 9 32 22 21 1.1 5 334 269.8 0 266.8 235.5 269.3 266.6 230.9`.
    #[test]
    fn parses_the_first_observed_row_correctly() {
        let f = SpaceWeatherFile::open(&file_path()).expect("open CSSI file");
        let (section, r) = f.record_for_date(1957, 10, 1).expect("1957-10-01 record");
        assert_eq!(section, WeatherSection::Observed);
        assert_eq!(r.year, 1957);
        assert_eq!(r.month, 10);
        assert_eq!(r.day, 1);
        assert_eq!(r.kp, [4.3, 4.0, 3.0, 2.0, 3.7, 2.3, 4.3, 3.7]);
        assert_eq!(r.ap, [32.0, 27.0, 15.0, 7.0, 22.0, 9.0, 32.0, 22.0]);
        assert_eq!(r.f107_obs, 269.3);
        assert_eq!(r.f107_obs_ctr81, 266.6);
        assert_eq!(r.f107_obs_lst81, 230.9);
        assert_eq!(r.first_kp(), 4.3);
    }

    #[test]
    fn a_date_with_no_record_is_a_typed_error_not_a_panic() {
        let f = SpaceWeatherFile::open(&file_path()).expect("open CSSI file");
        let err = f.record_for_date(1800, 1, 1).unwrap_err();
        assert!(matches!(err, WeatherError::DateNotFound { .. }));
    }

    #[test]
    fn daily_predicted_section_is_distinguished_from_observed() {
        let f = SpaceWeatherFile::open(&file_path()).expect("open CSSI file");
        let (section, r) = f.record_for_date(2025, 3, 21).expect("2025-03-21 record (this file's own DAILY_PREDICTED start)");
        assert_eq!(section, WeatherSection::DailyPredicted);
        assert_eq!(r.f107_obs, 171.3);
    }

    #[test]
    fn a_missing_file_is_a_typed_io_error_not_a_panic() {
        let err = SpaceWeatherFile::open(Path::new("/nonexistent/SpaceWeather-Nope.txt")).unwrap_err();
        assert!(matches!(err, WeatherError::Io { .. }));
    }

    #[test]
    fn gmat_constant_defaults_match_the_source_read() {
        let c = ConstantWeather::gmat_defaults();
        assert_eq!(c.f107, 150.0);
        assert_eq!(c.f107a, 150.0);
        assert_eq!(c.kp, 3.0);
    }
}
