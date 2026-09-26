/* Mutational fuzz for the JSONC stripper: no-crash + idempotence.
 * Usage: fuzz_jsonc [iterations] [seed] */
#include "../src/config.h"

#include <jansson.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *seeds[] = {
    "{\"a\": 1,}",
    "// hello\n{\"a\": [1, /* x */ 2,], \"b\": \"x,}y // z\",}",
    "{\"s\": \"q\\\"}//,x\", \"t\": 1,}",
    "{\"emoji\": \"caf\xc3\xa9 \xf0\x9f\x98\x80\",}",
    "{\"a\":{\"b\":{\"c\":[1,2,{\"d\":null,},],},},}",
    "[1,2,3,]",
    "{\"unclosed\": \"abc",
    "/* unterminated",
    "{\"a\":1} trailing junk {{{",
    "{}",
};

static const char interesting[] =
    "\"\\/{}[],: \t\n\rabcdef019_";

static unsigned lcg(unsigned *s) {
    *s = *s * 1103515245u + 12345u;
    return (*s >> 16) & 0x7fffu;
}

int main(int argc, char **argv) {
    long iters = argc > 1 ? atol(argv[1]) : 20000;
    unsigned rng = argc > 2 ? (unsigned)atol(argv[2]) : 0xC10C;
    char buf[2048];
    for (long it = 0; it < iters; it++) {
        const char *seed = seeds[it % (sizeof(seeds) / sizeof(seeds[0]))];
        size_t sl = strlen(seed);
        if (sl >= sizeof buf)
            sl = sizeof(buf) - 1;
        memcpy(buf, seed, sl);
        buf[sl] = '\0';
        /* 1-4 random mutations */
        int nm = 1 + (int)(lcg(&rng) % 4);
        for (int k = 0; k < nm && sl > 0; k++) {
            unsigned op = lcg(&rng) % 3;
            size_t pos = lcg(&rng) % (sl + 1);
            if (op == 0 && sl + 1 < sizeof buf) {
                /* insert */
                memmove(buf + pos + 1, buf + pos, sl - pos + 1);
                buf[pos] = interesting[lcg(&rng) % (sizeof(interesting) - 1)];
                sl++;
            } else if (op == 1 && sl > 0) {
                /* replace */
                if (pos >= sl)
                    pos = sl - 1;
                buf[pos] = interesting[lcg(&rng) % (sizeof(interesting) - 1)];
            } else if (sl > 0) {
                /* delete */
                if (pos >= sl)
                    pos = sl - 1;
                memmove(buf + pos, buf + pos + 1, sl - pos);
                sl--;
            }
        }
        char *s1 = config_strip_jsonc(buf, sl);
        if (!s1) {
            fprintf(stderr, "fuzz: NULL return (OOM) at iter %ld, continuing\n",
                    it);
            continue;
        }
        /* stripped output must be NUL-terminated and no longer than input */
        if (strlen(s1) > sl) {
            fprintf(stderr, "fuzz: output longer than input at iter %ld: [%s]\n",
                    it, buf);
            free(s1);
            return 1;
        }
        /* idempotence: stripping twice is a fixed point */
        char *s2 = config_strip_jsonc(s1, strlen(s1));
        if (!s2 || strcmp(s1, s2) != 0) {
            fprintf(stderr, "fuzz: not idempotent at iter %ld\ninput:  [%s]\n"
                            "pass1:  [%s]\npass2:  [%s]\n",
                    it, buf, s1, s2 ? s2 : "(null)");
            free(s1);
            free(s2);
            return 1;
        }
        free(s2);
        /* must be parseable-or-not without crashing */
        json_error_t e;
        json_t *r = json_loads(s1, 0, &e);
        json_decref(r);
        free(s1);
    }
    printf("fuzz_jsonc ok (%ld iters)\n", iters);
    return 0;
}
