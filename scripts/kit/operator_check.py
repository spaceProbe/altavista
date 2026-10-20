"""scripts/kit/operator_check.py -- question 250(a), task 3: the installer verifies every operator-supplied
component the kit needs, and prints what it found.

# What this is

`docs/kit-operator-supplied.md` lists the components an operator brings themselves (items 1 to 7 are
pinned; item 8, the Python floor, is checked by `install.sh`), and
`scripts/kit/operator_supplied.json` pins each one (the pins file, carried in every kit as
`installer/operator_supplied.json`). This module is the one place that decides, for a kit,

1. which of them the kit NEEDS (`requirement_for`, derived from `KIT_MANIFEST` and nothing else),
2. what "verified" means for each kind (`verify_image`: a container image by digest; `verify_gmat_tree`:
   a tree by descriptor; `verify_python_wheel`: a wheel file by sha256), each a function returning a typed
   `Verification` (found, expected, verified, reason, code) and never raising for a component that is
   merely absent or different, and
3. what is printed (`render_line`): one line per component in the pins file, always all of them (seven today).

It imports the standard library and `manifest` only, so it ships inside every kit's `installer/`
(`build_kit.INSTALLER_SOURCE_FILES`) and runs on the enclave host and on the Docker host alike.

# What a kit needs (`requirement_for`)

| kind (pins file) | needed when `KIT_MANIFEST` says |
|---|---|
| `container-base-image` | an image listed in the pin's `base_of` is carried (`images.<c>.collected`) |
| `data-tree` (GMAT) | `packs.<pin.pack>` is present |
| `container-image` (Versity Gateway, PostGIS, ClickHouse) | the pin's id is in `operator_supplied_declared`, which `build_kit.py --declare-operator-supplied NAME` fills (default: empty); ClickHouse alone is also OPTIONAL when not declared (below) |
| `python-wheel` | an entry of `wheels.operator_supplied` names the pin's id as its `component`: the wheel step found a bundled GPL-family library in that wheel (`wheel_libs.py`) and did not carry the wheel |

A service image that is not declared is outside the kit and is listed as such, not verified.

# An optional operator-supplied image (question 260: ClickHouse)

A `container-image` pin may carry `optional: {question, enables, otherwise}`. ClickHouse does: its server
binary statically links LGPL libraries, so the kit never carries the image (question 251), but a site may
load the pinned digest onto its Docker host and then gets the ClickHouse MoE (sweep study) store; without it
the MoE store is the file store and nothing fails. The words of that sentence are the pin's own `optional`
entry, in one place. An optional image that the kit does not declare is `optional (...)`, not "outside the
kit": the image step (`verify_optional_image`) looks at the Docker host and the files install lists it as
`checked by the image step, not here`. Declaring it (`--declare-operator-supplied clickhouse`) makes it
required like Versity Gateway. The image step's `--moe-store auto|file|clickhouse` (default `auto`) chooses
how it is treated:

| `--moe-store` | image on the host is the pin | another image of the repository | no image of the repository |
|---|---|---|---|
| `auto` (undeclared) | `verified`, store clickhouse | refused `image-digest-differs` (names `--moe-store file`) | `not-configured`, store file, the walk continues |
| `file` | `not-selected` (found recorded), store file | `not-selected` (found recorded), store file | `not-selected`, store file |
| `clickhouse`, or declared | `verified`, store clickhouse | refused `image-digest-differs` | refused `image-missing` |

A lookup that fails (`docker image ls` / the inspect output unreadable) is refused `image-lookup-failed`
under every value and is never read as absent. A kit that declares ClickHouse with `--moe-store file` is a
contradiction the image step refuses before any docker call (`moe-store-contradiction`).
`moe_store_selection(results, mode)` is the ONE place that decides the store, from the ClickHouse result's
outcome and the mode: ClickHouse is selected only for `verified`. "Selected" means recorded
(`IMAGE_BUILD_RECORD.moe_store`): nothing in the installed system reads the selection yet.

A `python-wheel` pin is `{dist, version, filename, sha256, reference}`; the operator downloads that exact
file and the files install (`--operator-wheels DIR`) accepts it only when the sha256 of `DIR/<filename>`
is the pin's. Today's pins file has one, `numpy-wheel-linux-aarch64` (item 7): numpy's linux/aarch64 wheel
bundles libgfortran, GPL-3.0-or-later WITH GCC-exception-3.1, and the kit redistributes no GPL or LGPL code,
so a `--with-wheels` kit lists that wheel in `KIT_MANIFEST.wheels.operator_supplied` and does not carry it.
`save_operator_wheels.py` is the connected-machine helper that downloads exactly the pinned file.

# Two steps, one list

Verification is split between the two steps of the installer, by where the thing lives, because the
files install runs inside the enclave and never holds the Docker socket or `DOCKER_HOST` (the lead's
ruling, question 250(a)):

- the **files install** (`install.sh <kit> <target> ...`, and `install.sh --check-operator-supplied
  <kit> ...`, which verifies without installing) verifies trees: GMAT by descriptor;
- the **host-side image step** (`install.sh --build-images`, `--probe-offline-build`) verifies container
  images by digest: the bases and any declared service image, with `docker image inspect`.

Each step prints the WHOLE list. For a component that needs verifying but belongs to the other step
the line says so (`verified by the image step, not here`), and names the record that step writes; a
step never reads the other's record, because the two run at different times and, usually, on different
machines. To read the two outputs together: every component whose line says `required` must say
`verified` in exactly one of the two outputs and `... not here` in the other; a `required` line that
says `NOT VERIFIED` in either is a refusal (exit 2), and that step stopped there, so the lines after it
were not printed. A component that says `not required` or `outside the kit` in both needs nothing.

Line shape (two spaces between the three fields):

    operator-supplied: ubuntu-22.04-base-image  required (base of cfs-image)  verified sha256:2edbbc5dc405… via Id
    operator-supplied: gmat-r2026a  required (packs.gmat)  verified macos-arm64 sha256:d21cddbbffbd… 2060 files
    operator-supplied: gmat-r2026a  required (packs.gmat)  verified linux-x86_64 (rhel7) sha256:3db3afed6d28… 2043 files
    operator-supplied: gmat-r2026a  required (packs.gmat)  verified by the files install, not here (...)
    operator-supplied: versitygw  outside the kit, not verified (not declared; a site that deploys it builds the kit with --declare-operator-supplied versitygw)
    operator-supplied: clickhouse  optional (question 260: enables the ClickHouse MoE (sweep study) store; the file store otherwise)  checked by the image step, not here (record: IMAGE_BUILD_RECORD moe_store; this step does not read it)
    operator-supplied: clickhouse  optional (...)  verified sha256:6ded2b2c2f2c… via Id
    operator-supplied: clickhouse  optional (...)  not configured (not on this Docker host; the MoE store is the file store). To add it: ...
    operator-supplied: clickhouse  optional (...)  not selected (--moe-store file; ...)

Digests are shortened to 12 hex digits in the printed line; the structured form (`Verification.to_record`,
written into `INSTALL_RECORD.installed.operator_supplied.components` and `IMAGE_BUILD_RECORD.operator_supplied`)
holds them in full.
"""
from __future__ import annotations

import hashlib
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Mapping, Optional

sys.dont_write_bytecode = True  # see install.py: an installer run from a kit must not add files to it

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import manifest as kit_manifest  # noqa: E402  (path insert must precede this import)

STEP_FILES = "files"
STEP_IMAGES = "images"
STEP_NAMES = {STEP_FILES: "files install", STEP_IMAGES: "image step"}
#: Where each step writes its record, for the "verified by the other step" line.
STEP_RECORDS = {
    STEP_FILES: "INSTALL_RECORD installed.operator_supplied.components",
    STEP_IMAGES: "IMAGE_BUILD_RECORD operator_supplied",
}
#: Where the image step records the MoE store it selected (`moe_store_selection`).
MOE_RECORD = "IMAGE_BUILD_RECORD moe_store"
#: The component whose result decides the MoE store (question 260).
MOE_COMPONENT = "clickhouse"
MOE_AUTO, MOE_FILE, MOE_CLICKHOUSE = "auto", "file", "clickhouse"
#: The values of the image step's `--moe-store`.
MOE_MODES = (MOE_AUTO, MOE_FILE, MOE_CLICKHOUSE)
#: The key of `verify_all`'s `verifiers` for an image whose pin carries `optional`.
OPTIONAL_IMAGE = "optional-container-image"
#: What the installed system does with the selection (the same words in both records).
MOE_CONSUMER_NOTE = (
    "nothing in the installed system reads this selection yet: 'selected' means recorded here (no consumer in the "
    "kit); av-sweep still selects its store by its own flags (--store-dir, --store-clickhouse)"
)

KIND_BASE = "container-base-image"
KIND_IMAGE = "container-image"
KIND_TREE = "data-tree"
KIND_WHEEL = "python-wheel"
#: Which step verifies each kind of component.
STEP_OF_KIND = {KIND_BASE: STEP_IMAGES, KIND_IMAGE: STEP_IMAGES, KIND_TREE: STEP_FILES, KIND_WHEEL: STEP_FILES}

VERIFIED = "verified"
REFUSED = "refused"
OTHER_STEP = "other-step"
NOT_REQUIRED = "not-required"
OUTSIDE_KIT = "outside-kit"
#: An optional component (question 260) that the other step checks / that is not on this host / that
#: `--moe-store file` chose not to select.
OPTIONAL_OTHER_STEP = "optional-other-step"
NOT_CONFIGURED = "not-configured"
NOT_SELECTED = "not-selected"

_DIGEST = re.compile(r"sha256:[0-9a-f]{64}")


# =================================================================================================
# The typed result
# =================================================================================================

@dataclass
class Requirement:
    required: bool
    text: str           # "required (base of cfs-image)", "not required (...)", "outside the kit"
    outside_kit: bool = False
    why_not: str = ""   # the parenthetical for a service image that is not declared
    optional: bool = False   # the pin carries `optional` (question 260): required only when declared or `--moe-store clickhouse`


@dataclass
class Verification:
    """One component's result for one step. `verified` is true only when THIS step checked it and it
    was what the pin says; `reason` is the whole human-readable account (a refusal's full message);
    `code` is the stable machine-readable name of a refusal."""

    component: str
    item: int
    kind: str
    step: str
    outcome: str
    required: bool
    requirement: str
    expected: Any = None
    found: Any = None
    reason: str = ""
    code: Optional[str] = None
    detail: dict = field(default_factory=dict)

    @property
    def verified(self) -> bool:
        return self.outcome == VERIFIED

    def to_record(self) -> dict:
        return {
            "component": self.component, "item": self.item, "kind": self.kind,
            "step": STEP_NAMES[self.step], "verifies_in": STEP_NAMES[STEP_OF_KIND[self.kind]],
            "required": self.required, "requirement": self.requirement, "outcome": self.outcome,
            "verified": self.verified, "expected": self.expected, "found": self.found,
            "reason": self.reason, "code": self.code,
            "detail": {k: v for k, v in self.detail.items() if k != "record"},  # `record` is GMAT's legacy INSTALL_RECORD entry
        }


class ComponentRefused(RuntimeError):
    """Raised by `verify_all` after it printed the refused component's line. `result` is the refused
    `Verification` and `results` every result up to and including it; callers turn it into their own
    typed refusal (`install.OperatorSuppliedRefused`, `image_build.ImageBuildRefused`) with
    `result.code` and `result.reason`."""

    def __init__(self, result: Verification, results: list[Verification]) -> None:
        super().__init__(f"{result.code}: {result.reason}")
        self.result = result
        self.results = results


# =================================================================================================
# The pins file
# =================================================================================================

def _is_digest(value: object) -> bool:
    return isinstance(value, str) and _DIGEST.fullmatch(value) is not None


def pins_problems(pins: object) -> list[str]:
    """What is wrong with the pins file's `components` (empty list: nothing): every component has an
    integer `item` (unique), a known `kind`, `licences`, `why`, and a `verification` whose `step` is the
    one `STEP_OF_KIND` gives for the kind; images carry a `reference` pinned by their `digest`; a tree
    carries its `pack`, `platforms`, `scope` and, optionally, `variants`."""
    if not isinstance(pins, dict) or not pins:
        return ["`components` is missing or not a non-empty object"]
    problems: list[str] = []
    items: dict[int, str] = {}
    for cid, pin in sorted(pins.items()):
        if not isinstance(pin, dict):
            problems.append(f"{cid}: not an object")
            continue
        item = pin.get("item")
        if not isinstance(item, int) or isinstance(item, bool):
            problems.append(f"{cid}.item: {item!r} is not an integer")
        elif item in items:
            problems.append(f"{cid}.item: {item} is also the item of {items[item]}")
        else:
            items[item] = cid
        kind = pin.get("kind")
        if kind not in STEP_OF_KIND:
            problems.append(f"{cid}.kind: {kind!r} is not one of {sorted(STEP_OF_KIND)}")
            continue
        for key in ("licences", "why"):
            if not isinstance(pin.get(key), str) or not pin[key].strip():
                problems.append(f"{cid}.{key}: missing or empty")
        verification = pin.get("verification")
        if not isinstance(verification, dict) or verification.get("step") != STEP_OF_KIND[kind] or not verification.get("method"):
            problems.append(f"{cid}.verification: must be {{step: {STEP_OF_KIND[kind]!r}, method: ...}}")
        if "held" in pin:
            problems.append(f"{cid}.held: no longer supported (question 260 replaced it with `optional`)")
        if "optional" in pin:
            optional = pin["optional"]
            if kind != KIND_IMAGE:
                problems.append(f"{cid}.optional: only a {KIND_IMAGE} can be optional, not a {kind}")
            elif not (
                isinstance(optional, dict) and set(optional) == {"question", "enables", "otherwise"}
                and isinstance(optional["question"], int) and not isinstance(optional["question"], bool)
                and all(isinstance(optional[k], str) and optional[k].strip() for k in ("enables", "otherwise"))
            ):
                problems.append(f"{cid}.optional: must be {{question: int, enables: str, otherwise: str}}")
        if kind in (KIND_BASE, KIND_IMAGE):
            digest, reference = pin.get("digest"), pin.get("reference")
            if not _is_digest(digest):
                problems.append(f"{cid}.digest: {digest!r} is not sha256:<64 hex>")
            elif not isinstance(reference, str) or not reference.endswith("@" + digest):
                problems.append(f"{cid}.reference: {reference!r} is not pinned by the digest {digest}")
            if kind == KIND_BASE and not (isinstance(pin.get("base_of"), list) and pin["base_of"]):
                problems.append(f"{cid}.base_of: missing or empty")
            if "arm64_manifest_digest" in pin and not _is_digest(pin["arm64_manifest_digest"]):
                problems.append(f"{cid}.arm64_manifest_digest: not sha256:<64 hex>")
        elif kind == KIND_WHEEL:
            for key in ("dist", "version", "reference"):
                if not isinstance(pin.get(key), str) or not pin[key].strip():
                    problems.append(f"{cid}.{key}: missing or empty")
            filename, digest = pin.get("filename"), pin.get("sha256")
            if not isinstance(filename, str) or not filename.endswith(".whl") or "/" in filename or "\\" in filename or filename.startswith("."):
                problems.append(f"{cid}.filename: {filename!r} is not a bare wheel file name")
            if not _is_digest(digest):
                problems.append(f"{cid}.sha256: {digest!r} is not sha256:<64 hex>")
        else:
            if not isinstance(pin.get("pack"), str):
                problems.append(f"{cid}.pack: missing")
            problems += [f"{cid}.{p}" for p in kit_manifest.pin_problems(pin.get("platforms"))]
            problems += [f"{cid}.{p}" for p in kit_manifest.variant_problems(pin.get("variants"), pin.get("platforms"))]
            problems += [f"{cid}.{p}" for p in kit_manifest.scope_problems(pin.get("scope"))]
    return problems


def wheel_pin_for(pins: Mapping[str, dict], filename: str) -> Optional[tuple[str, dict]]:
    """`(component id, pin)` of the `python-wheel` pin for the wheel file `filename`, or None. The
    builder's lookup, when the wheel step finds a GPL-family library in a wheel."""
    for cid, pin in sorted(pins.items()):
        if pin.get("kind") == KIND_WHEEL and pin.get("filename") == filename:
            return cid, pin
    return None


def kit_operator_wheels(doc: dict) -> list[dict]:
    """The `wheels.operator_supplied` entries of a parsed `KIT_MANIFEST` (empty when there are none or the
    section predates them -- `install._check_required_wheels` refuses that case for a collected `wheels`)."""
    entries = (doc.get("wheels") or {}).get("operator_supplied") or []
    return [e for e in entries if isinstance(e, dict)]


def declaration_problems(names: list[str], pins: Mapping[str, dict]) -> list[str]:
    """What is wrong with a `--declare-operator-supplied` list: a name that is not a component of the
    pins file, one that is not a service container image (the bases and GMAT are derived from what the
    kit carries, never declared). An optional image (ClickHouse, question 260) is declarable: declaring it
    makes it required."""
    problems: list[str] = []
    for name in names:
        pin = pins.get(name)
        if pin is None:
            problems.append(f"{name!r} is not a component of the pins file (declarable: {sorted(declarable(pins))})")
        elif pin.get("kind") != KIND_IMAGE:
            problems.append(
                f"{name!r} is a {pin.get('kind')}, derived from what the kit carries; only a service container image "
                f"is declared (declarable: {sorted(declarable(pins))})"
            )
    return problems


def declarable(pins: Mapping[str, dict]) -> list[str]:
    return [cid for cid, pin in pins.items() if pin.get("kind") == KIND_IMAGE]


def optional_text(pin: dict) -> str:
    """The words for an optional image, from the pin's own `optional` entry and nowhere else."""
    o = pin["optional"]
    return f"optional (question {o['question']}: enables {o['enables']}; {o['otherwise']} otherwise)"


def moe_mode_problem(doc: dict, pins: Mapping[str, dict], mode: str) -> Optional[str]:
    """Why `--moe-store MODE` cannot be used for the kit described by `doc`, or None. An unknown mode, or
    `file` for a kit that declares ClickHouse (a declared image is required; `file` says never to use it).
    Reads nothing but its arguments: the image step calls it before any docker call."""
    if mode not in MOE_MODES:
        return f"--moe-store {mode!r} is not one of {list(MOE_MODES)}"
    declared = [c for c in doc.get("operator_supplied_declared") or [] if (pins.get(c) or {}).get("optional")]
    if mode == MOE_FILE and declared:
        return (
            f"this kit declares {', '.join(declared)} (KIT_MANIFEST operator_supplied_declared, "
            f"--declare-operator-supplied {declared[0]}), so the image is required, and --moe-store file says never to "
            f"use it: a contradiction. Drop --moe-store file, or use a kit built without the declaration"
        )
    return None


# =================================================================================================
# What the kit needs
# =================================================================================================

def requirement_for(
    pin_id: str, pin: dict, doc: dict, *, images: Optional[set] = None, moe_mode: str = MOE_AUTO,
) -> Requirement:
    """Whether the kit described by `doc` (a parsed `KIT_MANIFEST`) needs this component, and the words
    for it. `images`, when given, restricts which of the kit's images count (the image step's
    `--component`). `moe_mode` is the image step's `--moe-store`: `clickhouse` makes an optional image
    required. Nothing here reads anything but `doc` and `pin`."""
    kind = pin["kind"]
    if kind == KIND_BASE:
        collected = {c for c, e in (doc.get("images") or {}).items() if isinstance(e, dict) and e.get("collected")}
        carried = sorted(c for c in pin.get("base_of", []) if c in collected)
        if images is not None:
            left_out = [c for c in carried if c not in images]
            carried = [c for c in carried if c in images]
            if not carried and left_out:
                return Requirement(False, f"not required (--component leaves out {', '.join(left_out)})")
        if carried:
            return Requirement(True, f"required (base of {', '.join(carried)})")
        return Requirement(False, f"not required (the kit carries no application layer for {', '.join(pin.get('base_of', []))})")
    if kind == KIND_TREE:
        pack = pin["pack"]
        if pack in (doc.get("packs") or {}):
            return Requirement(True, f"required (packs.{pack})")
        return Requirement(False, f"not required (the kit has no packs.{pack})")
    if kind == KIND_WHEEL:
        if any(e.get("component") == pin_id for e in kit_operator_wheels(doc)):
            return Requirement(True, f"required (wheels.operator_supplied: {pin['filename']})")
        return Requirement(False, "not required (no wheel of the kit bundles a GPL-family library)")
    optional = bool(pin.get("optional"))
    declared = doc.get("operator_supplied_declared") or []
    if pin_id in declared:
        return Requirement(True, f"required (declared at build time: --declare-operator-supplied {pin_id})", optional=optional)
    if optional:
        if moe_mode == MOE_CLICKHOUSE:
            return Requirement(True, f"required (--moe-store clickhouse; question {pin['optional']['question']})", optional=True)
        return Requirement(False, optional_text(pin), optional=True)
    why = f"not declared; a site that deploys it builds the kit with --declare-operator-supplied {pin_id}"
    return Requirement(False, "outside the kit", outside_kit=True, why_not=why)


def _kit_pin_problems(doc: dict, pins: Mapping[str, dict], images: Optional[set]) -> Optional[tuple[str, str, str]]:
    """The first way `doc` needs something the pins file cannot describe, as `(code, component, reason)`."""
    for pack, cid in kit_manifest.OPERATOR_SUPPLIED_PACKS.items():
        if pack in (doc.get("packs") or {}) and cid not in pins:
            return (f"{pack}-pin-malformed", cid, f"the kit has packs.{pack}, but the installer's pins file has no component {cid!r}")
    for name in doc.get("operator_supplied_declared") or []:
        problems = declaration_problems([name], pins)
        if problems:
            return ("declared-component-refused", name, f"KIT_MANIFEST declares operator-supplied {problems[0]}")
    for entry in kit_operator_wheels(doc):
        cid = entry.get("component")
        pin = pins.get(cid) if isinstance(cid, str) else None
        if pin is None or pin.get("kind") != KIND_WHEEL:
            return (
                "wheel-pin-missing", str(cid),
                f"KIT_MANIFEST's wheels.operator_supplied names {entry.get('filename')!r} as the operator-supplied "
                f"component {cid!r}, but the installer's pins file has no python-wheel component of that id",
            )
    for image, entry in sorted((doc.get("images") or {}).items()):
        if isinstance(entry, dict) and entry.get("collected") and (images is None or image in images):
            if sum(1 for p in pins.values() if p.get("kind") == KIND_BASE and image in p.get("base_of", [])) != 1:
                return ("base-pin-missing", image, f"the installer's pins file does not name exactly one base image for {image}")
    return None


# =================================================================================================
# The printed list
# =================================================================================================

def short(digest: str) -> str:
    """`sha256:` plus the first 12 hex digits and an ellipsis."""
    if isinstance(digest, str) and digest.startswith("sha256:") and len(digest) > 19:
        return digest[:19] + "…"
    return str(digest)


def render_line(v: Verification) -> str:
    """The one printed line for a result (module doc, "Line shape")."""
    head = f"operator-supplied: {v.component}  "
    if v.outcome == NOT_REQUIRED:
        return head + v.requirement + ", not verified"
    if v.outcome == OUTSIDE_KIT:
        return head + f"{v.requirement}, not verified ({v.detail.get('why_not', '')})"
    if v.outcome == OTHER_STEP:
        other = STEP_OF_KIND[v.kind]
        return head + (
            f"{v.requirement}  verified by the {STEP_NAMES[other]}, not here "
            f"(record: {STEP_RECORDS[other]}; this step does not read it)"
        )
    if v.outcome == OPTIONAL_OTHER_STEP:
        other = STEP_OF_KIND[v.kind]
        return head + (
            f"{v.requirement}  checked by the {STEP_NAMES[other]}, not here "
            f"(record: {MOE_RECORD}; this step does not read it)"
        )
    if v.outcome == NOT_CONFIGURED:
        return head + (
            f"{v.requirement}  not configured ({v.reason}; the MoE store is {v.detail['otherwise']}). "
            f"To add it: {v.detail['how_to_add']}"
        )
    if v.outcome == NOT_SELECTED:
        return head + f"{v.requirement}  not selected ({v.detail['summary']})"
    if v.outcome == VERIFIED:
        return head + f"{v.requirement}  verified {v.detail.get('summary', short(v.expected))}"
    return head + f"{v.requirement}  NOT VERIFIED [{v.code}]"


def verify_all(
    step: str,
    doc: dict,
    pins: Mapping[str, dict],
    *,
    verifiers: Mapping[str, Callable[[str, dict, Requirement], Verification]],
    out: Optional[Callable[[str], None]] = None,
    images: Optional[set] = None,
    moe_mode: str = MOE_AUTO,
) -> list[Verification]:
    """Walks every component of the pins file in item order, prints its line through `out` as it goes,
    and returns the results. A component the kit needs and `step` verifies goes to `verifiers[kind]`; one
    the kit needs and the other step verifies is listed as such; the rest are listed as not required or
    outside the kit. The first component whose verifier returns an unverified result stops the walk:
    its line is printed and `ComponentRefused` is raised, so the operator sees what was found up to the
    refusal. Raises `ValueError` if the pins file is malformed (`pins_problems`)."""
    out = out or (lambda line: None)
    problems = pins_problems(pins)
    if problems:
        raise ValueError("; ".join(problems))
    results: list[Verification] = []
    bad = _kit_pin_problems(doc, pins, images)
    if bad is not None:
        code, cid, reason = bad
        pin = pins.get(cid) or {"item": 0, "kind": KIND_TREE}
        refused = Verification(
            component=cid, item=pin.get("item", 0), kind=pin.get("kind", KIND_TREE), step=step, outcome=REFUSED, required=True,
            requirement="required", reason=reason, code=code,
        )
        out(render_line(refused))
        raise ComponentRefused(refused, [refused])
    for pin_id, pin in sorted(pins.items(), key=lambda kv: kv[1]["item"]):
        req = requirement_for(pin_id, pin, doc, images=images, moe_mode=moe_mode)
        base = dict(component=pin_id, item=pin["item"], kind=pin["kind"], step=step, required=req.required, requirement=req.text)
        if not req.required and not req.optional:
            outcome = OUTSIDE_KIT if req.outside_kit else NOT_REQUIRED
            result = Verification(outcome=outcome, detail={"why_not": req.why_not} if req.why_not else {}, **base)
        elif STEP_OF_KIND[pin["kind"]] != step:
            # An optional image that is not required is the other step's to look at; one that is required
            # (declared) is verified there like any other.
            result = Verification(
                outcome=OTHER_STEP if req.required else OPTIONAL_OTHER_STEP,
                reason=f"verified by the {STEP_NAMES[STEP_OF_KIND[pin['kind']]]}" if req.required
                else f"checked by the {STEP_NAMES[STEP_OF_KIND[pin['kind']]]}",
                expected=pin.get("digest"),
                detail=_optional_detail(pin_id, pin) if req.optional else {}, **base,
            )
        elif req.optional:
            result = verifiers[OPTIONAL_IMAGE](pin_id, pin, req)
        else:
            result = verifiers[pin["kind"]](pin_id, pin, req)
        results.append(result)
        out(render_line(result))
        if result.outcome == REFUSED:
            raise ComponentRefused(result, results)
    return results


# =================================================================================================
# Verifying a container image (the host-side image step)
# =================================================================================================

def _text(data: bytes) -> str:
    return data.decode("utf-8", "replace")


def _verified_by(image_id: object, repo_digests: object, digest: str) -> Optional[str]:
    """How an inspected image is the pinned `digest`: `"Id"` (containerd store: the image id IS the index
    digest), `"RepoDigests"` (classic store: an entry ends `@<digest>`), or None (it is not)."""
    if image_id == digest:
        return "Id"
    if any(isinstance(d, str) and d.endswith("@" + digest) for d in (repo_digests or [])):
        return "RepoDigests"
    return None


def verify_image(docker: Callable[..., Any], pin_id: str, pin: dict, requirement: Requirement) -> Verification:
    """A container image, found by its digest-qualified reference, must BE the pinned digest. `docker`
    is the image step's runner (`docker(args, timeout=...) -> CompletedProcess` of bytes). Accepted when
    `docker image inspect <reference>` reports an `Id` equal to the pinned digest (containerd image
    store: the image id IS the index digest) or a `RepoDigests` entry ending `@<digest>` (classic
    store). Absent, or present with another digest, is a refused result, never an exception; nothing is
    pulled or loaded."""
    ref, digest = pin["reference"], pin["digest"]
    is_base = pin["kind"] == KIND_BASE
    label = "base image" if is_base else "image"
    base = dict(
        component=pin_id, item=pin["item"], kind=pin["kind"], step=STEP_IMAGES, required=True,
        requirement=requirement.text, expected=digest,
    )

    def refused(code: str, reason: str, found: Any = None) -> Verification:
        return Verification(outcome=REFUSED, code=code, reason=reason, found=found, **base)

    missing = "base-image-missing" if is_base else "image-missing"
    r = docker(["image", "inspect", ref], timeout=60.0)
    if r.returncode != 0:
        how = (
            "Load the operator-supplied base first -- `docker load -i <tarball>` of an image saved from exactly that "
            "digest (scripts/kit/save_base_images.py makes one) -- and re-run. Nothing was built and nothing was pulled."
            if is_base else
            "Load the operator-supplied image first -- `docker load -i <tarball>` of an image saved from exactly that "
            "digest -- and re-run. Nothing was pulled."
        )
        return refused(
            missing,
            f"the {label} {ref} is not on this Docker host (expected digest {digest}; `docker image inspect` "
            f"rc={r.returncode}: {_text(r.stderr).strip()}). {how}",
        )
    try:
        info = json.loads(_text(r.stdout))[0]
    except (ValueError, IndexError, TypeError) as e:
        return refused(missing, f"`docker image inspect {ref}` gave output this step cannot read: {e}")
    image_id, repo_digests = info.get("Id"), info.get("RepoDigests") or []
    via = _verified_by(image_id, repo_digests, digest)
    if via is None:
        return refused(
            "base-image-digest-differs" if is_base else "image-digest-differs",
            f"the image found for {ref} has Id {image_id} and RepoDigests {repo_digests!r}; neither is the pinned digest "
            f"{digest}. Load the operator-supplied {label} saved from exactly that digest and re-run.",
            found={"Id": image_id, "RepoDigests": repo_digests},
        )
    detail = {"reference": ref, "digest": digest, "image_id": image_id, "verified_by": via,
              "summary": f"{short(digest)} via {via}"}
    return Verification(outcome=VERIFIED, found=image_id, detail=detail, **base)


# =================================================================================================
# An optional container image (ClickHouse, question 260) and the MoE store decision
# =================================================================================================

LIST_IMAGES_ARGS = ["image", "ls", "--no-trunc", "--digests", "--format", "{{json .}}"]


def optional_how_to_add(pin_id: str, pin: dict) -> str:
    """How an operator adds an optional image (one text, used by the line, the refusals and both records)."""
    ref = pin["reference"]
    return (
        f"load an image saved from exactly {ref} (`docker load -i <tarball>`; on a connected machine: "
        f"`docker pull {ref}` then `docker save {ref} -o {pin_id}.tar`), then re-run `install.sh --build-images <kit>`"
    )


def _optional_detail(pin_id: str, pin: dict) -> dict:
    return {
        "reference": pin["reference"], "digest": pin["digest"], "repository": pin["reference"].partition("@")[0],
        "optional": dict(pin["optional"]), "otherwise": pin["optional"]["otherwise"],
        "how_to_add": optional_how_to_add(pin_id, pin),
    }


def _list_repository(docker: Callable[..., Any], repository: str) -> tuple[Optional[list[dict]], str]:
    """Every image of `repository` on the Docker host, tagged or digest-only, as `(rows, "")`, or
    `(None, why)` when the lookup failed. Measured on the containerd image store: `docker image ls
    --no-trunc --digests --format '{{json .}}' <repository>` lists digest-only entries (Tag `<none>`) and
    exits 0 with no output for a repository the host does not hold."""
    r = docker(LIST_IMAGES_ARGS + [repository], timeout=60.0)
    if r.returncode != 0:
        return None, f"`docker image ls {repository}` rc={r.returncode}: {_text(r.stderr).strip()}"
    rows: list[dict] = []
    for line in _text(r.stdout).splitlines():
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except ValueError as e:
            return None, f"`docker image ls {repository}` gave a line this step cannot read ({e}): {line[:200]!r}"
        if not isinstance(row, dict):
            return None, f"`docker image ls {repository}` gave a line that is not an object: {line[:200]!r}"
        if row.get("Repository") == repository:
            rows.append({"repository": repository, "tag": row.get("Tag"), "id": row.get("ID"), "digest": row.get("Digest")})
    return rows, ""


def _describe_found(found: dict) -> str:
    parts = []
    if found.get("inspect"):
        parts.append(f"the pinned reference resolves to Id {found['inspect'].get('Id')} (RepoDigests {found['inspect'].get('RepoDigests')!r})")
    for i in found.get("images") or []:
        parts.append(f"{i['repository']}:{i['tag']} id {i['id']} digest {i['digest']}")
    return "; ".join(parts) or "nothing"


def verify_optional_image(
    docker: Callable[..., Any], pin_id: str, pin: dict, requirement: Requirement, mode: str = MOE_AUTO,
) -> Verification:
    """An image whose pin is `optional` (ClickHouse, question 260), looked for on the Docker host by its
    digest-qualified reference and, when that is not found, by its repository (every image of it, tagged
    or digest-only). Module doc, "An optional operator-supplied image", has the table of outcomes by
    `mode` (`--moe-store`). `requirement.required` is true when the kit declares the image or `mode` is
    `clickhouse`: absent is then `image-missing`, as for any declared image. Never raises for an image that
    is merely absent or different; a lookup that fails is `image-lookup-failed` under every mode. Nothing
    is pulled or loaded."""
    ref, digest = pin["reference"], pin["digest"]
    repository = ref.partition("@")[0]
    strict = requirement.required
    file_mode = mode == MOE_FILE and not strict
    detail = _optional_detail(pin_id, pin)
    base = dict(
        component=pin_id, item=pin["item"], kind=pin["kind"], step=STEP_IMAGES, required=requirement.required,
        requirement=requirement.text, expected=digest,
    )

    def refused(code: str, reason: str, found: Any = None) -> Verification:
        return Verification(outcome=REFUSED, code=code, reason=reason, found=found, detail=detail, **base)

    def lookup_failed(why: str) -> Verification:
        return refused(
            "image-lookup-failed",
            f"cannot tell whether {ref} is on this Docker host: {why}. Nothing was enabled and nothing was pulled; "
            f"a failed lookup is never read as 'not on this host'. Fix the Docker lookup and re-run.",
        )

    r = docker(["image", "inspect", ref], timeout=60.0)
    inspected: Optional[dict] = None
    images: list[dict] = []
    via: Optional[str] = None
    inspect_failure = ""
    if r.returncode == 0:
        try:
            info = json.loads(_text(r.stdout))[0]
            inspected = {"Id": info.get("Id"), "RepoDigests": info.get("RepoDigests") or []}
        except (ValueError, IndexError, TypeError, AttributeError) as e:
            return lookup_failed(f"`docker image inspect {ref}` gave output this step cannot read ({e})")
        via = _verified_by(inspected["Id"], inspected["RepoDigests"], digest)
    else:
        inspect_failure = f"`docker image inspect` rc={r.returncode}: {_text(r.stderr).strip()}"
        listed, why = _list_repository(docker, repository)
        if listed is None:
            return lookup_failed(f"{inspect_failure}; then {why}")
        images = listed
    found = {"inspect": inspected, "images": images, "pinned_image_present": via is not None}
    detail = {**detail, "found": found}

    if via is not None:
        if file_mode:
            return Verification(
                outcome=NOT_SELECTED, found=found, reason="--moe-store file: ClickHouse is not selected",
                detail={**detail, "summary": "--moe-store file; the pinned image is on this Docker host and was left unused"},
                **base,
            )
        detail = {**detail, "image_id": inspected["Id"], "verified_by": via, "summary": f"{short(digest)} via {via}"}
        return Verification(outcome=VERIFIED, found=inspected["Id"], detail=detail, **base)
    if inspected is not None or images:
        if file_mode:
            return Verification(
                outcome=NOT_SELECTED, found=found, reason="--moe-store file: ClickHouse is not selected",
                detail={**detail, "summary": f"--moe-store file; a ClickHouse image that is not the pin is on this Docker host and was left alone: {_describe_found(found)}"},
                **base,
            )
        hint = "" if strict else (
            " To go on without ClickHouse and leave that image alone, pass --moe-store file (the MoE store is then "
            "the file store and the image is recorded as found)."
        )
        return refused(
            "image-digest-differs",
            f"this Docker host holds a ClickHouse image that is not the pinned {ref} (expected digest {digest}): "
            f"{_describe_found(found)}. Nothing was enabled and nothing was pulled. To use ClickHouse, "
            f"{optional_how_to_add(pin_id, pin)}.{hint}",
            found=found,
        )
    if file_mode:
        return Verification(
            outcome=NOT_SELECTED, found=found, reason="--moe-store file: ClickHouse is not selected",
            detail={**detail, "summary": "--moe-store file; no ClickHouse image on this Docker host"}, **base,
        )
    if strict:
        return refused(
            "image-missing",
            f"the image {ref} is not on this Docker host (expected digest {digest}; {inspect_failure}). Load the "
            f"operator-supplied image first -- `docker load -i <tarball>` of an image saved from exactly that digest -- "
            f"and re-run. Nothing was pulled.",
        )
    return Verification(outcome=NOT_CONFIGURED, found=found, reason="not on this Docker host", detail=detail, **base)


def _clickhouse_result(results: "list[Verification]") -> Optional[Verification]:
    return next((r for r in results if r.component == MOE_COMPONENT), None)


def moe_store_selection(results: "list[Verification]", mode: str = MOE_AUTO) -> dict:
    """THE decision (question 260), the only one, of which MoE store the image step records:
    ClickHouse only when the ClickHouse result is `verified` (declared, `--moe-store clickhouse`, or
    optional and found by `--moe-store auto`); the file store for `not-configured` (auto, absent) and
    `not-selected` (`--moe-store file`). Any other outcome, or an outcome the mode cannot produce, cannot
    reach here (a refusal stops the step before a record is written) and raises `AssertionError`.
    "Selected" means recorded: nothing in the installed system reads it yet (`MOE_CONSUMER_NOTE`)."""
    if mode not in MOE_MODES:
        raise ValueError(f"--moe-store {mode!r} is not one of {list(MOE_MODES)}")
    r = _clickhouse_result(results)
    if r is None:
        return {
            "mode": mode, "store": "file", "decided_by": "image step", "consumer": MOE_CONSUMER_NOTE,
            "clickhouse": {"configured": False, "reason": f"the pins file has no {MOE_COMPONENT} component"},
        }
    decided = {"mode": mode, "decided_by": "image step", "consumer": MOE_CONSUMER_NOTE}
    if r.outcome == VERIFIED and mode != MOE_FILE:
        d = r.detail
        return {
            **decided, "store": "clickhouse", "file_store": "available",
            "clickhouse": {
                "configured": True, "reference": d["reference"], "digest": d["digest"], "image_id": d["image_id"],
                "verified_by": d["verified_by"],
                "how_enabled": (
                    f"the pinned image is on this Docker host, verified by digest ({d['verified_by']}); "
                    f"{'--moe-store clickhouse' if mode == MOE_CLICKHOUSE else 'the kit declares it' if r.required else '--moe-store auto'} "
                    f"selected the ClickHouse store, the file store stays available"
                ),
            },
        }
    if r.outcome == NOT_CONFIGURED and mode == MOE_AUTO:
        return {
            **decided, "store": "file",
            "clickhouse": {"configured": False, "reason": r.reason, "how_to_add": r.detail["how_to_add"]},
        }
    if r.outcome == NOT_SELECTED and mode == MOE_FILE:
        return {
            **decided, "store": "file",
            "clickhouse": {
                "configured": False, "selected": False, "outcome": NOT_SELECTED, "reason": r.reason,
                "found": r.found, "how_to_enable": "re-run the image step with --moe-store auto (or clickhouse)",
            },
        }
    raise AssertionError(f"outcome {r.outcome!r} with --moe-store {mode} cannot reach the MoE store decision")


def moe_store_files_install(results: "list[Verification]") -> dict:
    """`INSTALL_RECORD.installed.moe_store`: the files install holds no Docker socket and does not decide the
    store; the image step does (`IMAGE_BUILD_RECORD.moe_store`). Recorded as the file store, ClickHouse not
    configured by this step."""
    r = _clickhouse_result(results)
    declared = r is not None and r.outcome == OTHER_STEP
    how_to_add = (r.detail.get("how_to_add") if r is not None else None) or None
    reason = (
        f"this step holds no Docker socket; ClickHouse was declared at build time (--declare-operator-supplied "
        f"{MOE_COMPONENT}) and only the image step can verify it (install.sh --build-images), which records the store "
        f"it selects in IMAGE_BUILD_RECORD.moe_store"
        if declared else
        "this step holds no Docker socket; ClickHouse is an optional operator-supplied image that only the image step "
        "can verify (install.sh --build-images), which records the store it selects in IMAGE_BUILD_RECORD.moe_store"
    )
    return {
        "store": "file", "decided_by": "image step (install.sh --build-images), not this step",
        "consumer": MOE_CONSUMER_NOTE,
        "clickhouse": {
            "configured": False, "by": "files install", "reason": reason, "declared": declared, "how_to_add": how_to_add,
        },
    }


def render_moe_store(selection: dict) -> str:
    """The one printed `MoE store:` line for a record made by `moe_store_selection` or `moe_store_files_install`."""
    ch = selection["clickhouse"]
    if selection["store"] == "clickhouse":
        repo = ch["reference"].partition("@")[0]
        return f"MoE store: clickhouse ({repo}@{short(ch['digest'])} verified via {ch['verified_by']})"
    if ch.get("selected") is False:
        return f"MoE store: file -- ClickHouse is not selected (--moe-store file); on this Docker host: {_describe_found(ch['found'])}"
    return (
        f"MoE store: file -- ClickHouse is not configured: {ch['reason']}. "
        f"To add it: {ch.get('how_to_add') or 'see docs/kit-operator-supplied.md'}"
    )


# =================================================================================================
# Verifying a tree (GMAT, the files install)
# =================================================================================================

def _describe_pin(pin: dict) -> str:
    return (
        f"content_hash {pin['content_hash']}, {pin['file_count']} files, {pin['total_bytes']} bytes, "
        f"{pin['symlink_count']} symlinks (listing hash {pin['symlink_listing_hash']})"
    )


def _variant_keys(variants: Optional[dict]) -> dict[str, tuple[str, str]]:
    """`{pin key: (platform, variant name)}` for every variant's pin that is not its platform's own key
    (question 257: `linux-x86_64-rhel7` -> `("linux-x86_64", "rhel7")`). Such a key is a pin, not a
    platform: it is chosen with `--<pack>-variant`, never with `--<pack>-platform`."""
    out: dict[str, tuple[str, str]] = {}
    for platform, entry in (variants or {}).items():
        for name, key in entry["pins"].items():
            if key != platform:
                out[key] = (platform, name)
    return out


def _describe_pins(platforms: dict, variants: Optional[dict] = None, pack: str = "gmat") -> str:
    alias = _variant_keys(variants)

    def label(key: str) -> str:
        if key in alias:
            of, name = alias[key]
            return f"{key} (the {name} variant of {of}: --{pack}-platform {of} --{pack}-variant {name})"
        if variants and key in variants:
            return f"{key} (the {variants[key]['default']} variant, the default)"
        return key

    pinned = {k: v for k, v in sorted(platforms.items()) if v is not None}
    return "; ".join(f"{label(k)}: {_describe_pin(v)}" for k, v in pinned.items()) or "(none)"


def verify_gmat_tree(
    pin_id: str, pin: dict, requirement: Requirement, *,
    entry: Optional[dict], root: Optional[str], platform: Optional[str], variant: Optional[str] = None,
) -> Verification:
    """The operator's unpacked tree for a `data-tree` pin (GMAT), measured under the pin's `scope` and
    compared with the pin recorded for `platform` and, on a platform with `variants` (question 257:
    Linux x86_64, `ubuntu` by default or `rhel7`), for the chosen `variant` only. `entry` is the kit's
    `packs.<pack>` entry. Reads nothing but `root`, which it never writes or copies. Refusal order,
    cheapest first, the hash last: the kit's marker, the pin, no root, root not a directory, no
    platform, a variant's pin key given as the platform, no pin for the platform, a variant on a
    platform without variants or an unknown variant, then the measurement. A tree that differs from the
    chosen pin and equals another variant's pin is refused naming that variant and the option to pass
    (`<pack>-matches-other-variant`); one that equals none is `<pack>-descriptor-differs`. Codes are
    `<pack>-...` (`gmat-root-missing`, `gmat-descriptor-differs`, ...)."""
    pack = pin["pack"]
    base = dict(
        component=pin_id, item=pin["item"], kind=pin["kind"], step=STEP_FILES, required=True,
        requirement=requirement.text,
    )

    def refused(code: str, reason: str, expected: Any = None, found: Any = None, detail: Optional[dict] = None) -> Verification:
        return Verification(outcome=REFUSED, code=f"{pack}-{code}", reason=reason, expected=expected, found=found,
                            detail=detail or {}, **base)

    marker = (entry or {}).get("operator_supplied")
    if (entry or {}).get("copied") or not isinstance(marker, dict) or marker.get("component") != pin_id:
        return refused(
            "kit-not-operator-supplied",
            f"KIT_MANIFEST's packs.{pack} is not marked operator-supplied (copied={(entry or {}).get('copied')!r}, "
            f"operator_supplied={marker!r}) -- a kit built before question 250(a), task 2, or by another "
            f"builder; this installer will not skip the check for it. Rebuild the kit with --with-pack {pack}.",
        )
    platforms, scope, variants = pin.get("platforms"), pin.get("scope"), pin.get("variants")
    problems = (kit_manifest.pin_problems(platforms) + kit_manifest.variant_problems(variants, platforms)
                + kit_manifest.scope_problems(scope))
    if problems:
        return refused("pin-malformed", f"the installer's pins file: {pin_id!r}: " + "; ".join(problems))
    variants = variants or {}
    alias = _variant_keys(variants)
    item = pin.get("item")
    pinned = {k: v for k, v in platforms.items() if v is not None}
    flag, pflag, vflag = f"--{pack}-root", f"--{pack}-platform", f"--{pack}-variant"
    if root is None:
        return refused(
            "root-missing",
            f"this kit needs GMAT R2026a (operator-supplied: docs/kit-operator-supplied.md item {item}); the kit "
            f"does not carry it. Unpack GMAT R2026a for the target platform and pass {flag} PATH and "
            f"{pflag} KEY (and, on a platform with variants, {vflag} NAME). Pinned platforms: "
            f"{_describe_pins(platforms, variants, pack)}",
            expected={k: v for k, v in sorted(pinned.items())},
        )
    if not Path(root).is_dir():
        return refused("root-not-a-directory", f"{flag} {root!r} is not a directory")
    if platform is None:
        return refused(
            "platform-missing",
            f"{pflag} KEY is required with {flag} (never guessed from this host: the installer may "
            f"run in a Linux container over a macOS tree). Pinned platforms: {_describe_pins(platforms, variants, pack)}",
        )
    if platform in alias:
        of, name = alias[platform]
        return refused(
            "platform-is-a-variant",
            f"{platform!r} is the pin of the {name} variant of {of}, not a platform key: pass {pflag} {of} {vflag} {name}",
        )
    if platforms.get(platform) is None:
        return refused(
            "no-pin-for-platform",
            f"no GMAT pin is recorded for platform {platform!r}. Platforms with a pin: "
            f"{sorted(k for k in pinned if k not in alias)}; known platforms without one: "
            f"{sorted(k for k, v in platforms.items() if v is None)}. A pin for a new "
            f"platform is added when a kit is built for it (scripts/kit/README.md, 'GMAT is operator-supplied')",
        )
    choices = variants.get(platform)
    if choices is None:
        if variant is not None:
            return refused(
                "variant-not-applicable",
                f"{vflag} {variant!r} was given, but platform {platform!r} has one GMAT pin and no variants "
                f"(platforms with variants: {sorted(variants) or 'none'}). Drop {vflag}.",
            )
        chosen, chosen_by, pin_key = None, None, platform
    else:
        chosen = choices["default"] if variant is None else variant
        chosen_by = "default" if variant is None else "option"
        if chosen not in choices["pins"]:
            return refused(
                "variant-unknown",
                f"{vflag} {variant!r} is not a variant of {platform}: its variants are {sorted(choices['pins'])} "
                f"(default {choices['default']!r})",
            )
        pin_key = choices["pins"][chosen]
    pin_for_platform = platforms[pin_key]
    label = platform if chosen is None else f"{platform} {chosen} ({pin_key}, {'the default' if chosen_by == 'default' else vflag + ' ' + chosen})"
    print(f"verifying GMAT at {root!r} against the {label} pin (reads every file once) ...", file=sys.stderr, flush=True)
    try:
        found = kit_manifest.measure_tree(Path(root), scope)
    except OSError as e:
        return refused("root-unreadable", f"{flag} {root!r} could not be read ({type(e).__name__}: {e})",
                       expected=pin_for_platform)
    differs = kit_manifest.compare_to_pin(found, pin_for_platform)
    if differs:
        matches = [
            (name, key) for name, key in sorted((choices or {}).get("pins", {}).items())
            if key != pin_key and kit_manifest.compare_to_pin(found, platforms[key]) == []
        ]
        if matches:
            name, key = matches[0]
            return refused(
                "matches-other-variant",
                f"the GMAT tree at {root!r} is the {name} variant of {platform}: it equals the {key} pin "
                f"({_describe_pin(platforms[key])}). The {chosen} variant was chosen "
                f"({'the default' if chosen_by == 'default' else vflag + ' ' + chosen}), whose pin {pin_key} it is not "
                f"({_describe_pin(pin_for_platform)}). To install against this tree, pass {pflag} {platform} "
                f"{vflag} {name}. Not installing against a variant that was not chosen.",
                expected=pin_for_platform, found=found,
                detail={"platform": platform, "variant": chosen, "matches_variant": name, "matches_pin": key},
            )
        return refused(
            "descriptor-differs",
            f"the GMAT tree at {root!r} is not what the {label} pin records (differs in "
            f"{', '.join(differs)}). expected: {_describe_pin(pin_for_platform)}. found: {_describe_pin(found)}. Not "
            f"installing against a GMAT that is not the pinned release.",
            expected=pin_for_platform, found=found,
        )
    record = {
        "required": True, "verified": True, "copied": False,
        "component": pin_id, "item": item,
        "platform": platform, "variant": chosen, "variant_chosen_by": chosen_by, "pin_key": pin_key,
        "root": root,
        "found": found, "pin": pin_for_platform, "scope": scope,
        "not_covered": pin.get("not_covered", []),
    }
    summary = f"{platform}{'' if chosen is None else ' (' + chosen + ')'} {short(found['content_hash'])} {found['file_count']} files"
    return Verification(
        outcome=VERIFIED, expected=pin_for_platform, found=found,
        detail={"platform": platform, "variant": chosen, "pin_key": pin_key, "root": root, "summary": summary,
                "record": record},
        **base,
    )


# =================================================================================================
# Verifying an operator-supplied wheel (the files install)
# =================================================================================================

def sha256_of_file(path: Path) -> str:
    """Lower-case hex sha256 of a file, read in 1 MiB chunks."""
    h = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def verify_python_wheel(
    pin_id: str, pin: dict, requirement: Requirement, *, entry: Optional[dict], directory: Optional[str],
) -> Verification:
    """The operator's copy of a wheel the kit does not carry (`wheels.operator_supplied`, module doc):
    `directory/<pin filename>` must exist and its sha256 must be the pin's. `entry` is the kit's
    `wheels.operator_supplied` entry for this component. Refusal order, cheapest first, the hash last:
    the kit's entry against the pin, no directory, not a directory, no such file, unreadable, hash differs.
    Codes are `wheel-...`. Reads one file and never writes or copies it (the installer copies it itself,
    and re-hashes the copy)."""
    filename, digest = pin["filename"], pin["sha256"]
    want = digest.partition(":")[2]
    flag = "--operator-wheels"
    base = dict(
        component=pin_id, item=pin["item"], kind=pin["kind"], step=STEP_FILES, required=True,
        requirement=requirement.text, expected=digest,
    )

    def refused(code: str, reason: str, found: Any = None) -> Verification:
        return Verification(outcome=REFUSED, code=f"wheel-{code}", reason=reason, found=found, **base)

    libraries = ", ".join((entry or {}).get("gpl_family_libraries") or []) or "a GPL-family library"
    if not isinstance(entry, dict) or entry.get("filename") != filename or entry.get("sha256") != want:
        return refused(
            "kit-entry-differs",
            f"KIT_MANIFEST's wheels.operator_supplied entry for {pin_id!r} is {entry!r}, but the installer's pins file "
            f"pins {filename} {digest}: the kit was built against another pin than this installer carries. Rebuild "
            f"the kit, or use the installer of the kit that was built with that pin.",
        )
    need = (
        f"this kit needs {filename} ({pin['dist']} {pin['version']}), which it does not carry: the wheel bundles "
        f"{libraries} under a GPL-family licence, and the kit redistributes no GPL or LGPL code: the operator supplies it "
        f"(docs/kit-operator-supplied.md). Download exactly that file ({pin['reference']}), put it in a directory "
        f"and pass {flag} DIR. Pinned sha256: {digest}"
    )
    if directory is None:
        return refused("dir-missing", f"{need}")
    if not Path(directory).is_dir():
        return refused("dir-not-a-directory", f"{flag} {directory!r} is not a directory. {need}")
    path = Path(directory) / filename
    if not path.is_file():
        present = sorted(p.name for p in Path(directory).glob("*.whl"))
        return refused("missing", f"{path} does not exist (wheels in {directory!r}: {present or 'none'}). {need}")
    try:
        found = sha256_of_file(path)
    except OSError as e:
        return refused("unreadable", f"{path} could not be read ({type(e).__name__}: {e})")
    if found != want:
        return refused(
            "hash-differs",
            f"{path} has sha256:{found}, not the pinned {digest}: it is not the wheel this kit was built against. "
            f"Download {filename} again from the source the pin names ({pin['reference']}) and re-run.",
            found=f"sha256:{found}",
        )
    return Verification(
        outcome=VERIFIED, found=f"sha256:{found}",
        detail={"filename": filename, "path": str(path), "sha256": found, "summary": f"{filename} {short(digest)}"}, **base,
    )
