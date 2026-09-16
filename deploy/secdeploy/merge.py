"""Merge the AltaVista fragment into the user's secdeploy manifest — without ever touching
secdeploy itself (question 213(a): secdeploy is run, not vendored or edited).

``suite.merged.toml`` is built as the EXACT bytes of the base manifest, followed by a marker
comment line, followed by the AltaVista component tables rendered from
``suite.altavista.toml`` with each component's ADR-003 tier rewritten through ``[tier_compat]``
into one of secdeploy's own five tiers. That shape makes two things trivial to prove (see
``split_merged`` and ``tests/test_suite_declarations.py``):

* the base manifest survives the merge byte-for-byte (nothing about the user's own suite.toml
  is reformatted, reordered, or reflowed);
* "unmerging" — recovering the exact base text back out of the merged file — is a pure string
  operation on the marker, no re-parsing/re-serializing round-trip that could silently drop or
  reorder something.

Stdlib only (``tomllib``, no third-party TOML writer): the output is built as plain text, not
serialized from a parsed structure, specifically so the base file's own formatting/comments are
preserved exactly rather than reconstructed.
"""

from __future__ import annotations

import argparse
import tomllib
from pathlib import Path

# A single, greppable marker line the merged file uses to separate "the user's own suite.toml,
# untouched" from "what we rendered from suite.altavista.toml". Chosen to look like a TOML
# comment (so it's inert to any TOML parser) and to be distinctive enough that it will never
# collide with a real line in either input.
MARKER = (
    "# ==== AltaVista fragment begins here (rendered by deploy/secdeploy/merge.py from "
    "deploy/secdeploy/suite.altavista.toml — edit that file, not this one) ===="
)

DEFAULT_BASE = "/Users/probe/code/secdeploy/suite.toml"
DEFAULT_FRAGMENT = "deploy/secdeploy/suite.altavista.toml"

# Characters that would break naive `f'...{value}...'` interpolation into a TOML basic string
# (an unescaped double-quote ends the string early; a backslash starts an escape sequence TOML
# would try to interpret; a raw newline is not legal inside a basic string at all). We deliberately
# do NOT escape these — a component field is authored data, not user input in need of sanitizing,
# so a bad value here is a fragment bug to fix at the source, not paper over. See
# `_check_field_is_toml_safe` and `FragmentRenderError`.
_UNSAFE_TOML_CHARS = '"\\\n\r'


class FragmentRenderError(ValueError):
    """Raised by :func:`render_components` when the fragment cannot be safely rendered:

    * a component's ADR-003 tier has no ``[tier_compat]`` entry (``map_tiers=True``) — every
      AltaVista tier must be explicitly mapped, never fall through to an implicit identity
      mapping (see ``suite.altavista.toml``'s ``[tier_compat]`` header comment); or
    * a component field (``repo``/``ref``/``kind``/``tier``/``runtime``/``role``) contains a
      character that would break unescaped TOML string interpolation — a literal ``"``, ``\\``,
      or newline (see ``_UNSAFE_TOML_CHARS`` above).
    """


def _check_field_is_toml_safe(component: str, field: str, value: str) -> str:
    for ch in _UNSAFE_TOML_CHARS:
        if ch in value:
            raise FragmentRenderError(
                f"component {component!r} field {field!r} contains {ch!r}, which would break "
                f"unescaped TOML string interpolation in render_components — fix the source "
                f"value in suite.altavista.toml instead of escaping it here"
            )
    return value


def load_fragment(path: str | Path) -> dict:
    """Parse ``suite.altavista.toml`` (or an edited copy of it) with the stdlib TOML reader."""
    path = Path(path)
    return tomllib.loads(path.read_text())


def render_components(fragment: dict, map_tiers: bool = True) -> str:
    """Render the fragment's ``[components.*]`` tables as TOML text, in the fragment's own
    declared order (``tomllib.loads`` preserves file order in the returned dict, and so does
    this function — no sorting, no timestamps, no absolute paths).

    When ``map_tiers`` is true (the default), each component's ADR-003 tier is rewritten
    through ``fragment["tier_compat"]`` into one of secdeploy's five tiers — this is what makes
    the merged manifest something ``Manifest.load`` accepts. A tier with no ``[tier_compat]``
    entry is a :class:`FragmentRenderError`, never an implicit identity mapping (a component
    silently falling through unmapped would only be caught later, and confusingly, by
    secdeploy's own tier-rejection error). ``map_tiers=False`` renders the raw ADR-003 tier
    names unchanged instead, which is exactly what proves secdeploy rejects them today (see
    ``test_adr003_tiers_are_still_rejected_upstream`` and ``docs/secdeploy-upstream.md``) — it
    is a deliberate escape hatch for that one gap-proof test, not a normal merge mode.

    Every interpolated field is checked with :func:`_check_field_is_toml_safe` first: a value
    containing an unescaped ``"``, ``\\``, or newline raises :class:`FragmentRenderError` rather
    than being escaped — see that function's own docstring for why.
    """
    tier_compat: dict = fragment.get("tier_compat") or {}
    components: dict = fragment.get("components") or {}

    lines = [
        "# ---- AltaVista components ----",
        "# Rendered from deploy/secdeploy/suite.altavista.toml by deploy/secdeploy/merge.py.",
        "# Do not hand-edit this section of a merged file; it is regenerated on every merge.",
        "",
    ]
    for name, c in components.items():
        def _safe(field: str, value: str) -> str:
            return _check_field_is_toml_safe(name, field, value)

        tier = c.get("tier", "")
        if map_tiers:
            if tier not in tier_compat:
                raise FragmentRenderError(
                    f"component {name!r}: tier {tier!r} has no [tier_compat] entry — add one "
                    f"rather than letting it fall through to an implicit identity mapping"
                )
            tier = tier_compat[tier]
        block = [
            f"[components.{name}]",
            f'repo = "{_safe("repo", c.get("repo", ""))}"',
            f'ref = "{_safe("ref", c.get("ref", ""))}"',
            f'kind = "{_safe("kind", c.get("kind", "service"))}"',
            f'tier = "{_safe("tier", tier)}"',
        ]
        port = int(c.get("port", 0) or 0)
        if port:
            block.append(f"port = {port}")
        if c.get("optional"):
            block.append("optional = true")
        if c.get("experimental"):
            block.append("experimental = true")
        if c.get("fronted"):
            block.append("fronted = true")
        if c.get("runtime"):
            block.append(f'runtime = "{_safe("runtime", c["runtime"])}"')
        block.append(f'role = "{_safe("role", c.get("role", ""))}"')
        lines.extend(block)
        lines.append("")
    text = "\n".join(lines).rstrip("\n") + "\n"
    return text


def split_merged(text: str) -> tuple[str, str]:
    """Inverse of the merge: split a merged manifest's text back into ``(base_text,
    altavista_text)`` on the marker line. ``base_text`` is exactly the base manifest's own
    bytes (as a str) — see ``merge``'s construction, which inserts exactly one blank line
    between the base text and the marker, and exactly one blank line between the marker and
    the rendered AltaVista text; both are removed here so the round-trip is exact.
    """
    idx = text.index(MARKER)
    # `merge()` writes: base_text + "\n" + MARKER + "\n\n" + altavista_text. The "\n" right
    # before MARKER is the one separator byte to strip back off; base_text itself already ends
    # with its own trailing newline (the base file's own).
    base_text = text[: idx - 1] if text[idx - 1 : idx] == "\n" else text[:idx]
    after = text[idx + len(MARKER) :]
    if after.startswith("\n\n"):
        after = after[2:]
    else:
        after = after.lstrip("\n")
    return base_text, after


def merge(
    base: str | Path = DEFAULT_BASE,
    fragment: str | Path = DEFAULT_FRAGMENT,
    site: str | Path | None = None,
    out: str | Path = ".",
    map_tiers: bool = True,
) -> Path:
    """Write ``<out>/suite.merged.toml``, ``<out>/secsite.merged.toml`` (an exact byte copy of
    ``site``), and ``<out>/deploy`` (a symlink to the base manifest's own ``deploy/``
    directory, replacing it if it already exists). Returns the path to ``suite.merged.toml``.

    Deterministic: no timestamps, no absolute paths written into the output, a trailing
    newline, and the fragment's own declared component order — running this twice into two
    different ``out`` directories produces byte-identical ``suite.merged.toml`` files.
    """
    base_path = Path(base)
    fragment_path = Path(fragment)
    out_dir = Path(out)
    out_dir.mkdir(parents=True, exist_ok=True)

    base_bytes = base_path.read_bytes()
    base_text = base_bytes.decode("utf-8")
    if not base_text.endswith("\n"):
        base_text += "\n"

    frag = load_fragment(fragment_path)
    rendered = render_components(frag, map_tiers=map_tiers)

    merged_text = base_text + "\n" + MARKER + "\n\n" + rendered
    if not merged_text.endswith("\n"):
        merged_text += "\n"
    (out_dir / "suite.merged.toml").write_text(merged_text, encoding="utf-8")

    if site is not None:
        site_bytes = Path(site).read_bytes()
        (out_dir / "secsite.merged.toml").write_bytes(site_bytes)

    deploy_link = out_dir / "deploy"
    if deploy_link.is_symlink() or deploy_link.exists():
        deploy_link.unlink()
    # A relative-looking source path would resolve against out_dir at read time (correct for a
    # symlink either way), but resolving it explicitly here — against the BASE manifest's own
    # location, not the current working directory — is what makes this correct regardless of
    # where `merge()` is called from.
    deploy_link.symlink_to(base_path.resolve().parent / "deploy")

    return out_dir / "suite.merged.toml"


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--base", default=DEFAULT_BASE, help="base secdeploy suite.toml")
    p.add_argument("--fragment", default=DEFAULT_FRAGMENT, help="the AltaVista fragment")
    p.add_argument("--site", default=None, help="one of our standalone site files")
    p.add_argument("--out", required=True, help="output directory")
    p.add_argument(
        "--no-map-tiers", dest="map_tiers", action="store_false",
        help="render raw ADR-003 tier names instead of mapping through [tier_compat] "
             "(for proving secdeploy rejects them — see docs/secdeploy-upstream.md)",
    )
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    out_path = merge(
        base=args.base, fragment=args.fragment, site=args.site, out=args.out,
        map_tiers=args.map_tiers,
    )
    print(f"wrote {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
