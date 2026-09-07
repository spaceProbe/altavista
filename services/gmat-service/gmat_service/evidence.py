"""Evidence log (spoore ADR-005 pattern, cited by ADR-002's "Evidence, not re-execution" and
ADR-004's evidence rules): one JSON object per RPC response, so a replay can read the
recorded answer instead of re-running GMAT.

Format
------
One JSON object per line, UTF-8, newline-terminated, keys sorted::

    {
      "epoch":          <int64>   TAI nanoseconds the record was written (wall clock,
                                   converted via altavista.cdm.utc_ns_to_tai_ns) -- this is
                                   the evidence record's own creation time, not the
                                   request's simulated epoch (which is already inside
                                   request_hash, e.g. StateVector.tai_ns / GaussianState.
                                   epoch_ns)
      "hash":           <str>     SHA-256 hex of (prev_hash || body) -- see "Hash chain" below
      "method":         <str>     "Describe" | "Derivatives" | "Step" | "Propagate" | "Solve"
      "prev_hash":      <str>     previous record's "hash", or "GENESIS" for the first record
      "request_hash":   <str>     SHA-256 hex of the request protobuf's deterministic
                                   encoding (SerializeToString(deterministic=True))
      "response_hash":  <str>     SHA-256 hex of the response protobuf's deterministic
                                   encoding
      "run_id":         <str>     one id per server process (see gmat_service.server)
      "seq":            <int>     1-based, monotonic per evidence file
      "settings_hash":  <str>     gmat_service.config.settings_hash() at record time
    }

SHA-256 only, via ``hashlib`` -- no new crypto dependency, nothing bundled (ADR-004's
crypto rule).

Hash chain (ADR-004 M7.3, ``proto/altavista/v1/envelope.proto``'s ``SignedBatch`` convention)
-----------------------------------------------------------------------------------------------
Every record additionally carries ``seq``, ``prev_hash`` and ``hash``, chained exactly the
way ``SignedBatch.prev_hash``/``SignedBatch.hash`` are documented in ``envelope.proto``, and
**mirrored byte-for-byte in intent by
`crates/av-dynamics-service/src/evidence.rs`'s Rust twin** (same field names, same
convention, same body-bytes shape -- only the hashing library differs, ``hashlib`` here vs.
the ``openssl`` crate there, both backed by the system/Homebrew OpenSSL):

- ``prev_hash`` is the previous record's own ``hash`` (hex), or the literal string
  ``"GENESIS"`` for the first record in the log (the suite's ledger convention, verbatim).
- ``hash`` is ``SHA-256(prev_hash_bytes || body_bytes)`` hex-encoded, where
  ``prev_hash_bytes`` is the ASCII bytes of the literal ``"GENESIS"`` for the first record
  or else the 32 raw bytes the previous record's hex ``hash`` decodes to, and ``body_bytes``
  is the canonical (sorted-key) JSON encoding of this record's own
  ``epoch``/``method``/``request_hash``/``response_hash``/``run_id``/``seq``/
  ``settings_hash`` fields -- i.e. everything except ``prev_hash``/``hash`` themselves,
  playing the role ``SignedBatch.batch`` plays in the proto convention.

:meth:`EvidenceLog.verify` walks the file straight from disk (independent of any in-memory
state) and recomputes every record's ``hash``, so a record whose *content* was edited
without recomputing its ``hash`` -- or whose ``prev_hash``/``seq`` was tampered with -- is
detected and reported by ``seq``, mirroring ``altavista.v1.ChainVerification``'s
``ok``/``checked``/``broken_at_sequence``/``detail`` field shape (here:
``ok``/``checked``/``broken_at_seq``/``detail``).

**Not cross-language byte-identical (a deliberate, documented gap, not a bug):**
``json.dumps(body, sort_keys=True)`` emits ``", "``/``": "`` separators by default
(``{"epoch": 123, ...}``), while the Rust side's ``serde_json::to_vec`` emits compact JSON
(``{"epoch":123,...}``, no separators). ``_body_bytes`` therefore differs byte-for-byte
between the two languages for logically-identical field values, so **``hash`` values are
never comparable across a Python log and a Rust log**, even though both implement the
identical GENESIS/prev_hash/seq *algorithm*. Each language's own log is independently,
fully self-verifying (:meth:`verify` only ever compares against records written by the same
process's own :meth:`record`), which is all a replay or an assessor's chain-of-custody
check over *one* evidence file needs -- the same "not claimed byte-identical across
languages" caveat this module already states for :func:`hash_message`'s protobuf encoding,
just also true of this JSON body encoding.
"""
from __future__ import annotations

import hashlib
import json
import threading
import time
from pathlib import Path
from typing import Optional, Union

from google.protobuf.message import Message

from altavista.cdm import utc_ns_to_tai_ns

#: The suite's ledger convention (``envelope.proto``'s ``SignedBatch.prev_hash`` doc
#: comment, verbatim): the first record's ``prev_hash`` is this literal string, not a hash
#: of anything -- its ASCII bytes are what gets hashed with the first record's own body.
GENESIS = "GENESIS"


def hash_message(msg: Message) -> str:
    """SHA-256 hex of ``msg``'s deterministic protobuf encoding.

    ``deterministic=True`` sorts map-field entries by key internally, so this never
    depends on Python dict/field iteration order (the "determinism" binding rule) even
    though several request/response messages here (e.g. ``StepResponse.outputs``) carry
    map fields.
    """
    return hashlib.sha256(msg.SerializeToString(deterministic=True)).hexdigest()


def _body_bytes(*, epoch: int, method: str, request_hash: str, response_hash: str,
                run_id: str, seq: int, settings_hash: str) -> bytes:
    """The canonical bytes hashed alongside ``prev_hash`` -- the role ``SignedBatch.batch``
    plays in the proto convention. Field set matches
    `crates/av-dynamics-service/src/evidence.rs`'s ``EvidenceBody`` exactly."""
    body = {
        "epoch": epoch, "method": method, "request_hash": request_hash,
        "response_hash": response_hash, "run_id": run_id, "seq": seq,
        "settings_hash": settings_hash,
    }
    return json.dumps(body, sort_keys=True).encode("utf-8")


def _chain_hash(prev_hash: str, body: bytes) -> str:
    """``SHA-256(prev_hash_bytes || body)`` hex-encoded. Raises ``ValueError`` if
    ``prev_hash`` is neither ``GENESIS`` nor valid hex -- i.e. the log is already malformed
    at the caller's read site."""
    prev_bytes = GENESIS.encode("ascii") if prev_hash == GENESIS else bytes.fromhex(prev_hash)
    return hashlib.sha256(prev_bytes + body).hexdigest()


class EvidenceLog:
    """Appends one JSON line per RPC response to a local file.

    Not itself a multi-writer-process-safe log (plain file append); within one server
    process it is safe because :mod:`gmat_service.server` only ever calls :meth:`record`
    from the single GMAT worker thread (the same thread every RPC runs on), and the lock
    below only guards against a concurrent reader (e.g. a test) observing a partial line --
    and now also guards the chain state (``_next_seq``/``_last_hash``) that ``record``
    updates on every call.
    """

    def __init__(self, path: Union[str, Path]):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()
        self._next_seq, self._last_hash = self._recover_chain_state()

    def _recover_chain_state(self) -> tuple[int, str]:
        """Reads the last non-empty line of an existing evidence file (if any) to recover
        ``next_seq``/``last_hash`` -- a fresh/missing file starts a new chain at ``seq=1``,
        ``prev_hash="GENESIS"``. Deliberately tolerant of a malformed last line (falls back
        to a fresh chain rather than refusing to start the server): :meth:`verify` is the
        tool that reports a malformed log as a finding, not construction."""
        if not self.path.exists():
            return 1, GENESIS
        last_line: Optional[str] = None
        with self.path.open("r", encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if line:
                    last_line = line
        if last_line is None:
            return 1, GENESIS
        try:
            rec = json.loads(last_line)
            return int(rec["seq"]) + 1, str(rec["hash"])
        except (json.JSONDecodeError, KeyError, TypeError, ValueError):
            return 1, GENESIS

    def chain_head(self) -> str:
        """The current chain head (``"hash"`` of the most recently written record), or
        ``"GENESIS"`` if the log is still empty -- what ``/admin/api/evidence`` reports."""
        with self._lock:
            return self._last_hash

    def entry_count(self) -> int:
        """Number of records written so far (``next_seq - 1``)."""
        with self._lock:
            return self._next_seq - 1

    def record(self, *, method: str, request: Message, response: Message,
              settings_hash: str, run_id: str) -> dict:
        epoch = utc_ns_to_tai_ns(time.time_ns())
        request_hash = hash_message(request)
        response_hash = hash_message(response)
        with self._lock:
            seq = self._next_seq
            prev_hash = self._last_hash
            body = _body_bytes(epoch=epoch, method=method, request_hash=request_hash,
                               response_hash=response_hash, run_id=run_id, seq=seq,
                               settings_hash=settings_hash)
            record_hash = _chain_hash(prev_hash, body)
            entry = {
                "epoch": epoch, "hash": record_hash, "method": method, "prev_hash": prev_hash,
                "request_hash": request_hash, "response_hash": response_hash, "run_id": run_id,
                "seq": seq, "settings_hash": settings_hash,
            }
            line = json.dumps(entry, sort_keys=True)
            with self.path.open("a", encoding="utf-8") as fh:
                fh.write(line + "\n")
            self._next_seq = seq + 1
            self._last_hash = record_hash
        return entry

    def verify(self) -> dict:
        """Walks the evidence file straight from disk (independent of the in-memory chain
        state :meth:`record` maintains) and recomputes every record's chain link, so this
        detects tampering that happened to the file itself, not just a bug in ``record``.
        Stops and reports at the first broken record. Mirrors
        `crates/av-dynamics-service/src/evidence.rs`'s ``EvidenceLog::verify`` field-for-field
        (``ok``/``checked``/``broken_at_seq``/``detail``, matching
        ``altavista.v1.ChainVerification`` minus ``producer_id``)."""
        if not self.path.exists():
            return {"ok": True, "checked": 0, "broken_at_seq": None, "detail": "no evidence file yet"}

        expected_prev = GENESIS
        expected_seq = 1
        checked = 0
        with self.path.open("r", encoding="utf-8") as fh:
            for line_no, raw in enumerate(fh, start=1):
                line = raw.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError as e:
                    return {"ok": False, "checked": checked, "broken_at_seq": None,
                            "detail": f"line {line_no}: invalid JSON ({e})"}
                required = ("epoch", "hash", "method", "prev_hash", "request_hash",
                            "response_hash", "run_id", "seq", "settings_hash")
                if not all(k in rec for k in required):
                    return {"ok": False, "checked": checked, "broken_at_seq": None,
                            "detail": f"line {line_no}: record is missing one of the required evidence fields"}
                seq = rec["seq"]
                if seq != expected_seq:
                    return {"ok": False, "checked": checked, "broken_at_seq": seq,
                            "detail": f"line {line_no}: expected seq {expected_seq}, found seq {seq!r}"}
                if rec["prev_hash"] != expected_prev:
                    return {"ok": False, "checked": checked, "broken_at_seq": seq,
                            "detail": f"seq {seq}: prev_hash {rec['prev_hash']!r} does not match "
                                      f"the previous record's hash {expected_prev!r}"}
                body = _body_bytes(epoch=rec["epoch"], method=rec["method"],
                                   request_hash=rec["request_hash"], response_hash=rec["response_hash"],
                                   run_id=rec["run_id"], seq=rec["seq"], settings_hash=rec["settings_hash"])
                try:
                    recomputed = _chain_hash(expected_prev, body)
                except ValueError:
                    return {"ok": False, "checked": checked, "broken_at_seq": seq,
                            "detail": f"seq {seq}: prev_hash is not valid hex"}
                if recomputed != rec["hash"]:
                    return {"ok": False, "checked": checked, "broken_at_seq": seq,
                            "detail": f"seq {seq}: recorded hash does not match its recomputed "
                                      "content hash -- the record was tampered with after being written"}
                expected_prev = rec["hash"]
                expected_seq = seq + 1
                checked += 1
        return {"ok": True, "checked": checked, "broken_at_seq": None, "detail": "chain intact"}
