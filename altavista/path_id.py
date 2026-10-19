"""The path-component id rule (question 254, item 1), the Python twin of ``av_cdm::path_id``
(``crates/av-cdm/src/path_id.rs``).

When an id, name or run id that came from outside the program (a CLI argument, a YAML field, a
request) is about to become **one component of a filesystem path**, it must first pass
:func:`validate_path_component_id`.

A *path-component id* is 1 to 128 bytes of ASCII ``[A-Za-z0-9._-]`` whose first byte is
``[A-Za-z0-9]``. That refuses, among everything else: the empty id, ``.``, ``..``, anything
beginning with ``.``, ``/``, ``\\``, NUL, every control character, whitespace, ``%``, ``:`` and
every non-ASCII byte. A refusal is a typed :class:`PathIdError` raised **before** any path is
built. The reason is the first that applies, in this order: ``empty``, ``too_long``,
``disallowed_byte`` (the first byte outside the set), ``bad_first_byte`` (a first byte that is
``.``, ``-`` or ``_``).

Both implementations read their accepted and refused ids (each refused one with its reason) from
the committed fixture ``data/path_component_id_vectors.json`` (``tests/test_path_id.py`` here,
the unit tests in ``path_id.rs`` there), so the two cannot drift.

The rule is a *name* rule, not a containment check: a place whose legitimate inputs need more
must still enforce at least the minimum plus a lexical containment check, and say so there.
"""
from __future__ import annotations

MAX_LEN = 128
EMPTY = "empty"
TOO_LONG = "too_long"
DISALLOWED_BYTE = "disallowed_byte"
BAD_FIRST_BYTE = "bad_first_byte"

_ALNUM = frozenset(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789")
_ALLOWED = _ALNUM | frozenset(b"._-")


class PathIdError(ValueError):
    """``value`` is not a path-component id; ``reason`` is one of ``empty``, ``too_long``,
    ``disallowed_byte``, ``bad_first_byte`` (the names ``data/path_component_id_vectors.json``
    records)."""

    def __init__(self, value: str, reason: str, detail: str):
        super().__init__(f"{value!r} is not usable as a path component: {detail}")
        self.value = value
        self.reason = reason
        self.detail = detail


def validate_path_component_id(value: str) -> str:
    """Returns ``value`` unchanged when it is a path-component id; raises :class:`PathIdError`
    (before any path is built) when it is not."""
    if not isinstance(value, str):
        raise PathIdError(str(value), DISALLOWED_BYTE, f"it is a {type(value).__name__}, not a string")
    try:
        data = value.encode("utf-8")
    except UnicodeEncodeError:  # a lone surrogate: not ASCII either
        raise PathIdError(value, DISALLOWED_BYTE, "it holds a character that is not valid text") from None
    if not data:
        raise PathIdError(value, EMPTY, "it is empty")
    if len(data) > MAX_LEN:
        raise PathIdError(value, TOO_LONG, f"it is {len(data)} bytes, more than {MAX_LEN}")
    for index, byte in enumerate(data):
        if byte not in _ALLOWED:
            raise PathIdError(value, DISALLOWED_BYTE,
                              f"byte 0x{byte:02x} at offset {index} is not in [A-Za-z0-9._-]")
    if data[0] not in _ALNUM:
        raise PathIdError(value, BAD_FIRST_BYTE, f"it begins with {chr(data[0])!r}, not [A-Za-z0-9]")
    return value
