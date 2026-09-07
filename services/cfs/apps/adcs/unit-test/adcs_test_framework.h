/*
 * adcs_test_framework.h -- minimal, dependency-free C test harness for services/cfs/apps/adcs's
 * host-side unit tests.
 *
 * This app's worker brief: "Unit-tested on the host with cFS's own unit-test framework where
 * practical -- if that framework is not available because M23.2 has not landed the tree yet,
 * use a plain C test harness and say so." At the time this was written, third_party/cfs (and
 * with it cFS's own UT-assert framework) had not landed -- services/cfs held only a README, no
 * apps of any kind -- so this is that plain C harness. It has no dependency beyond the C
 * standard library, deliberately, so it never blocks on anything M23.2/M23.4 land later.
 */
#ifndef ADCS_TEST_FRAMEWORK_H
#define ADCS_TEST_FRAMEWORK_H

#include <stdio.h>
#include <string.h>

static int g_adcs_test_failures = 0;
static int g_adcs_test_count = 0;
static const char *g_adcs_current_test = "";

#define ADCS_TEST(name) static void name(void)
#define ADCS_RUN_TEST(name)                                  \
    do {                                                     \
        g_adcs_current_test = #name;                         \
        g_adcs_test_count++;                                 \
        int before = g_adcs_test_failures;                   \
        name();                                               \
        if (g_adcs_test_failures == before) {                 \
            printf("  [PASS] %s\n", #name);                   \
        }                                                      \
    } while (0)

#define ADCS_CHECK(cond)                                                                          \
    do {                                                                                          \
        if (!(cond)) {                                                                            \
            printf("  [FAIL] %s: %s:%d: assertion failed: %s\n", g_adcs_current_test, __FILE__, __LINE__, #cond); \
            g_adcs_test_failures++;                                                                \
        }                                                                                          \
    } while (0)

#define ADCS_CHECK_EQ_INT(a, b)                                                                                          \
    do {                                                                                                                \
        long long va_ = (long long)(a);                                                                                \
        long long vb_ = (long long)(b);                                                                                \
        if (va_ != vb_) {                                                                                              \
            printf("  [FAIL] %s: %s:%d: %s (%lld) != %s (%lld)\n", g_adcs_current_test, __FILE__, __LINE__, #a, va_, #b, vb_); \
            g_adcs_test_failures++;                                                                                     \
        }                                                                                                               \
    } while (0)

#define ADCS_CHECK_NEAR(a, b, tol)                                                                                                    \
    do {                                                                                                                             \
        double va_ = (double)(a);                                                                                                    \
        double vb_ = (double)(b);                                                                                                    \
        double diff_ = va_ - vb_;                                                                                                    \
        if (diff_ < 0) diff_ = -diff_;                                                                                               \
        if (diff_ > (tol)) {                                                                                                         \
            printf("  [FAIL] %s: %s:%d: %s (%.17e) != %s (%.17e), |diff|=%.3e > tol %.3e\n", g_adcs_current_test, __FILE__, __LINE__, #a, va_, #b, vb_, diff_, (double)(tol)); \
            g_adcs_test_failures++;                                                                                                   \
        }                                                                                                                            \
    } while (0)

#define ADCS_CHECK_BYTES_EQ(actual, expected, len)                                                                    \
    do {                                                                                                             \
        if (memcmp((actual), (expected), (len)) != 0) {                                                             \
            printf("  [FAIL] %s: %s:%d: byte buffers differ\n    actual:   ", g_adcs_current_test, __FILE__, __LINE__); \
            for (size_t i_ = 0; i_ < (size_t)(len); i_++) printf("%02X ", ((const unsigned char *)(actual))[i_]);    \
            printf("\n    expected: ");                                                                              \
            for (size_t i_ = 0; i_ < (size_t)(len); i_++) printf("%02X ", ((const unsigned char *)(expected))[i_]);  \
            printf("\n");                                                                                            \
            g_adcs_test_failures++;                                                                                  \
        }                                                                                                            \
    } while (0)

#define ADCS_TEST_SUMMARY_AND_EXIT()                                                                          \
    do {                                                                                                     \
        printf("%d/%d tests run, %d failure(s)\n", g_adcs_test_count, g_adcs_test_count, g_adcs_test_failures); \
        return g_adcs_test_failures == 0 ? 0 : 1;                                                             \
    } while (0)

#endif /* ADCS_TEST_FRAMEWORK_H */
