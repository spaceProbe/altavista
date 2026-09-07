// C shim over GMAT's C++ core (ADR-002 depth 2). Mirrors api/Ex_R2020a_BasicForceModel.py.
#include "gmatffi.h"

#include <cstring>
#include <string>
#include <vector>

#include "APIFunctions.hpp"
#include "BaseException.hpp"
#include "CoordinateConverter.hpp"
#include "CoordinateSystem.hpp"
#include "GmatBase.hpp"
#include "GmatState.hpp"
#include "ODEModel.hpp"
#include "PhysicalModel.hpp"
#include "PropagationStateManager.hpp"
#include "Rmatrix33.hpp"

namespace {

thread_local std::string g_last_error;

void set_error(const std::string &msg) { g_last_error = msg; }

template <typename F>
int guarded(F &&f) {
    try {
        f();
        g_last_error.clear();
        return 0;
    } catch (BaseException &e) {
        set_error(e.GetFullMessage());
        return -1;
    } catch (std::exception &e) {
        set_error(e.what());
        return -2;
    } catch (...) {
        set_error("unknown C++ exception");
        return -3;
    }
}

struct Model {
    ODEModel *ode = nullptr;                 // owned by GMAT's configuration
    PropagationStateManager *psm = nullptr;  // owned here
    GmatBase *spacecraft = nullptr;          // owned by GMAT's configuration (question 99)
    int dimension = 0;
    std::vector<double> scratch;
};

}  // namespace

extern "C" {

int gmatffi_setup(const char *startup_file) {
    return guarded([&] { Setup(std::string(startup_file)); });
}

gmatffi_object gmatffi_construct(const char *type, const char *name) {
    GmatBase *obj = nullptr;
    int rc = guarded([&] { obj = Construct(std::string(type), std::string(name ? name : "")); });
    if (rc != 0) return nullptr;
    if (obj == nullptr) set_error(std::string("Construct returned null for type ") + type);
    return obj;
}

int gmatffi_set_field_str(gmatffi_object obj, const char *field, const char *value) {
    return guarded([&] { static_cast<GmatBase *>(obj)->SetField(std::string(field), std::string(value)); });
}

int gmatffi_set_field_real(gmatffi_object obj, const char *field, double value) {
    return guarded([&] { static_cast<GmatBase *>(obj)->SetField(std::string(field), (Real)value); });
}

int gmatffi_set_field_int(gmatffi_object obj, const char *field, int value) {
    return guarded([&] { static_cast<GmatBase *>(obj)->SetField(std::string(field), (Integer)value); });
}

int gmatffi_set_reference(gmatffi_object obj, gmatffi_object ref) {
    return guarded([&] {
        GmatBase *o = static_cast<GmatBase *>(obj);
        GmatBase *r = static_cast<GmatBase *>(ref);
        if (!o->SetReference(r)) throw std::runtime_error("GmatBase::SetReference returned false");
    });
}

int gmatffi_add_force(gmatffi_object force_model, gmatffi_object force) {
    return guarded([&] {
        ODEModel *fm = dynamic_cast<ODEModel *>(static_cast<GmatBase *>(force_model));
        PhysicalModel *pm = dynamic_cast<PhysicalModel *>(static_cast<GmatBase *>(force));
        if (!fm) throw std::runtime_error("force_model is not an ODEModel");
        if (!pm) throw std::runtime_error("force is not a PhysicalModel");
        fm->AddForce(pm);
    });
}

int gmatffi_initialize(void) {
    return guarded([&] { Initialize(""); });
}

namespace {

// Shared by gmatffi_model_new and gmatffi_model_new_stm (ADR-002 amendment, STM spike):
// bind force_model/spacecraft through a fresh PropagationStateManager. When with_stm is set,
// PropagationStateManager::SetProperty("STM", spacecraft) is called before BuildState() --
// exactly api/docs' GMAT_API_Cookbook "STM and Covariance Propagation" chapter's
// `psm.SetProperty("STM", sat)` -- so BuildState() sizes the state at 42 instead of 6 and
// BuildModelFromMap() sets fillSTM on the ODEModel and each PhysicalModel that supports it.
Model *build_model(gmatffi_object force_model, gmatffi_object spacecraft, bool with_stm) {
    Model *m = new Model();
    int rc = guarded([&] {
        m->ode = dynamic_cast<ODEModel *>(static_cast<GmatBase *>(force_model));
        if (!m->ode) throw std::runtime_error("force_model is not an ODEModel");
        m->spacecraft = static_cast<GmatBase *>(spacecraft);
        m->psm = new PropagationStateManager();
        m->psm->SetObject(static_cast<GmatBase *>(spacecraft));
        if (with_stm) {
            if (!m->psm->SetProperty("STM", static_cast<GmatBase *>(spacecraft)))
                throw std::runtime_error("PropagationStateManager::SetProperty(\"STM\", spacecraft) returned false");
        }
        m->psm->BuildState();
        m->ode->SetPropStateManager(m->psm);
        m->ode->SetState(m->psm->GetState());
        Initialize("");
        if (!m->ode->BuildModelFromMap()) throw std::runtime_error("BuildModelFromMap failed");
        m->ode->UpdateInitialData();
        m->dimension = m->psm->GetState()->GetSize();
        m->scratch.assign(m->dimension, 0.0);
    });
    if (rc != 0) {
        delete m->psm;
        delete m;
        return nullptr;
    }
    return m;
}

}  // namespace

gmatffi_model gmatffi_model_new(gmatffi_object force_model, gmatffi_object spacecraft, int *dimension_out) {
    Model *m = build_model(force_model, spacecraft, /*with_stm=*/false);
    if (!m) return nullptr;
    if (dimension_out) *dimension_out = m->dimension;
    return m;
}

gmatffi_model gmatffi_model_new_stm(gmatffi_object force_model, gmatffi_object spacecraft, int *dimension_out) {
    Model *m = build_model(force_model, spacecraft, /*with_stm=*/true);
    if (!m) return nullptr;
    if (dimension_out) *dimension_out = m->dimension;
    return m;
}

int gmatffi_model_state(gmatffi_model model, double *state_out, int dimension) {
    return guarded([&] {
        Model *m = static_cast<Model *>(model);
        if (dimension != m->dimension) throw std::runtime_error("dimension mismatch");
        const Real *s = m->psm->GetState()->GetState();
        std::memcpy(state_out, s, sizeof(double) * dimension);
    });
}

double gmatffi_model_epoch(gmatffi_model model) {
    Model *m = static_cast<Model *>(model);
    return m->psm->GetState()->GetEpoch();
}

int gmatffi_model_derivatives(gmatffi_model model, const double *state, double dt_seconds,
                              double *state_dot_out, int dimension) {
    return guarded([&] {
        Model *m = static_cast<Model *>(model);
        if (dimension != m->dimension) throw std::runtime_error("dimension mismatch");
        std::memcpy(m->scratch.data(), state, sizeof(double) * dimension);
        if (!m->ode->GetDerivatives(m->scratch.data(), (Real)dt_seconds, 1)) {
            throw std::runtime_error("GetDerivatives returned false");
        }
        const Real *d = m->ode->GetDerivativeArray();
        std::memcpy(state_dot_out, d, sizeof(double) * dimension);
    });
}

void gmatffi_model_free(gmatffi_model model) {
    Model *m = static_cast<Model *>(model);
    if (!m) return;
    delete m->psm;
    delete m;
}

gmatffi_object gmatffi_model_spacecraft(gmatffi_model model) {
    Model *m = static_cast<Model *>(model);
    return m->spacecraft;
}

int gmatffi_get_real_parameter(gmatffi_object obj, const char *name, double *out) {
    return guarded([&] {
        GmatBase *o = static_cast<GmatBase *>(obj);
        Real value = o->GetRealParameter(std::string(name));
        *out = (double)value;
    });
}

namespace {

// Looked up by name through GMAT's own configuration (Exists()/GetObject(), the same free
// functions APIFunctions.hpp exposes and altavista's own bodies.py::coordinate_system() uses on
// the Python side) -- never constructed here: a missing or not-yet-a-CoordinateSystem name is
// this function's caller's problem to report, not something to paper over by creating one on
// the fly (that would silently accept an uninitialized/unconfigured system, exactly what this
// task's own "never a silent identity conversion" rule forbids).
CoordinateSystem *lookup_initialized_coordinate_system(const std::string &name) {
    if (!Exists(name)) {
        throw std::runtime_error("CoordinateSystem not registered: " + name);
    }
    GmatBase *obj = GetObject(name);
    CoordinateSystem *cs = dynamic_cast<CoordinateSystem *>(obj);
    if (!cs) {
        throw std::runtime_error("object is not a CoordinateSystem: " + name);
    }
    if (!cs->IsInitialized()) {
        throw std::runtime_error("CoordinateSystem not initialized: " + name);
    }
    return cs;
}

}  // namespace

int gmatffi_convert_state(double epoch_a1mjd, const double *state6, const char *from_cs, const char *to_cs, double *out6) {
    return guarded([&] {
        CoordinateSystem *fromCs = lookup_initialized_coordinate_system(std::string(from_cs));
        CoordinateSystem *toCs = lookup_initialized_coordinate_system(std::string(to_cs));
        Real in[6];
        Real out[6] = {0.0, 0.0, 0.0, 0.0, 0.0, 0.0};
        for (int i = 0; i < 6; ++i) in[i] = (Real)state6[i];
        CoordinateConverter converter;
        if (!converter.Convert(A1Mjd(epoch_a1mjd), in, fromCs, out, toCs)) {
            throw std::runtime_error("CoordinateConverter::Convert returned false");
        }
        for (int i = 0; i < 6; ++i) out6[i] = (double)out[i];
    });
}

int gmatffi_convert_state_and_rotation(double epoch_a1mjd, const double *state6, const char *from_cs, const char *to_cs,
                                        double *out6, double *out_r9, double *out_rdot9) {
    return guarded([&] {
        CoordinateSystem *fromCs = lookup_initialized_coordinate_system(std::string(from_cs));
        CoordinateSystem *toCs = lookup_initialized_coordinate_system(std::string(to_cs));
        Real in[6];
        Real out[6] = {0.0, 0.0, 0.0, 0.0, 0.0, 0.0};
        for (int i = 0; i < 6; ++i) in[i] = (Real)state6[i];
        CoordinateConverter converter;
        if (!converter.Convert(A1Mjd(epoch_a1mjd), in, fromCs, out, toCs)) {
            throw std::runtime_error("CoordinateConverter::Convert returned false");
        }
        for (int i = 0; i < 6; ++i) out6[i] = (double)out[i];
        // Same call, immediately after: GetLastRotationMatrix()/GetLastRotationDotMatrix() read
        // back the R/Rdot the Convert() call above just computed (CoordinateConverter.cpp's own
        // Convert() sets lastRotMatrix/lastRotDotMatrix unconditionally, not gated behind
        // SetToCalculateRotMatrixDeriv -- that flag only controls the separate per-parameter
        // GetLastRotationMatrixDerivative(), which this shim does not need) -- never a second
        // Convert call, and never a different CoordinateConverter instance.
        Rmatrix33 r = converter.GetLastRotationMatrix();
        Rmatrix33 rdot = converter.GetLastRotationDotMatrix();
        for (int i = 0; i < 3; ++i) {
            for (int j = 0; j < 3; ++j) {
                out_r9[i * 3 + j] = (double)r(i, j);
                out_rdot9[i * 3 + j] = (double)rdot(i, j);
            }
        }
    });
}

const char *gmatffi_last_error(void) { return g_last_error.c_str(); }

}  // extern "C"
