"""The S3 object store's image record, read from `services/store/IMAGE_DIGEST.md` (question 256).

One Python home for "what image is the object store, and how is it started": the Docker-gated
tests (`tests/heavy_stack.py` and everything that imports it) and the ten-gigabyte proof script
read the record here, so no second copy of the parsing rules exists (question 212(a): the recorded
digest has exactly one home; question 218: no second, independently-maintained copy of a rule).

The record is the markdown file's fenced blocks. Each block is found by a marker string on the
line before it (the first line that contains the marker, then the first fence after that line):

- `Registry reference`: the image reference to pull by digest.
- `docker image inspect`: the image id the local image must equal before a test trusts it.
- `Container port`: the TCP port the S3 server listens on inside the container.
- `Health path`: an unauthenticated GET path that answers 200 once the server serves.
- `Credential environment`: one `NAME=value` line per environment variable; the value is a
  template that may use the placeholders `{access_key}` and `{secret_key}` (the tests substitute
  per-run generated values, never a real credential).
- `Server arguments`: the container's arguments after the image reference, one per line.

Nothing here names a particular S3 server: any record of this shape works, so replacing the server
is an edit of the record, not of this module or the tests.

Standard library only.
"""
from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Optional, Tuple

REPO_ROOT = Path(__file__).resolve().parent.parent
STORE_IMAGE_DIGEST_MD = REPO_ROOT / "services" / "store" / "IMAGE_DIGEST.md"

ALLOWED_PLACEHOLDERS = frozenset({"access_key", "secret_key"})
_PLACEHOLDER_RE = re.compile(r"\{([^{}]*)\}")
_ENV_NAME_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def fenced_block_after(text: str, marker: str) -> Optional[str]:
    """The stripped content of the first ``` fenced block at or after the first line containing
    `marker`; `None` if there is no such line, no complete fence, or the block is empty. Same
    semantics as the Rust tests' `fenced_block_after`."""
    lines = text.splitlines()
    marker_idx = next((i for i, line in enumerate(lines) if marker in line), None)
    if marker_idx is None:
        return None
    try:
        fence_start = next(i for i in range(marker_idx, len(lines)) if lines[i].strip() == "```")
        fence_end = next(i for i in range(fence_start + 1, len(lines)) if lines[i].strip() == "```")
    except StopIteration:
        return None
    content = "\n".join(lines[fence_start + 1 : fence_end]).strip()
    return content or None


def _check_template(line: str, template: str, where: str) -> None:
    """Refuses a template with a placeholder other than `{access_key}`/`{secret_key}` or with a
    stray brace."""
    for name in _PLACEHOLDER_RE.findall(template):
        if name not in ALLOWED_PLACEHOLDERS:
            raise ValueError(
                f"{where}: line {line!r} uses the placeholder {{{name}}}; only "
                f"{{access_key}} and {{secret_key}} are allowed"
            )
    leftover = _PLACEHOLDER_RE.sub("", template)
    if "{" in leftover or "}" in leftover:
        raise ValueError(f"{where}: line {line!r} has an unbalanced brace")


def _split_env_line(line: str, where: str) -> Tuple[str, str]:
    name, sep, template = line.partition("=")
    if not sep:
        raise ValueError(f"{where}: line {line!r} has no '=' (expected NAME=template)")
    if not _ENV_NAME_RE.fullmatch(name):
        raise ValueError(f"{where}: line {line!r} has an invalid variable name {name!r}")
    _check_template(line, template, where)
    return name, template


@dataclass(frozen=True)
class StoreRecord:
    image_ref: str
    recorded_id: str
    container_port: int
    health_path: str
    credential_env: Tuple[str, ...]
    args: Tuple[str, ...]

    def env(self, access_key: str, secret_key: str) -> Dict[str, str]:
        """The credential environment with `{access_key}`/`{secret_key}` substituted, as
        name -> value in record order. Refuses a line without `=` and any other placeholder."""
        rendered: Dict[str, str] = {}
        for line in self.credential_env:
            name, template = _split_env_line(line, "Credential environment")
            rendered[name] = template.replace("{access_key}", access_key).replace("{secret_key}", secret_key)
        return rendered


def _required_block(text: str, marker: str, field: str, path: Path) -> str:
    block = fenced_block_after(text, marker)
    if block is None:
        raise ValueError(f"{path}: no fenced block after the {marker!r} marker ({field} is missing or empty)")
    return block


def parse_store_record(path: Path = STORE_IMAGE_DIGEST_MD) -> StoreRecord:
    """Reads the image record at `path`. Raises `ValueError` naming the missing or malformed
    field; an unreadable file raises the underlying `OSError`."""
    path = Path(path)
    text = path.read_text()

    image_ref = _required_block(text, "Registry reference", "image_ref", path)
    recorded_id = _required_block(text, "docker image inspect", "recorded_id", path)
    for field, value in (("image_ref", image_ref), ("recorded_id", recorded_id)):
        if len(value.split()) != 1:
            raise ValueError(f"{path}: {field} must be a single token, got {value!r}")

    port_text = _required_block(text, "Container port", "container_port", path)
    if not port_text.isascii() or not port_text.isdigit() or not 1 <= int(port_text) <= 65535:
        raise ValueError(f"{path}: container_port must be an integer from 1 to 65535, got {port_text!r}")
    container_port = int(port_text)

    health_path = _required_block(text, "Health path", "health_path", path)
    if not health_path.startswith("/") or len(health_path.split()) != 1:
        raise ValueError(f"{path}: health_path must be one token starting with '/', got {health_path!r}")

    env_block = _required_block(text, "Credential environment", "credential_env", path)
    credential_env = tuple(line.strip() for line in env_block.splitlines() if line.strip())
    for line in credential_env:
        _split_env_line(line, f"{path}: credential_env")

    args_block = _required_block(text, "Server arguments", "args", path)
    args = tuple(line.strip() for line in args_block.splitlines() if line.strip())

    return StoreRecord(
        image_ref=image_ref,
        recorded_id=recorded_id,
        container_port=container_port,
        health_path=health_path,
        credential_env=credential_env,
        args=args,
    )
