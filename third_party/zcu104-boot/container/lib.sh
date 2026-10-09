# container/lib.sh -- shared by the three container phases (sourced; runs inside the pinned Debian
# container as root). Not meant to be run from the host.

# Fixed environment: nothing locale-, zone- or umask-dependent reaches an artifact.
export LC_ALL=C TZ=UTC DEBIAN_FRONTEND=noninteractive
umask 022

. /recipe/pinned-inputs.sh

# The manifest hash of a directory tree (same idea as third_party/rtems-container/build-elf.sh):
# sorted relative paths; `F <sha256>  <path>` for a regular file, `X ...` for an executable one,
# `L <target>  <path>` for a symlink; directories are implied; anything else is an error. A
# `.git` directory at the top level is not part of the tree; extra arguments are path prefixes left out.
tree_manifest_hash() {
    python3 -I - "$@" <<'PY'
import hashlib, os, sys

root = sys.argv[1]
excludes = tuple(sys.argv[2:])
entries = []
for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
    if dirpath == root and ".git" in dirnames:
        dirnames.remove(".git")
    for name in dirnames + filenames:
        full = os.path.join(dirpath, name)
        rel = os.path.relpath(full, root)
        if excludes and rel.startswith(excludes):
            continue
        if os.path.islink(full):
            entries.append((rel, "L " + os.readlink(full) + "  " + rel))
        elif os.path.isfile(full):
            h = hashlib.sha256()
            with open(full, "rb") as f:
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
            tag = "X" if os.access(full, os.X_OK) else "F"
            entries.append((rel, tag + " " + h.hexdigest() + "  " + rel))
        elif not os.path.isdir(full):
            sys.exit("unsupported file type in manifest: " + full)
entries.sort(key=lambda e: e[0].encode("utf-8", "surrogateescape"))
text = "".join(line + "\n" for _, line in entries)
print(hashlib.sha256(text.encode("utf-8", "surrogateescape")).hexdigest())
PY
}

# Install the pinned .deb files from the cache with dpkg (no network) and check the complete
# installed set against DPKG_SET_SHA256.
install_pinned_debs() {
    local want got
    # python3-minimal and python3 Pre-Depend on python3.11-minimal: the first pass unpacks and
    # configures that, so those two are refused in it; a second pass installs them, then
    # everything is configured.
    dpkg -i /cache/apt/*.deb >/tmp/dpkg-install.log 2>&1 || true
    dpkg -i /cache/apt/python3-minimal_*.deb >>/tmp/dpkg-install.log 2>&1 \
        || { tail -30 /tmp/dpkg-install.log >&2; echo "dpkg -i (python3-minimal) failed" >&2; exit 3; }
    dpkg -i /cache/apt/python3_*.deb >>/tmp/dpkg-install.log 2>&1 \
        || { tail -30 /tmp/dpkg-install.log >&2; echo "dpkg -i (python3) failed" >&2; exit 3; }
    dpkg --configure -a >>/tmp/dpkg-install.log 2>&1 \
        || { tail -30 /tmp/dpkg-install.log >&2; echo "dpkg --configure failed" >&2; exit 3; }
    got="$(dpkg-query -W -f='${Package}:${Architecture}=${Version}\n' | LC_ALL=C sort | sha256sum | cut -d' ' -f1)"
    echo "container dpkg set sha256: ${got}"
    if [ "${got}" != "${DPKG_SET_SHA256}" ]; then
        echo "container package set ${got} != pinned ${DPKG_SET_SHA256}" >&2
        exit 3
    fi
}
