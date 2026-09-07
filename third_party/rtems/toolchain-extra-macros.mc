# Extra RSB macros for the arm-rtems6 toolchain build (loaded via `--macros`, RSB's own
# supported mechanism for this -- record format is "key: type, attribute, value", the same
# format RSB's own source-builder/defaults.mc uses). See build-toolchain.sh and
# third_party/rtems/REPORT.md ("Patch carried" section) for why this is needed: GCC's own
# bundled zlib fails to build against this host's Xcode/macOS SDK headers, the same
# `fdopen`-macro collision as binutils/gdb; `gcc-common-1.cfg` already exposes a
# `%{?gcc_configure_extra_options:...}` hook in its configure invocation for exactly this kind
# of override, so no vendored RSB config file is patched for this one -- only the two binutils/
# gdb configs (which have no such hook) are patched, under third_party/rtems/patches/.
gcc_configure_extra_options: none, none, '--with-system-zlib'
