/*
 * Cross-compilation shim: define missing C23 libc symbols as
 * wrappers around their older equivalents.
 *
 * The aarch64 OpenSSL sysroot is built on a host with glibc ≥ 2.38,
 * which emits the new C23 strtol variant `__isoc23_strtol`. The brew
 * aarch64 cross-toolchain ships an ancient glibc 2.17 that has no
 * such symbol. The semantic differences (C23's `0b` prefix support)
 * don't matter for OpenSSL's usage, so a thin wrapper is safe.
 *
 * Built per-target via cc-rs in build.rs at the workspace root, then
 * linked into the final binary. Drop once libcrypto.a is rebuilt on
 * a matching-glibc host.
 */
#include <stdlib.h>

long __isoc23_strtol(const char *nptr, char **endptr, int base) {
    return strtol(nptr, endptr, base);
}

unsigned long __isoc23_strtoul(const char *nptr, char **endptr, int base) {
    return strtoul(nptr, endptr, base);
}

long long __isoc23_strtoll(const char *nptr, char **endptr, int base) {
    return strtoll(nptr, endptr, base);
}

unsigned long long __isoc23_strtoull(const char *nptr, char **endptr, int base) {
    return strtoull(nptr, endptr, base);
}
