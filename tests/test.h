/* Minimal single-header test framework (no dependencies). */
#ifndef ARTIXY_TEST_H
#define ARTIXY_TEST_H

#include <stdio.h>

static int test_failures = 0;
static int test_count = 0;
static const char *test_current = "";

#define TEST(name)                                   \
    static void test_##name(void);                   \
    static void run_##name(void) {                   \
        test_current = #name;                        \
        test_count++;                                \
        test_##name();                               \
    }                                                \
    static void test_##name(void)

#define RUN(name) run_##name()

#define CHECK(cond)                                                        \
    do {                                                                   \
        if (!(cond)) {                                                     \
            printf("FAIL %s:%d: %s: %s\n", __FILE__, __LINE__,              \
                   test_current, #cond);                                   \
            test_failures++;                                               \
            return;                                                        \
        }                                                                  \
    } while (0)

#define CHECK_STR_EQ(a, b)                                                 \
    do {                                                                   \
        const char *_a = (a), *_b = (b);                                   \
        if (!_a || !_b || strcmp(_a, _b) != 0) {                           \
            printf("FAIL %s:%d: %s: '%s' != '%s'\n", __FILE__, __LINE__,    \
                   test_current, _a ? _a : "(null)",                       \
                   _b ? _b : "(null)");                                    \
            test_failures++;                                               \
            return;                                                        \
        }                                                                  \
    } while (0)

#define TEST_REPORT()                                                      \
    do {                                                                   \
        if (test_failures == 0)                                            \
            printf("ok (%d tests)\n", test_count);                         \
        else                                                               \
            printf("FAILED (%d failures / %d tests)\n", test_failures,     \
                   test_count);                                            \
        return test_failures != 0;                                         \
    } while (0)

#endif
