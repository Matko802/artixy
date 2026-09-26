/* Mutational fuzz for scrub_public_ip: no-crash + redaction sanity.
 * Usage: fuzz_scrub [iterations] [seed] */
#include "../src/scrub.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *seeds[] = {
    "my ip is 8.8.8.8 ok",
    "local 192.168.1.5 and 10.0.0.1 stay",
    "v6 2001:db8::1 and ::1 and fe80::1 here",
    "999.999.999.999 is not an ip",
    "1.2.3.4.5 dotted trail",
    "text 1234:5678:9abc:def0:1234:5678:9abc:def0 end",
    "edge :::: and 1::2::3 and ::ffff:1.2.3.4",
    "caf\xc3\xa9 \xf0\x9f\x98\x80 8.8.8.8",
    "",
};

static const char interesting[] = "0123456789abcdefABCDEF:. \t\nxyz";

static unsigned lcg(unsigned *s) {
    *s = *s * 1103515245u + 12345u;
    return (*s >> 16) & 0x7fffu;
}

int main(int argc, char **argv) {
    long iters = argc > 1 ? atol(argv[1]) : 20000;
    unsigned rng = argc > 2 ? (unsigned)atol(argv[2]) : 0x5C12B;
    char buf[1024];
    for (long it = 0; it < iters; it++) {
        const char *seed = seeds[it % (sizeof(seeds) / sizeof(seeds[0]))];
        size_t sl = strlen(seed);
        if (sl >= sizeof buf)
            sl = sizeof(buf) - 1;
        memcpy(buf, seed, sl);
        buf[sl] = '\0';
        int nm = 1 + (int)(lcg(&rng) % 4);
        for (int k = 0; k < nm && sl > 0; k++) {
            unsigned op = lcg(&rng) % 3;
            size_t pos = lcg(&rng) % (sl + 1);
            if (op == 0 && sl + 1 < sizeof buf) {
                memmove(buf + pos + 1, buf + pos, sl - pos + 1);
                buf[pos] = interesting[lcg(&rng) % (sizeof(interesting) - 1)];
                sl++;
            } else if (op == 1 && sl > 0) {
                if (pos >= sl)
                    pos = sl - 1;
                buf[pos] = interesting[lcg(&rng) % (sizeof(interesting) - 1)];
            } else if (sl > 0) {
                if (pos >= sl)
                    pos = sl - 1;
                memmove(buf + pos, buf + pos + 1, sl - pos);
                sl--;
            }
        }
        char *out = scrub_public_ip(buf);
        if (!out) {
            fprintf(stderr, "fuzz: NULL return at iter %ld\n", it);
            return 1;
        }
        /* output must be NUL-terminated within a sane bound */
        size_t ol = strlen(out);
        if (ol > sl + 64) {
            fprintf(stderr, "fuzz: output exploded at iter %ld\n", it);
            free(out);
            return 1;
        }
        /* a plain public IP alone must always redact */
        free(out);
    }
    /* fixed sanity cases */
    {
        char *o = scrub_public_ip("8.8.8.8");
        if (!o || strcmp(o, "[redacted]") != 0) {
            fprintf(stderr, "fuzz: lone public IP not redacted\n");
            free(o);
            return 1;
        }
        free(o);
        o = scrub_public_ip("10.1.2.3");
        if (!o || strcmp(o, "10.1.2.3") != 0) {
            fprintf(stderr, "fuzz: private IP redacted\n");
            free(o);
            return 1;
        }
        free(o);
    }
    printf("fuzz_scrub ok (%ld iters)\n", iters);
    return 0;
}
