#ifndef ARTIXY_B64_H
#define ARTIXY_B64_H

#include <stddef.h>

/* Standard base64. All outputs are NUL-terminated malloc'd buffers. */
/* Encodes len bytes. Returns NULL on allocation failure. */
char *b64_encode(const void *data, size_t len);
/*
 * Decodes a NUL-terminated standard base64 string (whitespace rejected;
 * padding required as produced by b64_encode). Returns NULL on invalid
 * input or allocation failure; *len_out holds the decoded length.
 */
unsigned char *b64_decode(const char *s, size_t *len_out);

#endif
