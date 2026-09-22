#!/usr/bin/env python3
"""Discover workspace targets that declare `required-features` and group them for clippy.

Used by scripts/lint/required_features_clippy.sh (question 231, heavy round 6 task 7):
`cargo clippy --workspace --all-targets` never builds a `[[bin]]`, `[[example]]`, `[[test]]`
or `[[bench]]` that declares `required-features` unless those features are explicitly
enabled, so such a target goes unlinted by the workspace's own named gate. This script reads
`cargo metadata --no-deps --format-version=1` output (already run, saved to a file, and
passed as this script's only argument -- this script itself does not invoke cargo and needs
no network) and prints one line per (package, feature set) group, so the caller can run
`cargo clippy -p <package> --features <feature-set> --all-targets -- -D warnings` for each.

Stdlib only, no dependency. Output format (tab-separated, one group per line):
    <package>\t<feature1,feature2,...>\t<target1:kind1;target2:kind2;...>
Features and targets within a line are sorted/ordered deterministically so the output is
stable across runs of the same metadata.
"""
import json
import sys


def discover_groups(metadata: dict) -> list[tuple[str, tuple[str, ...], list[str]]]:
    """Return [(package_name, sorted_feature_tuple, [target:kind, ...]), ...].

    Only workspace members are considered (cargo metadata --no-deps already limits
    `packages` to workspace members and their path dependents, but we filter by
    `workspace_members` explicitly so a future `--no-deps` behaviour change cannot
    silently widen or narrow this without the filter still doing its job).
    """
    ws_members = set(metadata["workspace_members"])

    # (package, feature tuple) -> list of "target:kind" strings, in discovery order.
    groups: dict[tuple[str, tuple[str, ...]], list[str]] = {}
    order: list[tuple[str, tuple[str, ...]]] = []

    for pkg in metadata["packages"]:
        if pkg["id"] not in ws_members:
            continue
        for target in pkg["targets"]:
            features = target.get("required-features") or []
            if not features:
                continue
            key = (pkg["name"], tuple(sorted(features)))
            kinds = "+".join(target.get("kind", []))
            entry = f"{target['name']}:{kinds}"
            if key not in groups:
                groups[key] = []
                order.append(key)
            groups[key].append(entry)

    return [(pkg_name, features, groups[(pkg_name, features)]) for pkg_name, features in order]


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: required_features_clippy.py <cargo-metadata-json-file>", file=sys.stderr)
        return 2

    with open(argv[1], encoding="utf-8") as fh:
        metadata = json.load(fh)

    for pkg_name, features, targets in discover_groups(metadata):
        print(f"{pkg_name}\t{','.join(features)}\t{';'.join(targets)}")

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
