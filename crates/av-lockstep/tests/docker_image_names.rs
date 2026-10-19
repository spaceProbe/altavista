//! Question 254 item 10 (t10): the image-name removal rule ([`av_lockstep::docker::remove_image_name`]
//! and [`av_lockstep::docker::ImageNameGuard`]) on throwaway images, never on a real image.
//!
//! On this host's containerd image store `docker rmi <repo:tag>` deletes the image itself unless
//! another reference survives: a nameless placeholder (an image known only by its id) goes with
//! the name. The test guards (`cfs_common::DockerImageGuard`, `drm_container.rs`,
//! `docker_lifecycle.rs`) used to run `docker rmi -f <name>` unconditionally, so a name added to a
//! pinned stand-in known only by its id took the pinned image with it. Every image here is a
//! `FROM scratch` build (a label and nothing else, no pull) under a run-scoped repository
//! `av-lockstep-names-<run>/...`, labelled `av.test=1` so the repository's own sweep reaps a leak.
//!
//! The docker tests take `lock_docker_tests()` themselves; never wrap this file in
//! `with_docker_lock.py`.

use std::collections::BTreeSet;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Command, Stdio};

use av_lockstep::docker::{announce_gate_skip, docker_daemon_status, image_ids_present, lock_docker_tests, remove_image_name, test_run_id, ImageNameGuard, ImageNameRemoval};

fn docker_ok(args: &[&str]) -> Result<String, String> {
    let out = Command::new("docker").args(args).output().map_err(|e| format!("could not launch docker: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn docker(args: &[&str]) -> String {
    docker_ok(args).unwrap_or_else(|e| panic!("`docker {args:?}` failed: {e}"))
}

/// Throwaway images of one run; reaped by label on drop (ids that carry THIS run's unique label
/// only, nameless ones included).
struct Scratch {
    run: String,
    repo: String,
    counter: usize,
}

impl Scratch {
    fn new() -> Self {
        let run = test_run_id();
        Self { repo: format!("av-lockstep-names-{run}"), run, counter: 0 }
    }

    /// A fresh, unique `FROM scratch` image named `<repo>/<name>:1`; returns `(ref, id)`.
    fn make(&mut self, name: &str) -> (String, String) {
        self.counter += 1;
        let reference = format!("{}/{name}:1", self.repo);
        let dockerfile = format!("FROM scratch\nLABEL av.test=1 av.test.run_id={} av.names.run={} av.names.n={}\n", self.run, self.run, self.counter);
        let mut child = Command::new("docker")
            .args(["build", "-q", "-t", &reference, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("launch docker build");
        child.stdin.take().expect("stdin").write_all(dockerfile.as_bytes()).expect("write Dockerfile");
        let out = child.wait_with_output().expect("docker build");
        assert!(out.status.success(), "docker build failed: {}", String::from_utf8_lossy(&out.stderr));
        let id = docker(&["image", "inspect", &reference, "--format", "{{.Id}}"]);
        (reference, id)
    }

    /// An image known ONLY by its id (the "pinned by id" shape): tagged `t`, then `t` moves onto
    /// another image, which leaves the first nameless. Returns its id.
    fn make_nameless(&mut self, name: &str) -> String {
        let t = format!("{}/mv-{name}:t", self.repo);
        let (a_ref, a_id) = self.make(&format!("{name}-a"));
        docker(&["tag", &a_id, &t]);
        docker(&["rmi", &a_ref]);
        let (_b_ref, b_id) = self.make(&format!("{name}-b"));
        docker(&["tag", &b_id, &t]);
        docker(&["rmi", &t]);
        assert!(self.names_of(&a_id).is_empty(), "the stand-in must be nameless: {:?}", self.names_of(&a_id));
        a_id
    }

    fn exists(reference: &str) -> bool {
        docker_ok(&["image", "inspect", reference]).is_ok()
    }

    fn names_of(&self, id: &str) -> Vec<String> {
        let out = docker(&["image", "ls", "--no-trunc", "--format", "{{.Repository}}:{{.Tag}} {{.ID}}", "--filter", &format!("label=av.names.run={}", self.run)]);
        out.lines().filter_map(|l| l.split_once(' ')).filter(|(_, i)| *i == id).map(|(n, _)| n.to_string()).collect()
    }

    fn labelled_ids(&self) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        for extra in [&[][..], &["--filter", "dangling=true"][..]] {
            let filter = format!("label=av.names.run={}", self.run);
            let mut args = vec!["image", "ls", "--no-trunc", "-q", "--filter", filter.as_str()];
            args.extend(extra.iter().copied());
            ids.extend(docker(&args).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string));
        }
        ids
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for _ in 0..3 {
            let ids = self.labelled_ids();
            if ids.is_empty() {
                break;
            }
            for id in ids {
                // Only ids carrying this run's unique label: throwaways this file made.
                let _ = docker_ok(&["rmi", "-f", &id]);
            }
        }
        let left = self.labelled_ids();
        if !left.is_empty() && !std::thread::panicking() {
            panic!("throwaway images left behind: {left:?}");
        }
    }
}

fn gated(name: &str) -> bool {
    match docker_daemon_status() {
        Ok(()) => true,
        Err(reason) => {
            let line = announce_gate_skip(name, &reason);
            assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
            false
        }
    }
}

/// The ordinary case the test guards were written for: a name added to an image that keeps
/// another tag. The name goes, the image stays.
#[test]
fn a_name_added_to_an_image_that_keeps_another_tag_is_untagged_and_the_image_survives() {
    if !gated("a_name_added_to_an_image_that_keeps_another_tag_is_untagged_and_the_image_survives") {
        return;
    }
    let _lock = lock_docker_tests();
    let mut s = Scratch::new();
    let (pin, pin_id) = s.make("pin");
    let ids_before = image_ids_present().expect("image ids");
    let alias = format!("127.0.0.1:1/{}/alias:test", s.repo);
    docker(&["tag", &pin_id, &alias]);
    let guard = ImageNameGuard::new(alias.clone(), &ids_before);
    assert!(!guard.created_here(), "the image was on the host before the tag was added");
    drop(guard);
    assert!(!Scratch::exists(&alias), "the alias must be gone");
    assert!(Scratch::exists(&pin), "the stand-in pinned image must survive");
    assert_eq!(docker(&["image", "inspect", &pin, "--format", "{{.Id}}"]), pin_id);
}

/// **The defect.** The stand-in pinned image is known only by its id; a helper tags it and, on
/// drop, used to `docker rmi -f <alias>`, which deleted it ("Untagged: alias", "Deleted: sha256").
/// The guard now refuses, loudly (a panic naming the image and the name), and the image and the
/// alias stay. **What this fails against:** the pre-fix `Drop` (an unconditional `docker rmi -f
/// <name>`) deletes the stand-in, `Scratch::exists(&pin_id)` is false and the first assertion
/// (no refusal) fails.
#[test]
fn a_name_added_to_a_nameless_pinned_image_is_refused_and_the_image_survives() {
    if !gated("a_name_added_to_a_nameless_pinned_image_is_refused_and_the_image_survives") {
        return;
    }
    let _lock = lock_docker_tests();
    let mut s = Scratch::new();
    let pin_id = s.make_nameless("pinned-by-id");
    let ids_before = image_ids_present().expect("image ids");
    assert!(ids_before.contains(&pin_id), "the stand-in is on the host");
    let alias = format!("{}/alias-of-nameless:test", s.repo);
    docker(&["tag", &pin_id, &alias]);
    let guard = ImageNameGuard::new(alias.clone(), &ids_before);
    assert!(!guard.created_here());
    let refusal = catch_unwind(AssertUnwindSafe(|| drop(guard))).expect_err("dropping the guard must refuse, loudly");
    let message = refusal.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("REFUSED") && message.contains(&alias) && message.contains(&pin_id), "the refusal must name the image and the name: {message:?}");
    assert!(Scratch::exists(&pin_id), "the stand-in pinned image must survive every guard");
    assert!(Scratch::exists(&alias), "the refused name is left in place");
}

/// The same refusal from the function itself, and `-f` does not change it.
#[test]
fn remove_image_name_refuses_the_last_name_of_a_preexisting_image_with_and_without_force() {
    if !gated("remove_image_name_refuses_the_last_name_of_a_preexisting_image_with_and_without_force") {
        return;
    }
    let _lock = lock_docker_tests();
    let mut s = Scratch::new();
    let pin_id = s.make_nameless("pinned-fn");
    let alias = format!("{}/alias-fn:test", s.repo);
    docker(&["tag", &pin_id, &alias]);
    for force in [false, true] {
        match remove_image_name(&alias, false, force) {
            ImageNameRemoval::Refused { name, image_id, .. } => assert!(name == alias && image_id == pin_id, "{name} {image_id}"),
            other => panic!("expected a refusal (force={force}), got {other:?}"),
        }
        assert!(Scratch::exists(&pin_id) && Scratch::exists(&alias), "nothing was removed (force={force})");
    }
    assert_eq!(remove_image_name(&format!("{}/never-existed:1", s.repo), false, true), ImageNameRemoval::Absent);
}

/// An image THIS run made keeps the old behaviour: its last name is removed and the image goes
/// with it (the throwaway `lockstep-ref` builds and the pushed names the tests create).
#[test]
fn the_last_name_of_an_image_made_by_this_run_is_removed_with_the_image() {
    if !gated("the_last_name_of_an_image_made_by_this_run_is_removed_with_the_image") {
        return;
    }
    let _lock = lock_docker_tests();
    let mut s = Scratch::new();
    let ids_before = image_ids_present().expect("image ids");
    let (made, made_id) = s.make("made-here");
    let second = format!("{}/made-here-second:1", s.repo);
    docker(&["tag", &made_id, &second]);
    let g_second = ImageNameGuard::new(second.clone(), &ids_before);
    let g_made = ImageNameGuard::new(made.clone(), &ids_before);
    assert!(g_second.created_here() && g_made.created_here());
    drop(g_second);
    assert!(Scratch::exists(&made) && !Scratch::exists(&second), "the second name goes, the image stays under its first");
    drop(g_made);
    assert!(!Scratch::exists(&made_id), "the image this run made goes with its last name");
}
