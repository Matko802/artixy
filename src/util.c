#include "util.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <errno.h>

void *xmalloc(size_t n) {
    if (n == 0)
        n = 1;
    return malloc(n);
}

void *xrealloc(void *p, size_t n) {
    if (n == 0)
        n = 1;
    return realloc(p, n);
}

char *xstrdup(const char *s) {
    if (!s)
        return NULL;
    size_t n = strlen(s) + 1;
    char *p = xmalloc(n);
    if (p)
        memcpy(p, s, n);
    return p;
}

int read_file(const char *path, char **out, size_t *len_out) {
    FILE *f = fopen(path, "rb");
    if (!f)
        return -1;
    size_t cap = 8192, len = 0;
    char *buf = xmalloc(cap);
    if (!buf) {
        fclose(f);
        return -1;
    }
    size_t r;
    while ((r = fread(buf + len, 1, cap - len, f)) > 0) {
        len += r;
        if (len == cap) {
            cap *= 2;
            char *nb = xrealloc(buf, cap);
            if (!nb) {
                free(buf);
                fclose(f);
                return -1;
            }
            buf = nb;
        }
    }
    if (ferror(f)) {
        free(buf);
        fclose(f);
        return -1;
    }
    fclose(f);
    buf[len] = '\0'; /* buffer always has room: len < cap */
    *out = buf;
    if (len_out)
        *len_out = len;
    return 0;
}

int mkdir_p(const char *dir) {
    if (!dir || !*dir)
        return -1;
    char tmp[4096];
    snprintf(tmp, sizeof tmp, "%s", dir);
    size_t n = strlen(tmp);
    if (n == 0)
        return -1;
    if (tmp[n - 1] == '/')
        tmp[n - 1] = '\0';
    for (char *p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
            if (mkdir(tmp, 0700) != 0 && errno != EEXIST)
                return -1;
            *p = '/';
        }
    }
    if (mkdir(tmp, 0700) != 0 && errno != EEXIST)
        return -1;
    return 0;
}
