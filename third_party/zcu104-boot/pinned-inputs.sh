# third_party/zcu104-boot/pinned-inputs.sh -- every input of build-boot-bin.sh that is fetched or
# installed, pinned. Sourced (bash) by build-boot-bin.sh and by the container scripts; parsed by
# tests/test_build_boot_bin_script.py. A pin is the exact version or commit AND a content hash; the
# hash is what is checked after every fetch, so a mirror that serves different bytes fails loudly.
# Hashes marked "self-measured" were computed by the person who pinned them on the first fetch
# (2026-10-07), there being no publisher-signed digest to compare with; the GNU/sourceware
# tarballs' values were also recognised against the values widely published for them.

# ---- builder image: debian:bookworm-slim by content digest (arm64), never a tag.
BASE_IMAGE="debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171"

# ---- apt: the closure `apt-get install --no-install-recommends build-essential git ca-certificates
# curl python3 xz-utils bzip2 libssl-dev m4 patch file` added or upgraded on that image on
# 2026-10-07, name=version. Phase 1 (network) downloads exactly these .deb files; every file is
# checked against apt-debs.sha256; phases 2 and 3 (no network) install them with dpkg.
APT_PACKAGES=(
    binutils-aarch64-linux-gnu=2.40-2
    binutils-common=2.40-2
    binutils=2.40-2
    build-essential=12.9
    bzip2=1.0.8-5+b1
    ca-certificates=20250419~deb12u1
    cpp-12=12.2.0-14+deb12u1
    cpp=4:12.2.0-3
    curl=7.88.1-10+deb12u15
    dpkg-dev=1.21.23
    file=1:5.44-3
    g++-12=12.2.0-14+deb12u1
    g++=4:12.2.0-3
    gcc-12=12.2.0-14+deb12u1
    gcc=4:12.2.0-3
    git-man=1:2.39.5-0+deb12u3
    git=1:2.39.5-0+deb12u3
    libasan8=12.2.0-14+deb12u1
    libatomic1=12.2.0-14+deb12u1
    libbinutils=2.40-2
    libbrotli1=1.0.9-2+b6
    libc-dev-bin=2.36-9+deb12u14
    libc6-dev=2.36-9+deb12u14
    libcc1-0=12.2.0-14+deb12u1
    libcrypt-dev=1:4.4.33-2
    libctf-nobfd0=2.40-2
    libctf0=2.40-2
    libcurl3-gnutls=7.88.1-10+deb12u15
    libcurl4=7.88.1-10+deb12u15
    libdpkg-perl=1.21.23
    liberror-perl=0.17029-2
    libexpat1=2.5.0-1+deb12u4
    libgcc-12-dev=12.2.0-14+deb12u1
    libgdbm-compat4=1.23-3
    libgdbm6=1.23-3
    libgomp1=12.2.0-14+deb12u1
    libgprofng0=2.40-2
    libgssapi-krb5-2=1.20.1-2+deb12u5
    libhwasan0=12.2.0-14+deb12u1
    libisl23=0.25-1.1
    libitm1=12.2.0-14+deb12u1
    libjansson4=2.14-2
    libk5crypto3=1.20.1-2+deb12u5
    libkeyutils1=1.6.3-2
    libkrb5-3=1.20.1-2+deb12u5
    libkrb5support0=1.20.1-2+deb12u5
    libldap-2.5-0=2.5.13+dfsg-5
    liblsan0=12.2.0-14+deb12u1
    liblzma5=5.4.1-1+deb12u2
    libmagic-mgc=1:5.44-3
    libmagic1=1:5.44-3
    libmpc3=1.3.1-1
    libmpfr6=4.2.0-1
    libncursesw6=6.4-4
    libnghttp2-14=1.52.0-1+deb12u3
    libnsl-dev=1.3.0-2
    libnsl2=1.3.0-2
    libperl5.36=5.36.0-7+deb12u4
    libpsl5=0.21.2-1
    libpython3-stdlib=3.11.2-1+b1
    libpython3.11-minimal=3.11.2-6+deb12u9
    libpython3.11-stdlib=3.11.2-6+deb12u9
    libreadline8=8.2-1.3
    librtmp1=2.4+20151223.gitfa8646d.1-2+b2
    libsasl2-2=2.1.28+dfsg-10
    libsasl2-modules-db=2.1.28+dfsg-10
    libsqlite3-0=3.40.1-2+deb12u2
    libssh2-1=1.10.0-3+deb12u1
    libssl-dev=3.0.22-1~deb12u1
    libssl3=3.0.22-1~deb12u1
    libstdc++-12-dev=12.2.0-14+deb12u1
    libtirpc-common=1.3.3+ds-1
    libtirpc-dev=1.3.3+ds-1
    libtirpc3=1.3.3+ds-1
    libtsan2=12.2.0-14+deb12u1
    libubsan1=12.2.0-14+deb12u1
    linux-libc-dev=6.1.187-1
    m4=1.4.19-3
    make=4.3-4.1
    media-types=10.0.0
    openssl=3.0.22-1~deb12u1
    patch=2.7.6-7
    perl-base=5.36.0-7+deb12u4
    perl-modules-5.36=5.36.0-7+deb12u4
    perl=5.36.0-7+deb12u4
    python3-minimal=3.11.2-1+b1
    python3.11-minimal=3.11.2-6+deb12u9
    python3.11=3.11.2-6+deb12u9
    python3=3.11.2-1+b1
    readline-common=8.2-1.3
    rpcsvc-proto=1.4.3-1
    xz-utils=5.4.1-1+deb12u2
)
# sha256 of the sorted `dpkg-query -W -f '${Package}:${Architecture}=${Version}\n'` listing of the
# container after the install (every package, base image included).
DPKG_SET_SHA256="b0dc5c6cbbb91237d29e62f3a78459e6314bf38f6daf2b8a3a8360d1eb5c3d9e"

# ---- Xilinx embeddedsw at the commit xilinx_v2024.2 points to (annotated tag object a3cac98a...).
# Why 2024.2: the last release before AMD's 2025 restructuring of lib/sw_apps; it still carries the
# stand-alone zynqmp_fsbl and zynqmp_pmufw makefile flows (misc/copy_bsp.sh) this recipe uses, and
# it is the release the bootgen below belongs to. TREE_SHA256 is the manifest hash (see the
# build script) of the checked-out tree without .git.
EMBEDDEDSW_URL="https://github.com/Xilinx/embeddedsw.git"
EMBEDDEDSW_COMMIT="6e4d0b89d2958994ab9b3531eb4c6e648a63f201"
EMBEDDEDSW_TREE_SHA256="ce09fc9af75d7609f03658d58853074b2ab1e560edc45938b4354c3ee19bb720"

# ---- Xilinx bootgen (open source, ZynqMP and Zynq-7000 support) at the commit xilinx_v2024.2
# points to (annotated tag object c045ff3e...).
BOOTGEN_URL="https://github.com/Xilinx/bootgen.git"
BOOTGEN_COMMIT="6f448fece5d999985128fd454ae047e065a5e45d"
BOOTGEN_TREE_SHA256="743a2822746eb3a738d151d6500b3794e9323a339cf5f62f1cbd9925a500c60a"

# ---- Toolchain sources (built from source: no prebuilt binary is trusted). name|url|sha256.
TOOLCHAIN_TARBALLS=(
    "binutils-2.42.tar.xz|https://ftp.gnu.org/gnu/binutils/binutils-2.42.tar.xz|f6e4d41fd5fc778b06b7891457b3620da5ecea1006c6a4a41ae998109f85a800"
    "gcc-13.3.0.tar.xz|https://ftp.gnu.org/gnu/gcc/gcc-13.3.0/gcc-13.3.0.tar.xz|0845e9621c9543a13f484e94584a49ffc0129970e9914624235fc1d061a0c083"
    "newlib-4.4.0.20231231.tar.gz|https://sourceware.org/pub/newlib/newlib-4.4.0.20231231.tar.gz|0c166a39e1bf0951dfafcd68949fe0e4b6d3658081d6282f39aeefc6310f2f13"
    "gmp-6.3.0.tar.xz|https://ftp.gnu.org/gnu/gmp/gmp-6.3.0.tar.xz|a3c2b80201b89e68616f4ad30bc66aee4927c3ce50e33929ca819d5c43538898"
    "mpfr-4.2.1.tar.xz|https://ftp.gnu.org/gnu/mpfr/mpfr-4.2.1.tar.xz|277807353a6726978996945af13e52829e3abd7a9a5b7fb2793894e18f1fcbb2"
    "mpc-1.3.1.tar.gz|https://ftp.gnu.org/gnu/mpc/mpc-1.3.1.tar.gz|ab642492f5cf882b74aa0cb730cd410a81edcdbec895183ce930e706c1c759b8"
)
# Manifest hashes of the two built toolchain trees (measured after the first build; checked before
# every build; see "toolchain" in the build script header). Two independent builds of each gave the
# same hash, with one exception that is part of the pin: the MicroBlaze tree's target libraries
# (newlib's libc.a and libgloss's libnosys.a under <target>/lib/) are not bit-reproducible from run
# to run (README.md, "Found"), so they are left out of that tree's manifest; the PMU firmware that
# links them is pinned by its own hash instead (PMUFW_ELF_SHA256 below).
AARCH64_TOOLCHAIN_MANIFEST_SHA256="cc6a9e382dcd8266577afb8c758bc9a1b4abe11b230940c68e71a403de026cb8"
MICROBLAZE_TOOLCHAIN_MANIFEST_SHA256="9b8b3632ed46f60807e2e8256bc9baf8c2b609951e7c6b32e0b796af78f85600"
MICROBLAZE_TOOLCHAIN_MANIFEST_EXCLUDE="microblazeel-xilinx-elf/lib/"

# ---- The RPU application: the reproducible RTEMS 6.1 zynqmp_rpu_lock_step cFS ELF
# (third_party/rtems-container/build-elf.sh, docs/open-questions.md question 240).
RPU_ELF_SHA256="de96907ff95fc8854723fbc71cd0c084483332c08984b60ec22ef923e7dacaf3"

# ---- Build date for every __DATE__/__TIME__ (FSBL and PMU firmware banners): 2026-10-07 00:00:00 UTC.
PINNED_SOURCE_DATE_EPOCH="1791331200"

# ---- Expected outputs of the shipping build (SOURCE_DATE_EPOCH above, the RPU ELF above). Measured
# 2026-10-07/08 over five builds from different host staging and output paths, on the container's
# filesystem and on a tmpfs, with two independently built sets of toolchains. phase3-build.sh checks
# them whenever the build date is the pinned one, so a drift in any input shows up as a mismatch here.
# The PMU firmware does not depend on the board. The FSBL and the image do: the two "STANDIN" values
# are for the ZCU102 psu_init files embeddedsw ships (README.md, "Status"); a build with a supplied
# psu_init is not checked against them.
PMUFW_ELF_SHA256="81701d2bcc3ffd507e276e6e3c2b705cc094504fac8f28555b9aa02958cca8a0"
STANDIN_FSBL_ELF_SHA256="61f8b9451b62c8ff4b148ac13550247e475f63250e2771e28adb0b7ce1df3d45"
STANDIN_BOOT_BIN_SHA256="72fd2d6f64d49ac53a64edba34ede15e7863f3e8cbebb4766a830a649797d489"
