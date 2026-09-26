/* Unit tests for the terminal PNG renderer. */
#include "test.h"

#include "../src/termrender.h"

#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* PNG signature + IHDR parsing (big-endian). */
static bool is_png(const unsigned char *p, size_t n) {
    static const unsigned char sig[8] = { 0x89, 'P', 'N', 'G',
                                          0x0d, 0x0a, 0x1a, 0x0a };
    return n > 33 && memcmp(p, sig, 8) == 0 &&
           memcmp(p + 12, "IHDR", 4) == 0;
}

static void ihdr_size(const unsigned char *p, uint32_t *w, uint32_t *h) {
    *w = ((uint32_t)p[16] << 24) | ((uint32_t)p[17] << 16) |
         ((uint32_t)p[18] << 8) | p[19];
    *h = ((uint32_t)p[20] << 24) | ((uint32_t)p[21] << 16) |
         ((uint32_t)p[22] << 8) | p[23];
}

TEST(strip_kitty_sequences) {
    /* ESC _ G ... BEL wrapped payload must vanish */
    const unsigned char in[] = "ab\x1b_Gi=1;OK\x07"
                               "cd\x1b_Gf=100;AAAA\x1b\\ef";
    size_t n = 0;
    unsigned char *out = termrender_strip_kitty(in, sizeof(in) - 1, &n);
    CHECK(out != NULL);
    CHECK(n == 6 && memcmp(out, "abcdef", 6) == 0);
    free(out);
    /* plain text untouched */
    const char *plain = "hello \x1b[31mred\x1b[0m";
    out = termrender_strip_kitty((const unsigned char *)plain, strlen(plain),
                                 &n);
    CHECK(out != NULL && n == strlen(plain));
    free(out);
}

TEST(render_produces_valid_png) {
    /* NOTE: real guest output comes through `script`/pty, so newlines are
     * \r\n. Bare \n is treated as linefeed-only (correct terminal
     * behavior) and would stair-step; the renderer must get raw bytes. */
    const char *sample = "$ echo hi\r\n"
                         "hi\r\n"
                         "\x1b[1;32mgreen bold\x1b[0m normal\r\n";
    size_t pn = 0;
    unsigned char *png =
        termrender_png((const unsigned char *)sample, strlen(sample), &pn);
    if (!png) {
        printf("(skip: no font available)\n");
        return;
    }
    CHECK(is_png(png, pn));
    uint32_t w = 0, h = 0;
    ihdr_size(png, &w, &h);
    CHECK(w > 700 && w < 2200 && h > 200 && h < 1400);
    /* deterministic: same input -> identical bytes */
    size_t pn2 = 0;
    unsigned char *png2 =
        termrender_png((const unsigned char *)sample, strlen(sample), &pn2);
    CHECK(png2 != NULL && pn2 == pn && memcmp(png, png2, pn) == 0);
    free(png2);
    free(png);
}

TEST(render_empty_and_plain) {
    size_t pn = 0;
    unsigned char *png = termrender_png((const unsigned char *)"", 0, &pn);
    if (!png) {
        printf("(skip: no font available)\n");
        return;
    }
    CHECK(is_png(png, pn));
    free(png);
    /* cursor movement + colors exercise the grid */
    const char *sample = "\x1b[2J\x1b[Hline1\nline2\n\x1b[31mred\x1b[0m";
    png = termrender_png((const unsigned char *)sample, strlen(sample), &pn);
    CHECK(png != NULL && is_png(png, pn));
    free(png);
}

int main(void) {
    RUN(strip_kitty_sequences);
    RUN(render_produces_valid_png);
    RUN(render_empty_and_plain);
    TEST_REPORT();
}
