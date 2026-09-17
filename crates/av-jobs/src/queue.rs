//! Taking jobs from the durable log: [`JobQueue`] is the thin, ordered API
//! `crates/av-jobs::runner::Runner` and this crate's own tests use, over one [`crate::log::
//! JobLog`].

use std::path::Path;

use av_cdm::pb;

use crate::log::{JobLog, JobLogError, RecoveryReport};

/// What can go wrong submitting to, reading from, or completing a job in the queue.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// The underlying [`crate::log::JobLog`] refused the operation.
    #[error(transparent)]
    Log(#[from] JobLogError),
    /// [`JobQueue::submit`] was called with a `job_id` that already has a `submitted` record
    /// on this queue's log. A job submitted twice under the same id is a typed refusal, not
    /// a silent second entry -- this is what makes `job_id` the queue's own stable identity
    /// for a job (`JobSpec.job_id`'s own doc comment in `heavy.proto`).
    #[error("job_id {job_id:?} was already submitted to this queue -- refusing a duplicate submission")]
    DuplicateJobId { job_id: String },
}

fn as_submitted(record: &pb::JobLogRecord) -> Option<&pb::JobSpec> {
    match &record.event {
        Some(pb::job_log_record::Event::Submitted(spec)) => Some(spec),
        _ => None,
    }
}

fn as_completed(record: &pb::JobLogRecord) -> Option<&pb::JobCompletion> {
    match &record.event {
        Some(pb::job_log_record::Event::Completed(completion)) => Some(completion),
        _ => None,
    }
}

/// A durable, hash-chained queue of jobs, over one [`JobLog`]. Every method that inspects
/// queue state (`pending`) replays the whole log from disk rather than keeping a separate
/// in-memory index -- the log itself is the only state this type owns, so there is never a
/// second copy of "which jobs are pending" that could drift from what the log actually says.
#[derive(Debug)]
pub struct JobQueue {
    log: JobLog,
    // Serializes submit()'s own read-then-append (the duplicate-job_id check plus the
    // append that would make a race window otherwise): two threads racing to submit the
    // same job_id must not both pass the "not yet on the log" check before either appends.
    // A single Mutex<()> around the whole submit body is enough since JobLog::append
    // already serializes the write itself; this guards the check-then-act sequence around
    // it, which JobLog::append's own internal lock cannot do on its own.
    submit_guard: std::sync::Mutex<()>,
}

impl JobQueue {
    /// Opens (creating if necessary) the queue file for `name` under `dir` -- see
    /// [`JobLog::open`] for the recovery contract.
    pub fn open(dir: &Path, name: &str) -> Result<(Self, Option<RecoveryReport>), JobError> {
        let (log, report) = JobLog::open(dir, name)?;
        Ok((Self { log, submit_guard: std::sync::Mutex::new(()) }, report))
    }

    /// Wraps an already-open [`JobLog`] -- used by `crates/av-jobs::runner::Runner` and this
    /// crate's own tests that build the log themselves.
    pub fn new(log: JobLog) -> Self {
        Self { log, submit_guard: std::sync::Mutex::new(()) }
    }

    pub fn log(&self) -> &JobLog {
        &self.log
    }

    /// Appends `spec` as a new `submitted` record, refusing (as [`JobError::DuplicateJobId`])
    /// if `spec.job_id` already has a `submitted` record on this queue -- checked by
    /// replaying the whole log, so the check is always against what is actually durable, not
    /// an in-memory cache that could have drifted. Returns the hex SHA-256 of `spec`'s own
    /// `prost::Message::encode_to_vec` encoding.
    pub fn submit(&self, spec: &pb::JobSpec) -> Result<String, JobError> {
        let _guard = self.submit_guard.lock().unwrap_or_else(|p| p.into_inner());

        let existing = self.log.read_all()?;
        if existing.iter().filter_map(as_submitted).any(|s| s.job_id == spec.job_id) {
            return Err(JobError::DuplicateJobId { job_id: spec.job_id.clone() });
        }

        let payload = prost::Message::encode_to_vec(spec);
        let spec_sha256 = crate::hash::hex_encode(&openssl::sha::sha256(&payload));

        let record = pb::JobLogRecord { event: Some(pb::job_log_record::Event::Submitted(spec.clone())) };
        self.log.append(&record)?;
        Ok(spec_sha256)
    }

    /// Every submitted job with no matching `completed` record, matched on `job_id`, **in
    /// submission order** -- never a `HashMap`-iteration order, since both the "which jobs
    /// are completed" set and the final ordered walk are built from the log's own
    /// deterministic replay order.
    pub fn pending(&self) -> Result<Vec<pb::JobSpec>, JobError> {
        let records = self.log.read_all()?;
        let completed_ids: std::collections::BTreeSet<&str> = records.iter().filter_map(as_completed).map(|c| c.job_id.as_str()).collect();
        Ok(records.iter().filter_map(as_submitted).filter(|s| !completed_ids.contains(s.job_id.as_str())).cloned().collect())
    }

    /// Appends `completion` as a new `completed` record.
    pub fn complete(&self, completion: &pb::JobCompletion) -> Result<(), JobError> {
        let record = pb::JobLogRecord { event: Some(pb::job_log_record::Event::Completed(completion.clone())) };
        self.log.append(&record)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("av-jobs-queue-test-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spec(job_id: &str) -> pb::JobSpec {
        pb::JobSpec { job_id: job_id.to_string(), kind: "echo".to_string(), ..Default::default() }
    }

    fn completion(job_id: &str) -> pb::JobCompletion {
        pb::JobCompletion { job_id: job_id.to_string(), ok: true, ..Default::default() }
    }

    #[test]
    fn submit_returns_the_hex_sha256_of_the_specs_own_encoding() {
        let dir = TempDir::new("submit-hash");
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
        let s = spec("job-1");
        let got = queue.submit(&s).unwrap();
        let expected = crate::hash::hex_encode(&openssl::sha::sha256(&prost::Message::encode_to_vec(&s)));
        assert_eq!(got, expected);
        assert_eq!(got.len(), 64);
    }

    // -- acceptance evidence item 6: duplicate job_id is a typed refusal ---------------

    #[test]
    fn submit_refuses_a_duplicate_job_id() {
        let dir = TempDir::new("dup");
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
        queue.submit(&spec("job-1")).unwrap();
        let err = queue.submit(&spec("job-1")).unwrap_err();
        assert!(matches!(err, JobError::DuplicateJobId { ref job_id } if job_id == "job-1"), "{err:?}");
        // The duplicate must not have been appended: exactly one submitted record for
        // job-1 is on the log.
        let records = queue.log().read_all().unwrap();
        assert_eq!(records.iter().filter_map(as_submitted).filter(|s| s.job_id == "job-1").count(), 1);
    }

    #[test]
    fn submit_refuses_a_duplicate_job_id_even_after_that_job_completed() {
        let dir = TempDir::new("dup-after-complete");
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
        queue.submit(&spec("job-1")).unwrap();
        queue.complete(&completion("job-1")).unwrap();
        let err = queue.submit(&spec("job-1")).unwrap_err();
        assert!(matches!(err, JobError::DuplicateJobId { .. }), "{err:?}");
    }

    // -- acceptance evidence item 5: pending() is deterministic and order-preserving ----

    #[test]
    fn pending_returns_uncompleted_jobs_in_submission_order_regardless_of_completion_order() {
        let dir = TempDir::new("pending-order");
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
        for i in 0..6 {
            queue.submit(&spec(&format!("job-{i}"))).unwrap();
        }
        // Complete out of order: 3, 0, 5 -- leaving 1, 2, 4 pending.
        queue.complete(&completion("job-3")).unwrap();
        queue.complete(&completion("job-0")).unwrap();
        queue.complete(&completion("job-5")).unwrap();

        let expected: Vec<String> = vec!["job-1".to_string(), "job-2".to_string(), "job-4".to_string()];

        // Run the check several times over -- pending() replays the log fresh each call, so
        // this demonstrates the order does not depend on any HashMap's own iteration order
        // (which, if one were used, could vary run to run within the same process for a
        // hash-randomized default hasher).
        for _ in 0..20 {
            let pending: Vec<String> = queue.pending().unwrap().into_iter().map(|s| s.job_id).collect();
            assert_eq!(pending, expected);
        }
    }

    #[test]
    fn pending_is_empty_once_every_submitted_job_is_completed() {
        let dir = TempDir::new("pending-empty");
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
        queue.submit(&spec("job-1")).unwrap();
        queue.submit(&spec("job-2")).unwrap();
        assert_eq!(queue.pending().unwrap().len(), 2);
        queue.complete(&completion("job-1")).unwrap();
        queue.complete(&completion("job-2")).unwrap();
        assert!(queue.pending().unwrap().is_empty());
    }

    #[test]
    fn complete_appends_a_completed_record_readable_from_a_fresh_log() {
        let dir = TempDir::new("complete-fresh");
        {
            let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
            queue.submit(&spec("job-1")).unwrap();
            queue.complete(&completion("job-1")).unwrap();
        }
        // Re-open from disk in a fresh JobQueue/JobLog -- never trust the in-memory state
        // of the queue that wrote it.
        let (queue2, report) = JobQueue::open(dir.path(), "q").unwrap();
        assert!(report.is_none());
        let records = queue2.log().read_all().unwrap();
        assert_eq!(records.len(), 2);
        assert!(as_completed(&records[1]).is_some_and(|c| c.job_id == "job-1" && c.ok));
    }
}
