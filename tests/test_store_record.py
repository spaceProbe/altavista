"""Docker-free tests of `altavista/store_record.py`, the one Python reader of the S3 store's image
record (`services/store/IMAGE_DIGEST.md`, question 256).

The committed record must parse, and every missing field and every malformed value is refused with
a `ValueError` that names the field.
Nothing here starts a container or names a particular S3 server.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from altavista.store_record import STORE_IMAGE_DIGEST_MD, StoreRecord, fenced_block_after, parse_store_record

def _record_text(
    *,
    image_ref: str = "example/store@sha256:" + "a" * 64,
    recorded_id: str = "sha256:" + "a" * 64,
    port: str = "9999",
    health: str = "/ready",
    env: str = "STORE_USER={access_key}\nSTORE_PASSWORD={secret_key}",
    args: str = "serve\n--flag\n/data",
    omit: str = "",
) -> str:
    """A record of the documented shape; `omit` drops the fenced block after that marker."""
    blocks = [
        ("Registry reference", f"- Registry reference (pull by digest):\n```\n{image_ref}\n```"),
        ("docker image inspect", f"- `docker image inspect --format '{{{{.Id}}}}'`:\n```\n{recorded_id}\n```"),
        ("Container port", f"**Container port**\n```\n{port}\n```"),
        ("Health path", f"**Health path** (GET)\n```\n{health}\n```"),
        ("Credential environment", f"**Credential environment**\n```\n{env}\n```"),
        ("Server arguments", f"**Server arguments**\n```\n{args}\n```"),
    ]
    return "# a record\n\n" + "\n\n".join(body for marker, body in blocks if marker != omit) + "\n"


def _parse(tmp_path: Path, text: str) -> StoreRecord:
    path = tmp_path / "IMAGE_DIGEST.md"
    path.write_text(text)
    return parse_store_record(path)


def test_committed_record_parses():
    record = parse_store_record(STORE_IMAGE_DIGEST_MD)
    assert record.image_ref and record.image_ref.count("@sha256:") == 1, record
    assert record.recorded_id.startswith("sha256:"), record
    assert 1 <= record.container_port <= 65535
    assert record.health_path.startswith("/")
    assert record.args, "a record with no server arguments cannot start a server"
    rendered = record.env("AK", "SK")
    assert rendered, "a record with no credential environment would run an unauthenticated server"
    assert "AK" in rendered.values() and "SK" in rendered.values(), rendered


def test_a_well_formed_synthetic_record_parses(tmp_path):
    record = _parse(tmp_path, _record_text())
    assert record.image_ref == "example/store@sha256:" + "a" * 64
    assert record.recorded_id == "sha256:" + "a" * 64
    assert record.container_port == 9999
    assert record.health_path == "/ready"
    assert record.args == ("serve", "--flag", "/data")
    assert record.env("AK", "SK") == {"STORE_USER": "AK", "STORE_PASSWORD": "SK"}


def test_record_is_frozen(tmp_path):
    record = _parse(tmp_path, _record_text())
    with pytest.raises(Exception):
        record.container_port = 1  # type: ignore[misc]


def test_fenced_block_after_semantics():
    text = "intro\nMark one\n```\nfirst\n```\nMark two\n```\n\n```\nMark three\n```\nunterminated\n"
    assert fenced_block_after(text, "Mark one") == "first"
    assert fenced_block_after(text, "Mark two") is None, "an empty block is no block"
    assert fenced_block_after(text, "absent") is None
    assert fenced_block_after("Mark\n```\nnever closed\n", "Mark") is None


@pytest.mark.parametrize(
    "marker, field",
    [
        ("Registry reference", "image_ref"),
        ("docker image inspect", "recorded_id"),
        ("Container port", "container_port"),
        ("Health path", "health_path"),
        ("Credential environment", "credential_env"),
        ("Server arguments", "args"),
    ],
)
def test_each_missing_field_is_refused_by_name(tmp_path, marker, field):
    with pytest.raises(ValueError) as excinfo:
        _parse(tmp_path, _record_text(omit=marker))
    assert field in str(excinfo.value) and marker in str(excinfo.value)


@pytest.mark.parametrize(
    "overrides, field",
    [
        ({"image_ref": "two tokens"}, "image_ref"),
        ({"recorded_id": "two tokens"}, "recorded_id"),
        ({"port": "http"}, "container_port"),
        ({"port": "0"}, "container_port"),
        ({"port": "70000"}, "container_port"),
        ({"port": "-1"}, "container_port"),
        ({"port": "80 81"}, "container_port"),
        ({"health": "healthz"}, "health_path"),
        ({"health": "/two tokens"}, "health_path"),
        ({"env": "STORE_USER"}, "credential_env"),
        ({"env": "STORE_USER={access_key}\nNOEQUALS"}, "credential_env"),
        ({"env": "=value"}, "credential_env"),
        ({"env": "BAD NAME={access_key}"}, "credential_env"),
        ({"env": "STORE_USER={account}"}, "credential_env"),
        ({"env": "STORE_USER={access_key}{}"}, "credential_env"),
        ({"env": "STORE_USER={access_key"}, "credential_env"),
        ({"env": "STORE_USER=access_key}"}, "credential_env"),
    ],
)
def test_each_malformed_value_is_refused_by_name(tmp_path, overrides, field):
    with pytest.raises(ValueError) as excinfo:
        _parse(tmp_path, _record_text(**overrides))
    assert field in str(excinfo.value), excinfo.value


def test_env_refuses_a_line_without_equals_and_an_unknown_placeholder():
    no_equals = StoreRecord("r", "i", 1, "/h", ("NOEQUALS",), ())
    with pytest.raises(ValueError, match="no '='"):
        no_equals.env("a", "s")
    unknown = StoreRecord("r", "i", 1, "/h", ("X={region}",), ())
    with pytest.raises(ValueError, match="region"):
        unknown.env("a", "s")


def test_env_substitutes_both_placeholders_wherever_they_appear():
    record = StoreRecord("r", "i", 1, "/h", ("A={access_key}", "B=pre-{secret_key}-{access_key}", "C=plain"), ())
    assert record.env("AK", "SK") == {"A": "AK", "B": "pre-SK-AK", "C": "plain"}
