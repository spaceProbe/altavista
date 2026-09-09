//! Sample mode: one child process, one sample, the only place this binary ever touches GMAT
//! (`main.rs`'s own module doc comment explains why one binary has two modes at all).
//!
//! **Ordering requirement, load-bearing for `study.rs`'s "a bad sample is recorded, not
//! fatal" test:** every input is decoded and validated (hash-verified) BEFORE
//! `gmat_sys::engine_lock()`/`gmat_sys::Gmat::setup` -- see [`run_sample`]'s own body. This
//! matters because `av_kernel::drm::executor::execute` re-verifies these same three hashes
//! itself, but only AFTER its `RunConfig.gmat: &'a Gmat` field already borrows a live handle
//! (`Gmat::setup` already ran) -- so relying on `execute`'s own check alone would mean every
//! rejected sample still pays for a full GMAT startup first. Doing the same pure, GMAT-free
//! checks (`av_kernel::drm::hash::verify_*_hash`) here first is what makes a bad sample fail
//! fast.

use std::path::Path;

use av_cdm::pb;
use av_kernel::drm::{execute, RunConfig};
use prost::Message;

use crate::cli::SampleArgs;

fn read_and_decode<M: Message + Default>(path: &Path, what: &str) -> Result<M, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {} ({what}): {e}", path.display()))?;
    M::decode(bytes.as_slice()).map_err(|e| format!("decoding {} ({what}) as protobuf: {e}", path.display()))
}

pub fn run_sample(args: SampleArgs) -> Result<(), String> {
    // ---- Decode: no bespoke envelope, each file is one real CDM message (the module doc
    // comment's own contrast with the deleted AVRUN1 ad hoc framing av-run/src/main.rs's own
    // module doc comment records). ----
    let drm = read_and_decode::<pb::DesignReferenceMission>(&args.drm_pb, "DesignReferenceMission")?;
    let sos = read_and_decode::<pb::SosConfiguration>(&args.sos_pb, "SosConfiguration")?;
    let mut systems = std::collections::BTreeMap::new();
    for p in &args.system_pbs {
        let sys = read_and_decode::<pb::SystemDefinition>(p, "SystemDefinition")?;
        if let Some(_prev) = systems.insert(sys.id.clone(), sys) {
            return Err(format!("two --system-pb files declare the same SystemDefinition.id (last one: {})", p.display()));
        }
    }

    // ---- Validate, still before GMAT: the same canonical-hash checks execute() would run
    // anyway, done here first so a tampered/corrupt sample refuses cheaply. ----
    av_kernel::drm::hash::verify_drm_hash(&drm).map_err(|e| format!("{}: {e}", args.drm_pb.display()))?;
    av_kernel::drm::hash::verify_sos_hash(&sos).map_err(|e| format!("{}: {e}", args.sos_pb.display()))?;
    for (id, sys) in &systems {
        av_kernel::drm::hash::verify_system_hash(id, sys).map_err(|e| format!("system {id:?}: {e}"))?;
    }

    // ---- Only now does GMAT get touched -- gmat_sys::engine_lock() first (this crate's own
    // process-per-run convention, mirroring av-run/src/main.rs). ----
    let _engine = gmat_sys::engine_lock();
    let startup = args.gmat_startup.clone().unwrap_or_else(gmat_sys::Gmat::default_startup_file);
    let gmat = gmat_sys::Gmat::setup(&startup).map_err(|e| format!("GMAT setup ({startup}): {e}"))?;

    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: args.run_id.clone(), error_mode: args.error_mode, products_dir: None, replay: None };
    let products = execute(cfg).map_err(|e| format!("DRM execution failed: {e}"))?;

    eprintln!(
        "av-sweep: sample run_id={:?} config_hash={} trajectories={} events={}",
        args.run_id,
        products.provenance.config_hash,
        products.trajectories.len(),
        products.events.len()
    );

    let bundle = products.to_proto().encode_to_vec();
    std::fs::write(&args.out, &bundle).map_err(|e| format!("writing {}: {e}", args.out.display()))?;
    Ok(())
}
