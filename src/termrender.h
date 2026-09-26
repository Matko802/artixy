#ifndef ARTIXY_TERMRENDER_H
#define ARTIXY_TERMRENDER_H

#include <stddef.h>

/* 120x40 terminal, matching the guest runner's stty settings. */
#define TERM_COLS 120
#define TERM_ROWS 40

/*
 * Render terminal output bytes to a PNG image.
 * Returns malloc'd PNG data (*png_len set) or NULL when no usable font
 * was found (caller falls back to text). Thread-safe.
 */
unsigned char *termrender_png(const unsigned char *output, size_t len,
                              size_t *png_len);

/* Strip kitty graphics sequences (ESC _ ... ST). Exposed for tests. */
unsigned char *termrender_strip_kitty(const unsigned char *in, size_t len,
                                      size_t *out_len);

#endif
