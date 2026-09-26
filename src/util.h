#ifndef ARTIXY_UTIL_H
#define ARTIXY_UTIL_H

#include <stddef.h>

/* Allocation helpers. Return NULL on failure (never abort). */
void *xmalloc(size_t n);
void *xrealloc(void *p, size_t n);
char *xstrdup(const char *s);

/* Read whole file into malloc'd buffer (NUL-terminated, len set without NUL).
 * Returns 0 on success, -1 on error. */
int read_file(const char *path, char **out, size_t *len_out);

/* mkdir -p equivalent. Returns 0 on success, -1 on error. */
int mkdir_p(const char *dir);

#endif
