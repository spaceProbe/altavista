"""scripts/kit/licences.py -- D2 (docs/compliance/sbom): the SPDX license-expression evaluator
and the deny.toml allow-list check.

Library only, stdlib-only: never prints, never exits, never reads the process environment.
Two responsibilities that are always used together, kept in one small module:

1. **Decision E** -- parse `deny.toml`'s `[licenses].allow` list with `tomllib`, so the
   licence policy this module enforces has exactly one source of truth (never re-typed by hand
   in a second place). `deny.toml` itself is never edited by this task (the licence policy is
   the lead's decision, not this task's) -- this module only *reads* it.

2. **Decision F** -- a small, explicit recursive-descent SPDX license-expression evaluator (not
   a fuzzy string match): tokenise, treat `/` as a legacy synonym for `OR`, handle parentheses,
   `AND`, `OR`, and `WITH` (an id plus a `WITH` clause is one leaf whose exact text must be in
   the allow list -- e.g. `"Apache-2.0 WITH LLVM-exception"` is in `deny.toml`'s allow list
   verbatim). An expression is satisfied when some allowed assignment exists: `OR` is satisfied
   if either side is; `AND` only if both are.

Non-SPDX free text (a handful of older Python packages still declare a plain-English `License`
field instead of a PEP 639 `License-Expression`) is normalised through the small, explicit,
commented `FREE_TEXT_ALIASES` table below -- never guessed by fuzzy matching. A string that
neither parses as an SPDX expression nor aliases to one is returned as its own finding
(`reason="unparseable"`), never silently dropped.
"""
from __future__ import annotations

import re
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import FrozenSet, Optional, Union


def load_allow_list(deny_toml_path: Path) -> frozenset[str]:
    """The one place `deny.toml`'s `[licenses].allow` is read from. Decision E: "parses
    deny.toml's [licenses].allow list with tomllib (so the policy has exactly one source of
    truth)"."""
    with open(deny_toml_path, "rb") as f:
        data = tomllib.load(f)
    return frozenset(data["licenses"]["allow"])


# --- Non-SPDX free-text alias table (Decision F) ---------------------------------------------
# Explicit and commented, never fuzzy-matched. Populated from what this workspace's own Python
# venv actually reports (`importlib.metadata` over `.venv`), not guessed at: `protobuf` (7.36.1)
# declares the classic setuptools free-text `License` field "3-Clause BSD License", and
# `uvloop` (0.22.1) declares "MIT License" -- both pre-PEP-639 packages with no
# `License-Expression` field at all. Any string that is neither a real SPDX token nor listed
# here is left unparsed and reported as its own finding (see `evaluate_license_field` below)
# rather than guessed at.
FREE_TEXT_ALIASES: dict[str, str] = {
    "MIT License": "MIT",
    "3-Clause BSD License": "BSD-3-Clause",
}


# --- A ~60-line recursive-descent SPDX expression parser + evaluator (Decision F) ------------

@dataclass(frozen=True)
class Leaf:
    """One license id, or one `id WITH exception-id` pair (the whole `WITH` clause is a single
    leaf -- its exact text, e.g. "Apache-2.0 WITH LLVM-exception", must appear in the allow
    list verbatim)."""
    text: str


@dataclass(frozen=True)
class And:
    left: "SpdxNode"
    right: "SpdxNode"


@dataclass(frozen=True)
class Or:
    left: "SpdxNode"
    right: "SpdxNode"


SpdxNode = Union[Leaf, And, Or]


class SpdxParseError(ValueError):
    """Raised when a string is neither a well-formed SPDX expression nor empty."""


#: Parens and the legacy `/` (OR) synonym are their own tokens; everything else runs of
#: non-space/non-paren/non-slash characters (which is what a license id, `AND`/`OR`/`WITH`, and
#: an exception id all look like) is one token.
_TOKEN_RE = re.compile(r"\(|\)|/|[^\s()/]+")


def tokenize(expr: str) -> list[str]:
    return _TOKEN_RE.findall(expr)


def parse_spdx(expr: str) -> SpdxNode:
    """Tokenise and parse a (possibly legacy-slash) SPDX license expression into a small AST.

    Grammar (AND/OR/WITH case-insensitive; '/' is a legacy synonym for OR):
        or_expr  := and_expr (OR and_expr)*
        and_expr := primary (AND primary)*
        primary  := '(' or_expr ')' | LICENSE-ID ('WITH' EXCEPTION-ID)?
    """
    raw_tokens = tokenize(expr)
    if not raw_tokens:
        raise SpdxParseError(f"empty license expression: {expr!r}")
    tokens = ["OR" if t == "/" else t for t in raw_tokens]
    pos = 0

    def peek() -> Optional[str]:
        return tokens[pos] if pos < len(tokens) else None

    def advance() -> str:
        nonlocal pos
        tok = tokens[pos]
        pos += 1
        return tok

    def is_op(tok: Optional[str], op: str) -> bool:
        return tok is not None and tok.upper() == op

    def parse_or() -> SpdxNode:
        node = parse_and()
        while is_op(peek(), "OR"):
            advance()
            node = Or(node, parse_and())
        return node

    def parse_and() -> SpdxNode:
        node = parse_primary()
        while is_op(peek(), "AND"):
            advance()
            node = And(node, parse_primary())
        return node

    def parse_primary() -> SpdxNode:
        tok = peek()
        if tok is None:
            raise SpdxParseError(f"unexpected end of license expression: {expr!r}")
        if tok == "(":
            advance()
            node = parse_or()
            if peek() != ")":
                raise SpdxParseError(f"unbalanced parentheses in license expression: {expr!r}")
            advance()
            return node
        if tok == ")" or is_op(tok, "AND") or is_op(tok, "OR") or is_op(tok, "WITH"):
            raise SpdxParseError(f"unexpected token {tok!r} in license expression: {expr!r}")
        license_id = advance()
        if is_op(peek(), "WITH"):
            advance()
            exc_tok = peek()
            if exc_tok is None or exc_tok in ("(", ")"):
                raise SpdxParseError(f"'WITH' with no exception id: {expr!r}")
            exception_id = advance()
            return Leaf(f"{license_id} WITH {exception_id}")
        return Leaf(license_id)

    node = parse_or()
    if peek() is not None:
        raise SpdxParseError(f"trailing tokens after license expression: {expr!r}")
    return node


@dataclass(frozen=True)
class EvalResult:
    satisfied: bool
    #: The specific leaf license strings responsible for `satisfied` being False. Always empty
    #: when `satisfied` is True.
    unmet_leaves: frozenset[str]


def evaluate_node(node: SpdxNode, allow: FrozenSet[str]) -> EvalResult:
    if isinstance(node, Leaf):
        ok = node.text in allow
        return EvalResult(ok, frozenset() if ok else frozenset({node.text}))
    if isinstance(node, And):
        left = evaluate_node(node.left, allow)
        right = evaluate_node(node.right, allow)
        ok = left.satisfied and right.satisfied
        return EvalResult(ok, frozenset() if ok else (left.unmet_leaves | right.unmet_leaves))
    if isinstance(node, Or):
        left = evaluate_node(node.left, allow)
        right = evaluate_node(node.right, allow)
        ok = left.satisfied or right.satisfied
        return EvalResult(ok, frozenset() if ok else (left.unmet_leaves | right.unmet_leaves))
    raise TypeError(f"unknown SPDX AST node: {node!r}")  # pragma: no cover -- exhaustive above


def evaluate_expression(expr: str, allow: FrozenSet[str]) -> EvalResult:
    """Parse + evaluate in one call -- the direct entry point the evaluator's own unit tests
    (Decision F) drive against real expressions."""
    return evaluate_node(parse_spdx(expr), allow)


# --- Typed findings: one entry point tying it all together for the SBOM generator/tests ------

@dataclass(frozen=True)
class LicenseFinding:
    """One (component, package, version, licence) combination NOT covered by deny.toml's allow
    list -- Decision E's "typed findings". `reason` is `"not-allowed"` (parsed fine, some leaf
    license is not in the allow list) or `"unparseable"` (neither a valid SPDX expression nor a
    known free-text alias)."""
    component: str
    package: str
    version: str
    licence: str
    reason: str


def evaluate_license_field(
    component: str,
    package: str,
    version: str,
    raw_license: Optional[str],
    allow: FrozenSet[str],
) -> list[LicenseFinding]:
    """Normalise (`FREE_TEXT_ALIASES`) and evaluate one package's raw license metadata string
    against `allow`. Returns an empty list when the license is fully covered (an SPDX `OR` is
    satisfied if either side is -- nothing to report). A missing (`None`/empty) license is NOT a
    finding here: the SBOM generator records that case as an explicit "no licence metadata"
    component property instead -- this function only evaluates licenses the SBOM actually
    declares."""
    if not raw_license:
        return []
    text = FREE_TEXT_ALIASES.get(raw_license, raw_license)
    try:
        result = evaluate_expression(text, allow)
    except SpdxParseError:
        return [LicenseFinding(component, package, version, raw_license, "unparseable")]
    if result.satisfied:
        return []
    return [
        LicenseFinding(component, package, version, leaf, "not-allowed")
        for leaf in sorted(result.unmet_leaves)
    ]
