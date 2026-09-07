/* C shim over GMAT's C++ core for the gmat-sys crate (ADR-002 depth 2).
 *
 * Every function returns 0 on success and a negative code on failure; the last error text
 * is available from gmatffi_last_error(). Handles are opaque pointers owned by GMAT's
 * configuration (objects) or by this shim (derivative models). */
#ifndef GMATFFI_H
#define GMATFFI_H

#ifdef __cplusplus
extern "C" {
#endif

typedef void *gmatffi_object;   /* GmatBase* owned by GMAT's configuration manager */
typedef void *gmatffi_model;    /* an ODEModel bound to one propagated object, owned by the shim */

/* Load a GMAT startup file (absolute paths). Call once per process. */
int gmatffi_setup(const char *startup_file);

/* Create a GMAT object of `type` named `name` (empty name allowed). NULL on failure. */
gmatffi_object gmatffi_construct(const char *type, const char *name);

int gmatffi_set_field_str(gmatffi_object obj, const char *field, const char *value);
int gmatffi_set_field_real(gmatffi_object obj, const char *field, double value);
int gmatffi_set_field_int(gmatffi_object obj, const char *field, int value);

/* GmatBase::SetReference(ref, -1) -- calls SetRefObject(ref, ref->GetType(), ref->GetName())
 * under the hood (GmatBase.cpp), i.e. the type/name GMAT uses to route the reference is read
 * off `ref` itself, not chosen by this shim. This is the same GmatBase method the Python API's
 * `obj.SetReference(ref)` calls (SWIG exposes it verbatim; it is not a Python-only helper) --
 * e.g. `dragforce.SetReference(atmosphere_model)`, which DragForce::SetRefObject accepts when
 * `ref->GetType() == Gmat::ATMOSPHERE`. Closes the gap `tests/drag_srp_stm.rs` documented:
 * before this function existed, a DragForce built through this shim had no way to receive its
 * required AtmosphereModel and always failed at Initialize() with "Atmosphere model not
 * defined". Additive; every existing shim function is unchanged. */
int gmatffi_set_reference(gmatffi_object obj, gmatffi_object ref);

/* ODEModel::AddForce(PhysicalModel*). Ownership of the force passes to the model. */
int gmatffi_add_force(gmatffi_object force_model, gmatffi_object force);

/* Top-level API Initialize(): wires objects (coordinate systems, solar system). */
int gmatffi_initialize(void);

/* Bind a force model to a spacecraft through a PropagationStateManager and prepare it for
 * GetDerivatives. `dimension_out` receives the state size (6 for a single spacecraft). */
gmatffi_model gmatffi_model_new(gmatffi_object force_model, gmatffi_object spacecraft, int *dimension_out);

/* Same as gmatffi_model_new, but also requests the spacecraft's orbit State Transition
 * Matrix from the PropagationStateManager (PropagationStateManager::SetProperty("STM", sc))
 * before BuildState(), so the bound model carries the 6-state Cartesian block plus the
 * 36-element STM (row-major: index 6 + row*6 + col), dimension 42. GetDerivatives on the
 * returned model fills d(Phi)/dt = A(t) Phi in the STM block (ADR-002 amendment, STM spike). */
gmatffi_model gmatffi_model_new_stm(gmatffi_object force_model, gmatffi_object spacecraft, int *dimension_out);

/* The model's current state vector (EarthMJ2000Eq-centred on the central body, km, km/s). */
int gmatffi_model_state(gmatffi_model model, double *state_out, int dimension);

/* A.1 Modified Julian epoch of the model's state. */
double gmatffi_model_epoch(gmatffi_model model);

/* state_dot = f(state, epoch + dt_seconds). `state` and `state_dot_out` have `dimension` entries. */
int gmatffi_model_derivatives(gmatffi_model model, const double *state, double dt_seconds,
                              double *state_dot_out, int dimension);

void gmatffi_model_free(gmatffi_model model);

/* The Spacecraft bound to `model` -- the same handle gmatffi_model_new/gmatffi_model_new_stm's
 * `spacecraft` argument was (GmatBase* owned by GMAT's configuration manager, never freed by
 * this shim). Lets a caller read the model's own spacecraft's real parameters
 * (gmatffi_get_real_parameter) after writing the propagated state into it
 * (gmatffi_set_field_real "X"/"Y"/"Z"/"VX"/"VY"/"VZ" -- GMAT's own Cartesian element labels;
 * Spacecraft::SetElement performs whatever representation conversion is needed, the same path
 * GMAT's scripting and object API already go through to seed a spacecraft's state) -- question
 * 99: this crate's own `av_dynamics::integrate::Dopri5` drives GetDerivatives directly and never
 * calls GMAT's PropagationStateManager::MapVectorToObjects, so nothing else keeps the bound
 * Spacecraft's fields in sync with the state this crate is actually propagating. */
gmatffi_object gmatffi_model_spacecraft(gmatffi_model model);

/* GmatBase::GetRealParameter(name): read a named real parameter directly off `obj` (typically a
 * Spacecraft handle from gmatffi_model_spacecraft) through GMAT's own parameter subsystem --
 * question 99. An unknown `name` is a typed failure, not a garbage or silent-zero value:
 * GmatBase::GetParameterID throws GmatBaseException("... has no parameter defined with ...")
 * for a label it does not recognize, and like every other shim function this is caught by
 * guarded() and reported through gmatffi_last_error() -- never lets a C++ exception cross the
 * boundary. `*out` is left unwritten on failure. */
int gmatffi_get_real_parameter(gmatffi_object obj, const char *name, double *out);

/* CoordinateConverter::Convert -- rotate (and, for a body-fixed target, translate/rotate with
 * the body's own angular velocity) a 6-element Cartesian state from `from_cs` to `to_cs` at
 * `epoch_a1mjd` (A.1 Modified Julian, matching every other epoch this shim takes/returns).
 * `state6`/`out6` are km/km-s, GMAT's own native units for a CoordinateSystem state (same
 * convention as gmatffi_model_state) -- `out6` may alias `state6`.
 *
 * `from_cs`/`to_cs` name CoordinateSystem objects that must already be REGISTERED (found by
 * GMAT's own Exists()/GetObject(), i.e. already `gmatffi_construct`ed into GMAT's
 * configuration -- typically via Construct("CoordinateSystem", name) plus Construct(<axis
 * type>, ...)/SetField("Origin", ...)/SetReference(axis) for a name that is not one of GMAT's
 * own defaults, exactly the pattern tests/epoch_writeback.rs's own "EpochBackEarthFixed"
 * mirror spacecraft already uses) AND INITIALIZED (gmatffi_initialize() run since they were
 * configured) -- a name gmat has never heard of, an object that is not a CoordinateSystem, or
 * one that is not yet initialized is a status failure (`gmatffi_last_error()` says which),
 * never a crash and never a silent identity conversion. */
int gmatffi_convert_state(double epoch_a1mjd, const double *state6, const char *from_cs, const char *to_cs, double *out6);

/* M21.4 (`docs/open-questions.md` question 138, ADR-002's fourth amendment): a sibling of
 * gmatffi_convert_state that additionally returns the 3x3 rotation matrix and its time
 * derivative that the SAME `CoordinateConverter::Convert` call computed while doing this
 * conversion (`CoordinateConverter::GetLastRotationMatrix()`/`GetLastRotationDotMatrix()`,
 * called immediately after `Convert` on the same local `CoordinateConverter` instance --
 * never a second `Convert` call, and never a different converter than the one that produced
 * `out6`). Chosen over a second "rotation-only" entry point so a caller rotating a covariance
 * never has to trust that two separate shim calls agree on which conversion they mean (same
 * epoch, same `from_cs`/`to_cs`, same underlying C++ call): there is exactly one `Convert` per
 * sample, whether or not the caller also wants the rotation.
 *
 * `out_r9`/`out_rdot9` are row-major 3x3 (`out_r9[i*3+j]` is `R(i,j)`), unitless and
 * epoch/frame-dependent only -- never scaled by the km-vs-m choice of `state6`/`out6`, so a
 * caller applying the resulting 6x6 Jacobian to an SI (metre) covariance needs no unit
 * conversion of `out_r9`/`out_rdot9` themselves (only of the state, exactly like
 * gmatffi_convert_state already requires).
 *
 * The state map this pair of matrices linearizes is the 6x6 block matrix
 *   M = [[R, 0], [Rdot, R]]
 * i.e. out_pos = R*in_pos (+ a state-independent origin translation `gmatffi_convert_state`'s
 * own `out6` already carries, not part of M) and out_vel = Rdot*in_pos + R*in_vel (+ any
 * origin-velocity term, likewise state-independent) -- the rotation between two frames is
 * generally time-varying (a body-fixed frame rotates), so M is NOT block-diagonal
 * `[[R,0],[0,R]]`; the velocity block gains the `Rdot*in_pos` coupling term. This is exactly
 * the Jacobian GMAT's own `Spacecraft::GetCoordinateSystemTransformMatrix()` builds from these
 * same two matrices (`third_party/gmat-src/src/base/spacecraft/Spacecraft.cpp`), and is what a
 * caller needs to rotate a covariance: `P_to = M P_from M^T`. Contrast GMAT's own
 * `OrbitErrorCovariance` reportable Parameter (`OrbitData::GetCovarianceRmat66`,
 * `third_party/gmat-src/src/base/parameter/OrbitData.cpp`), which builds only the
 * block-diagonal `[[R,0],[0,R]]` transform -- omitting this `Rdot` coupling -- and is
 * therefore NOT a correct reference for a rotating target frame; see this repository's own
 * `goldens/gen_covariance_bodyfixed_leo_2h.py` and `crates/gmat-sys/tests/
 * convert_rotation.rs` for the measured disagreement this causes and why this shim does not
 * imitate it.
 *
 * Same failure contract as gmatffi_convert_state: a missing or uninitialized `from_cs`/`to_cs`
 * is a status failure, never a crash and never silently-identity output. */
int gmatffi_convert_state_and_rotation(double epoch_a1mjd, const double *state6, const char *from_cs, const char *to_cs,
                                        double *out6, double *out_r9, double *out_rdot9);

/* Text of the most recent failure in this thread; empty string if none. */
const char *gmatffi_last_error(void);

#ifdef __cplusplus
}
#endif
#endif
