//! `gmat-sys`: drive GMAT's force models from Rust (ADR-002, depth 2).
//!
//! The crate links `libGmatBase` / `libGmatUtil` through a small C shim (`shim/gmatffi.cpp`)
//! and exposes GMAT's `ODEModel::GetDerivatives` as a plain derivative function, so the
//! kernel's own integrator (or any engine's) steps the state while GMAT supplies the
//! validated physics. Units follow GMAT at this boundary: kilometres and km/s, A.1 MJD
//! epochs; the CDM adapter converts to SI metres and TAI (ADR-001).
//!
//! GMAT holds one configuration per process; [`Gmat::setup`] may be called once.
//!
//! ## `av-dynamics` (M2.1)
//!
//! The Dormand-Prince integrator that used to live in this crate's own `integrate` module
//! moved to `av-dynamics` (ADR-002: "the integrator is ours" -- one family shared by every
//! domain and binding, not a GMAT-specific detail). This crate depends on `av-dynamics`
//! (rather than the other way around, and rather than `av-dynamics` re-exporting anything
//! from here) because `gmat-sys` is the one that needs `av-dynamics`'s pieces --
//! [`integrate::Dopri5`] to keep driving `DerivativeModel`, and the `DynamicsModel` trait for
//! [`model::GmatModel`] -- while `av-dynamics` must stay buildable with no GMAT dependency at
//! all; a dependency edge pointing the other way would contradict that. `pub use
//! av_dynamics::integrate;` below re-exports the module under its old path so every existing
//! caller of `gmat_sys::integrate::Dopri5` (this crate's own tests included) needed no
//! changes; the move carried no other source change (verified bit-identical, see the README).
pub use av_dynamics::integrate;
pub mod model;

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::sync::{Mutex, MutexGuard, Once};

/// Process-wide lock over GMAT's engine.
///
/// GMAT holds one configuration per process and is not thread-safe: two threads
/// constructing objects or calling `Initialize()` concurrently abort the process
/// inside GMAT's own code. Rust's test harness runs the tests in one binary on
/// several threads by default, so every test (and any other multi-threaded
/// caller) holds this guard for as long as it touches the engine. The guard is
/// deliberately coarse — GMAT offers no finer granularity to hold.
///
/// Poisoning is ignored: a panicking test leaves GMAT's configuration dirty
/// either way, and turning that into a second failure hides the first.
pub fn engine_lock() -> MutexGuard<'static, ()> {
    static ENGINE: Mutex<()> = Mutex::new(());
    ENGINE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

mod ffi {
    use super::*;
    extern "C" {
        pub fn gmatffi_setup(startup_file: *const c_char) -> c_int;
        pub fn gmatffi_construct(ty: *const c_char, name: *const c_char) -> *mut c_void;
        pub fn gmatffi_set_field_str(obj: *mut c_void, field: *const c_char, value: *const c_char) -> c_int;
        pub fn gmatffi_set_field_real(obj: *mut c_void, field: *const c_char, value: f64) -> c_int;
        pub fn gmatffi_set_field_int(obj: *mut c_void, field: *const c_char, value: c_int) -> c_int;
        pub fn gmatffi_set_reference(obj: *mut c_void, reference: *mut c_void) -> c_int;
        pub fn gmatffi_add_force(fm: *mut c_void, force: *mut c_void) -> c_int;
        pub fn gmatffi_initialize() -> c_int;
        pub fn gmatffi_model_new(fm: *mut c_void, sc: *mut c_void, dim: *mut c_int) -> *mut c_void;
        pub fn gmatffi_model_new_stm(fm: *mut c_void, sc: *mut c_void, dim: *mut c_int) -> *mut c_void;
        pub fn gmatffi_model_state(model: *mut c_void, out: *mut f64, dim: c_int) -> c_int;
        pub fn gmatffi_model_epoch(model: *mut c_void) -> f64;
        pub fn gmatffi_model_derivatives(model: *mut c_void, state: *const f64, dt: f64, out: *mut f64, dim: c_int) -> c_int;
        pub fn gmatffi_model_free(model: *mut c_void);
        pub fn gmatffi_model_spacecraft(model: *mut c_void) -> *mut c_void;
        pub fn gmatffi_get_real_parameter(obj: *mut c_void, name: *const c_char, out: *mut f64) -> c_int;
        pub fn gmatffi_convert_state(epoch_a1mjd: f64, state6: *const f64, from_cs: *const c_char, to_cs: *const c_char, out6: *mut f64) -> c_int;
        pub fn gmatffi_convert_state_and_rotation(
            epoch_a1mjd: f64,
            state6: *const f64,
            from_cs: *const c_char,
            to_cs: *const c_char,
            out6: *mut f64,
            out_r9: *mut f64,
            out_rdot9: *mut f64,
        ) -> c_int;
        pub fn gmatffi_last_error() -> *const c_char;
    }
}

/// An error reported by GMAT through the shim.
#[derive(Debug, Clone)]
pub struct GmatError {
    pub code: i32,
    pub message: String,
}

impl fmt::Display for GmatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GMAT error {}: {}", self.code, self.message)
    }
}
impl std::error::Error for GmatError {}

fn last_error(code: i32) -> GmatError {
    let message = unsafe { CStr::from_ptr(ffi::gmatffi_last_error()) }.to_string_lossy().into_owned();
    GmatError { code, message }
}

fn check(code: c_int) -> Result<(), GmatError> {
    if code == 0 { Ok(()) } else { Err(last_error(code)) }
}

fn cstr(s: &str) -> CString {
    CString::new(s).expect("string with interior NUL")
}

/// The GMAT engine in this process. Constructed once by [`Gmat::setup`].
pub struct Gmat {
    _private: (),
}

static SETUP: Once = Once::new();

impl Gmat {
    /// Load an absolute-path startup file (GMAT's `bin/api_startup_file.txt`). Idempotent.
    pub fn setup(startup_file: &str) -> Result<Gmat, GmatError> {
        let mut result = Ok(());
        let path = cstr(startup_file);
        SETUP.call_once(|| {
            result = check(unsafe { ffi::gmatffi_setup(path.as_ptr()) });
        });
        result.map(|_| Gmat { _private: () })
    }

    /// The startup file inside the GMAT install this crate was built against.
    pub fn default_startup_file() -> String {
        format!("{}/bin/api_startup_file.txt", env!("GMAT_SYS_ROOT"))
    }

    /// `Construct(type, name)`: create a configured GMAT object.
    pub fn construct(&self, ty: &str, name: &str) -> Result<Object, GmatError> {
        let (t, n) = (cstr(ty), cstr(name));
        let p = unsafe { ffi::gmatffi_construct(t.as_ptr(), n.as_ptr()) };
        if p.is_null() { Err(last_error(-1)) } else { Ok(Object { ptr: p }) }
    }

    /// Top-level `Initialize()`: wire coordinate systems, solar system and references.
    pub fn initialize(&self) -> Result<(), GmatError> {
        check(unsafe { ffi::gmatffi_initialize() })
    }

    /// Bind a force model to a spacecraft and prepare `GetDerivatives`.
    pub fn derivative_model(&self, force_model: &Object, spacecraft: &Object) -> Result<DerivativeModel, GmatError> {
        let mut dim: c_int = 0;
        let p = unsafe { ffi::gmatffi_model_new(force_model.ptr, spacecraft.ptr, &mut dim) };
        if p.is_null() {
            return Err(last_error(-1));
        }
        let sc = unsafe { ffi::gmatffi_model_spacecraft(p) };
        Ok(DerivativeModel { ptr: p, dimension: dim as usize, spacecraft: sc })
    }

    /// Bind a force model to a spacecraft, additionally requesting the spacecraft's orbit
    /// State Transition Matrix from the `PropagationStateManager`
    /// (`PropagationStateManager::SetProperty("STM", spacecraft)`, GMAT_API_Cookbook's "STM
    /// and Covariance Propagation" chapter). The returned model's dimension is 42: index 0..6
    /// is the Cartesian state, index `6 + row*6 + col` is STM element `(row, col)`
    /// (row-major; GMAT's own `Spacecraft::GetRmatrixParameter("STM")` agrees with this flat
    /// layout element-for-element, verified against GMAT's Python API in the STM spike). The
    /// initial STM is the identity (`Phi(t0,t0) = I`); `GetDerivatives` fills the 6-state
    /// block exactly as without STM (bit-identical, verified) and additionally fills
    /// `d(Phi)/dt = A(t) Phi` in the STM block, where `A` sums the gravity-gradient (up to
    /// each `GravityField`'s `StmLimit`, default 100, i.e. effectively the full field for
    /// degree/order <= 100) and point-mass contributions of every force in the model that
    /// implements it. ADR-002 amendment (STM spike).
    pub fn derivative_model_with_stm(&self, force_model: &Object, spacecraft: &Object) -> Result<DerivativeModel, GmatError> {
        let mut dim: c_int = 0;
        let p = unsafe { ffi::gmatffi_model_new_stm(force_model.ptr, spacecraft.ptr, &mut dim) };
        if p.is_null() {
            return Err(last_error(-1));
        }
        let sc = unsafe { ffi::gmatffi_model_spacecraft(p) };
        Ok(DerivativeModel { ptr: p, dimension: dim as usize, spacecraft: sc })
    }

    /// Construct (or, if `name` already names a GMAT-configured object -- one of GMAT's own
    /// defaults, e.g. `"EarthMJ2000Eq"`/`"EarthICRF"`, or one this same process already built
    /// under this name -- fetch and reconfigure) a body-axes `CoordinateSystem` named `name`,
    /// with `Origin = body` and `Axes` = the freshly built `axes`-type `AxisSystem` (one of
    /// GMAT's own AxisSystem factory type strings: `"ICRF"`, `"MJ2000Eq"`, `"MJ2000Ec"`,
    /// `"BodyFixed"` -- identical to this repository's own registry axes vocabulary, see
    /// `av-kernel`'s `drm::executor::body_axes_suffix`). Exactly the generic `Construct`/
    /// `SetField`/`SetReference` shim calls `tests/epoch_writeback.rs`'s own "EpochBackEarthFixed"
    /// mirror spacecraft already uses -- no new shim surface needed for construction, only for
    /// [`Gmat::convert`] itself. The caller must still call [`Gmat::initialize`] afterward (once,
    /// after every object this run needs is constructed -- exactly `av-kernel`'s own
    /// `materialize_gmat` convention) before the returned handle's name can be passed to
    /// [`Gmat::convert`] as `from_cs`/`to_cs`.
    pub fn coordinate_system(&self, name: &str, body: &str, axes: &str) -> Result<Object, GmatError> {
        let axis = self.construct(axes, &format!("{name}Axes"))?;
        let cs = self.construct("CoordinateSystem", name)?;
        cs.set_str("Origin", body)?;
        cs.set_reference(&axis)?;
        Ok(cs)
    }

    /// `CoordinateConverter::Convert` (ADR-002's fourth amendment, `docs/open-questions.md`
    /// question 128): `state_km` (`[x,y,z,vx,vy,vz]`, GMAT's native km/km-s, matching every
    /// other state this crate's shim boundary passes -- see [`DerivativeModel::state`]'s own
    /// doc comment) at `epoch_a1mjd`, expressed in the CoordinateSystem named `from_cs`,
    /// converted to the CoordinateSystem named `to_cs`. Both must already be registered in
    /// GMAT's configuration and initialized (typically via [`Gmat::coordinate_system`] followed
    /// by [`Gmat::initialize`], or one of GMAT's own always-initialized defaults) -- a missing
    /// or uninitialized name is `Err(GmatError)`, never a crash and never a silent identity
    /// conversion (`shim/gmatffi.h`'s own doc comment on `gmatffi_convert_state`).
    pub fn convert(&self, epoch_a1mjd: f64, state_km: &[f64; 6], from_cs: &str, to_cs: &str) -> Result<[f64; 6], GmatError> {
        let (from, to) = (cstr(from_cs), cstr(to_cs));
        let mut out = [0.0_f64; 6];
        check(unsafe { ffi::gmatffi_convert_state(epoch_a1mjd, state_km.as_ptr(), from.as_ptr(), to.as_ptr(), out.as_mut_ptr()) })?;
        Ok(out)
    }

    /// M21.4 (`docs/open-questions.md` question 138, ADR-002's fourth amendment): a sibling of
    /// [`Gmat::convert`] that additionally returns the 3x3 rotation matrix and its time
    /// derivative that the *same* `CoordinateConverter::Convert` call computed while performing
    /// this conversion (`shim/gmatffi.h`'s own doc comment on `gmatffi_convert_state_and_
    /// rotation` has the full account, including why this is one call rather than two, and why
    /// it does not imitate GMAT's own `OrbitErrorCovariance` Parameter's block-diagonal
    /// shortcut).
    ///
    /// [`ConvertedStateWithRotation::rotation`]/[`ConvertedStateWithRotation::rotation_dot`] are
    /// row-major 3x3, unitless (never scaled by `state_km`'s km-vs-m choice) -- a caller does
    /// not convert them when applying the resulting 6x6 Jacobian `[[R,0],[Rdot,R]]` to an SI
    /// covariance, only the state/covariance itself needs a unit crossing (see `av-kernel`'s
    /// `drm::executor::convert_gmat_trajectory_to_declared_frame`, which builds and applies that
    /// 6x6 matrix as `P_to = M P_from M^T`).
    pub fn convert_with_rotation(&self, epoch_a1mjd: f64, state_km: &[f64; 6], from_cs: &str, to_cs: &str) -> Result<ConvertedStateWithRotation, GmatError> {
        let (from, to) = (cstr(from_cs), cstr(to_cs));
        let mut out = [0.0_f64; 6];
        let mut r = [0.0_f64; 9];
        let mut rdot = [0.0_f64; 9];
        check(unsafe {
            ffi::gmatffi_convert_state_and_rotation(epoch_a1mjd, state_km.as_ptr(), from.as_ptr(), to.as_ptr(), out.as_mut_ptr(), r.as_mut_ptr(), rdot.as_mut_ptr())
        })?;
        Ok(ConvertedStateWithRotation { state_km: out, rotation: r, rotation_dot: rdot })
    }
}

/// Output of [`Gmat::convert_with_rotation`]. `rotation`/`rotation_dot` are row-major 3x3
/// (`rotation[i*3+j]` is `R(i,j)`) -- see that method's own doc comment for units and the 6x6
/// Jacobian `[[R,0],[Rdot,R]]` these two matrices describe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConvertedStateWithRotation {
    pub state_km: [f64; 6],
    pub rotation: [f64; 9],
    pub rotation_dot: [f64; 9],
}

/// A GMAT object owned by GMAT's configuration manager (never freed from Rust).
pub struct Object {
    ptr: *mut c_void,
}

impl Object {
    pub fn set_str(&self, field: &str, value: &str) -> Result<(), GmatError> {
        let (f, v) = (cstr(field), cstr(value));
        check(unsafe { ffi::gmatffi_set_field_str(self.ptr, f.as_ptr(), v.as_ptr()) })
    }
    pub fn set_real(&self, field: &str, value: f64) -> Result<(), GmatError> {
        let f = cstr(field);
        check(unsafe { ffi::gmatffi_set_field_real(self.ptr, f.as_ptr(), value) })
    }
    pub fn set_int(&self, field: &str, value: i32) -> Result<(), GmatError> {
        let f = cstr(field);
        check(unsafe { ffi::gmatffi_set_field_int(self.ptr, f.as_ptr(), value) })
    }

    /// `GmatBase::SetReference(reference, -1)` -- the same method the Python API's
    /// `obj.SetReference(reference)` calls (see `shim/gmatffi.h`'s doc comment on
    /// `gmatffi_set_reference` for exactly how GMAT routes it to the right `SetRefObject`
    /// overload: by `reference`'s own `GetType()`/`GetName()`, not a type this shim picks).
    /// The motivating case is `DragForce::SetReference(atmosphere_model)` -- without this,
    /// `crates/gmat-sys` had no way to give a `DragForce` its required `AtmosphereModel` and
    /// every such force failed at `Initialize()` with "Atmosphere model not defined"
    /// (`tests/drag_srp_stm.rs`'s shim-gap test).
    pub fn set_reference(&self, reference: &Object) -> Result<(), GmatError> {
        check(unsafe { ffi::gmatffi_set_reference(self.ptr, reference.ptr) })
    }

    /// `ODEModel::AddForce`; `self` must be a ForceModel.
    pub fn add_force(&self, force: &Object) -> Result<(), GmatError> {
        check(unsafe { ffi::gmatffi_add_force(self.ptr, force.ptr) })
    }

    /// `GmatBase::GetRealParameter(name)` on this object directly, through the same
    /// `gmatffi_get_real_parameter` call [`DerivativeModel::real_parameter`] uses on its bound
    /// spacecraft -- exposed here too for a plain `Object` never bound to any
    /// `DerivativeModel`/`ODEModel` (question 105: `tests/epoch_writeback.rs`'s "mirror"
    /// spacecraft, constructed only to read a GMAT-computed value off manually-set fields, is
    /// never propagated and so never has a `DerivativeModel`). See
    /// [`DerivativeModel::real_parameter`]'s own doc comment for the error behaviour (an unknown
    /// `name` is `Err`, never a garbage or silently-zero value).
    pub fn real_parameter(&self, name: &str) -> Result<f64, GmatError> {
        let n = cstr(name);
        let mut out: f64 = 0.0;
        check(unsafe { ffi::gmatffi_get_real_parameter(self.ptr, n.as_ptr(), &mut out) })?;
        Ok(out)
    }

    /// Sets `DryMass` (kg), `Cd`, `Cr`, `DragArea` (m²) and `SRPArea` (m²) in one call --
    /// `self` must be a `Spacecraft`.
    ///
    /// `docs/open-questions.md` question 81, "a seed is a vehicle, not a state vector": a
    /// `Spacecraft` seeded from a Cartesian state without copying these five fields flies
    /// with GMAT's own defaults ([`DEFAULT_DRY_MASS_KG`] 850 kg, [`DEFAULT_DRAG_AREA_M2`] 15
    /// m², [`DEFAULT_SRP_AREA_M2`] 1 m²) instead of the vehicle's actual ones --
    /// `goldens/leo_1day_jgm2_8x8_sunmoon_drag_srp.json`'s own `"reason"` field records the
    /// measured consequence: 467,025.793 m of one-day position error with the defaults,
    /// 0.000 m once the golden generator copied the real properties. This method makes
    /// copying all five the natural one-call thing an adapter does, rather than five
    /// individually easy-to-forget `set_real` calls (every existing caller in this crate's own
    /// tests already sets all five fields by iterating the golden's `spacecraft` map; this is
    /// the typed equivalent for a production adapter building a `Spacecraft` from a CDM
    /// `Entity`/`GaussianState` rather than from a JSON fixture).
    pub fn set_ballistics(&self, props: &SpacecraftBallistics) -> Result<(), GmatError> {
        self.set_real("DryMass", props.dry_mass_kg)?;
        self.set_real("Cd", props.cd)?;
        self.set_real("Cr", props.cr)?;
        self.set_real("DragArea", props.drag_area_m2)?;
        self.set_real("SRPArea", props.srp_area_m2)?;
        Ok(())
    }
}

/// GMAT `Spacecraft` defaults for the fields [`SpacecraftBallistics`] carries, when a caller
/// never sets them (question 81). Recorded here, rather than only in prose, so a caller
/// checking "did I actually override the default" has something concrete to compare against.
pub const DEFAULT_DRY_MASS_KG: f64 = 850.0;
pub const DEFAULT_DRAG_AREA_M2: f64 = 15.0;
pub const DEFAULT_SRP_AREA_M2: f64 = 1.0;

/// The full ballistic-property set a GMAT `Spacecraft` needs to fly with a vehicle's actual
/// mass, drag and SRP properties instead of GMAT's defaults (`DEFAULT_DRY_MASS_KG`,
/// `DEFAULT_DRAG_AREA_M2`, `DEFAULT_SRP_AREA_M2`) -- question 81. See
/// [`Object::set_ballistics`]. Units: kilograms and square metres, GMAT's own native units
/// for these fields regardless of the km/m convention that applies to position and velocity
/// (there is no unit crossing to get wrong here, unlike the state vector).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpacecraftBallistics {
    pub dry_mass_kg: f64,
    pub cd: f64,
    pub cr: f64,
    pub drag_area_m2: f64,
    pub srp_area_m2: f64,
}

/// An ODEModel bound to one propagated object: `f(state, dt) -> state_dot`.
pub struct DerivativeModel {
    ptr: *mut c_void,
    dimension: usize,
    /// The bound spacecraft's `GmatBase*`, owned by GMAT's configuration manager (never freed
    /// from Rust, exactly like `Object::ptr`) -- question 99. Retained so a caller can read the
    /// model's own spacecraft's real parameters ([`DerivativeModel::real_parameter`]) after a
    /// step, without the caller having to keep its own `Object` handle around separately.
    spacecraft: *mut c_void,
}

impl DerivativeModel {
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// The state GMAT holds for the bound object (km, km/s), central-body MJ2000Eq.
    pub fn state(&self) -> Result<Vec<f64>, GmatError> {
        let mut out = vec![0.0; self.dimension];
        check(unsafe { ffi::gmatffi_model_state(self.ptr, out.as_mut_ptr(), self.dimension as c_int) })?;
        Ok(out)
    }

    /// A.1 Modified Julian epoch of that state.
    pub fn epoch_a1mjd(&self) -> f64 {
        unsafe { ffi::gmatffi_model_epoch(self.ptr) }
    }

    /// `state_dot = f(state, epoch + dt_seconds)`, written into `out`.
    pub fn derivatives_into(&self, state: &[f64], dt_seconds: f64, out: &mut [f64]) -> Result<(), GmatError> {
        assert_eq!(state.len(), self.dimension);
        assert_eq!(out.len(), self.dimension);
        check(unsafe {
            ffi::gmatffi_model_derivatives(self.ptr, state.as_ptr(), dt_seconds, out.as_mut_ptr(), self.dimension as c_int)
        })
    }

    pub fn derivatives(&self, state: &[f64], dt_seconds: f64) -> Result<Vec<f64>, GmatError> {
        let mut out = vec![0.0; self.dimension];
        self.derivatives_into(state, dt_seconds, &mut out)?;
        Ok(out)
    }

    /// Write `state_km` (`[x, y, z, vx, vy, vz]`, GMAT's native Cartesian units: km, km/s) into
    /// the bound spacecraft's own `X`/`Y`/`Z`/`VX`/`VY`/`VZ` fields through `GmatBase::SetField`
    /// (the same `gmatffi_set_field_real` path `crate::Object::set_real` already uses to seed a
    /// spacecraft's initial state, e.g. `av-kernel`'s `binding::materialize_gmat`).
    ///
    /// **Why this exists (question 99).** This crate's own [`crate::integrate::Dopri5`] drives
    /// [`DerivativeModel::derivatives`] directly and never calls GMAT's own
    /// `PropagationStateManager::MapVectorToObjects` (the step GMAT's own `Propagator` would
    /// take), so the bound spacecraft's fields stay at whatever they were set to at
    /// construction throughout an entire propagation -- nothing keeps them in sync with the
    /// state this crate is actually integrating. A caller that wants
    /// [`DerivativeModel::real_parameter`] to reflect the *current* propagated state (rather
    /// than the initial one) must call this first with that state, converted to km/km-s.
    pub fn sync_spacecraft_cartesian_km(&self, state_km: &[f64; 6]) -> Result<(), GmatError> {
        const FIELDS: [&str; 6] = ["X", "Y", "Z", "VX", "VY", "VZ"];
        for (field, value) in FIELDS.iter().zip(state_km.iter()) {
            let f = cstr(field);
            check(unsafe { ffi::gmatffi_set_field_real(self.spacecraft, f.as_ptr(), *value) })?;
        }
        Ok(())
    }

    /// `GmatBase::GetRealParameter(name)` on the bound spacecraft (question 99): GMAT's own
    /// named real parameter, read through GMAT's own parameter subsystem -- not a value this
    /// crate derives or re-implements. An unknown `name` is `Err(GmatError)`, never a garbage
    /// or silently-zero value: GMAT's own `GmatBase::GetParameterID` throws a
    /// `GmatBaseException` for a label it does not recognize
    /// (`third_party/gmat-src/src/base/foundation/GmatBase.cpp`), which the shim's `guarded()`
    /// wrapper -- the same one every other call in this crate goes through -- turns into this
    /// crate's usual `Result`, never letting the C++ exception cross the FFI boundary.
    ///
    /// Position-dependent parameters (e.g. `"RMAG"`) reflect whatever state was last written
    /// with [`DerivativeModel::sync_spacecraft_cartesian_km`] (or the spacecraft's state at
    /// construction, if that was never called); parameters that do not depend on propagated
    /// state (e.g. `"Cd"`, `"DryMass"`) reflect whatever the caller configured the spacecraft
    /// with, unaffected by propagation either way. Epoch-dependent parameters (question 105)
    /// additionally need [`DerivativeModel::sync_spacecraft_epoch_a1mjd`] to have been called
    /// with the propagated epoch -- see that method's doc comment.
    pub fn real_parameter(&self, name: &str) -> Result<f64, GmatError> {
        let n = cstr(name);
        let mut out: f64 = 0.0;
        check(unsafe { ffi::gmatffi_get_real_parameter(self.spacecraft, n.as_ptr(), &mut out) })?;
        Ok(out)
    }

    /// Write a single named real field onto the bound spacecraft through `GmatBase::SetField`
    /// -- the same `gmatffi_set_field_real` FFI call [`DerivativeModel::sync_spacecraft_cartesian_km`]
    /// already uses for its own six Cartesian fields, generalized to one caller-named field (M18.3,
    /// `docs/open-questions.md` question 126): a SIGNAL-commanded parameter (e.g. `"Cd"`) applied
    /// mid-run by [`crate::model::GmatModel::step_with_ports`] before that step's own integration,
    /// so the new value is in effect for every sub-step `GetDerivatives` takes within it, exactly
    /// as if the caller had set it at construction. No new shim export needed -- this reuses the
    /// identical FFI call `sync_spacecraft_cartesian_km`/`crate::Object::set_real` already make.
    /// An unknown or read-only `name` surfaces however `GmatBase::SetField` itself reports it
    /// (through the shim's own `guarded()` wrapper, the same `Result` path every other call in
    /// this crate uses), never silently ignored or applied to the wrong field.
    pub fn set_real_parameter(&self, name: &str, value: f64) -> Result<(), GmatError> {
        let f = cstr(name);
        check(unsafe { ffi::gmatffi_set_field_real(self.spacecraft, f.as_ptr(), value) })
    }

    /// Write `epoch_a1mjd` into the bound spacecraft's own `Epoch` field (question 105: through
    /// M12.1, nothing ever wrote the propagated epoch back into the spacecraft after
    /// construction, so any epoch-dependent GMAT parameter read after a step reflected the
    /// spacecraft's *construction* epoch forever -- harmless for `"RMAG"`/`"Cd"`, which do not
    /// depend on epoch at all, but silently stale for anything that does, e.g. an Earth-fixed
    /// longitude).
    ///
    /// Uses `GmatBase::SetField` on the string `"Epoch"` field -- the same mechanism
    /// `av-kernel`'s `binding::materialize_gmat` already uses to seed the spacecraft's *initial*
    /// epoch (see that module's doc comment, question 96): empirically, GMAT's `Epoch` field
    /// takes a string even under the numeric `A1ModJulian` `DateFormat`;
    /// `gmatffi_set_field_real("Epoch", ...)` is refused with "Epoch expects a String". This
    /// method first forces `"DateFormat"` to `"A1ModJulian"` -- unconditionally, not only when
    /// it looks unset -- because `epoch_a1mjd` is always an A1MJD number and a spacecraft built
    /// outside `av-kernel`'s binding (e.g. a caller's own test harness) may have configured a
    /// different `DateFormat` (`UTCGregorian` say) for its own construction convenience;
    /// `Spacecraft::SetDateFormat` only relabels how `"Epoch"` strings are parsed/displayed
    /// (`third_party/gmat-src/src/base/spacecraft/Spacecraft.cpp`: it updates `epochType` and
    /// re-derives the display string from the state's *current* epoch, no `RecomputeStateAtEpoch`
    /// call, no state mutation), so forcing it here is side-effect-free even when it was already
    /// `A1ModJulian`. `epoch_a1mjd.to_string()` is Rust's shortest round-trippable `f64`
    /// formatting, so the string carries the *exact* bit pattern of `epoch_a1mjd` -- no precision
    /// is lost in the string conversion itself. The only approximation is the one already
    /// inherent in converting integer TAI nanoseconds to an `f64` A1MJD in the first place
    /// (`av_cdm::time::Tai::to_a1_mjd`, documented up to 252 ns, question 81) -- this method
    /// does not add to it.
    ///
    /// **Ordering with [`DerivativeModel::sync_spacecraft_cartesian_km`].** For a spacecraft
    /// whose own `CoordinateSystem` field names the same coordinate system GMAT is using
    /// internally (every spacecraft this crate binds into a `DerivativeModel`, e.g.
    /// `EarthMJ2000Eq` in every existing golden), the order does not matter:
    /// `Spacecraft::SetEpoch` calls `RecomputeStateAtEpoch`/`RecomputeStateAtEpochGT`
    /// (`third_party/gmat-src/src/base/spacecraft/Spacecraft.cpp`), which is a documented no-op
    /// ("otherwise, state stays the same") whenever the display and internal coordinate systems
    /// are the same object -- confirmed both by reading that source and empirically (the RMAG/Cd
    /// goldens still match their existing tolerance with this method wired in). **This is not
    /// true in general**: for a spacecraft whose `CoordinateSystem` differs from GMAT's internal
    /// one, `RecomputeStateAtEpoch` *rewrites* the internal Cartesian state to hold the
    /// display-frame representation constant across the epoch change, silently clobbering a
    /// Cartesian state written *before* the epoch. `tests/epoch_writeback.rs` builds exactly one
    /// such spacecraft (a "mirror" with `CoordinateSystem = EarthFixed`, needed to reach a
    /// genuinely epoch-dependent parameter at all -- see that test's doc comment) and always
    /// writes its epoch first for this reason. [`crate::model::GmatModel::step`]/`step_with_stm`
    /// call this method before [`DerivativeModel::sync_spacecraft_cartesian_km`] unconditionally,
    /// even though it is a no-op for their own (matching-`CoordinateSystem`) spacecraft, so the
    /// ordering stays correct if that ever changes.
    pub fn sync_spacecraft_epoch_a1mjd(&self, epoch_a1mjd: f64) -> Result<(), GmatError> {
        let date_format_field = cstr("DateFormat");
        let a1_mod_julian = cstr("A1ModJulian");
        check(unsafe { ffi::gmatffi_set_field_str(self.spacecraft, date_format_field.as_ptr(), a1_mod_julian.as_ptr()) })?;
        let epoch_field = cstr("Epoch");
        let v = cstr(&epoch_a1mjd.to_string());
        check(unsafe { ffi::gmatffi_set_field_str(self.spacecraft, epoch_field.as_ptr(), v.as_ptr()) })
    }
}

impl Drop for DerivativeModel {
    fn drop(&mut self) {
        unsafe { ffi::gmatffi_model_free(self.ptr) }
    }
}

// GMAT's engine is a process-wide singleton and not thread-safe. `Object` and
// `DerivativeModel` hold raw pointers, so they are `!Send` and `!Sync` by construction;
// keep every handle on the thread that called `Gmat::setup`.
