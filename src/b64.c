#include "b64.h"
#include "util.h"

#include <stdlib.h>
#include <string.h>

static const char enc_table[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

char *b64_encode(const void *data, size_t len) {
    const unsigned char *in = data;
    size_t out_len = ((len + 2) / 3) * 4;
    char *out = xmalloc(out_len + 1);
    if (!out)
        return NULL;
    size_t i = 0, o = 0;
    while (i < len) {
        unsigned int a = in[i++];
        unsigned int b = i < len ? in[i++] : 0;
        unsigned int c = i < len ? in[i++] : 0;
        unsigned int triple = (a << 16) | (b << 8) | c;
        out[o++] = enc_table[(triple >> 18) & 0x3f];
        out[o++] = enc_table[(triple >> 12) & 0x3f];
        out[o++] = enc_table[(triple >> 6) & 0x3f];
        out[o++] = enc_table[triple & 0x3f];
    }
    /* padding */
    size_t mod = len % 3;
    if (mod == 1) {
        out[out_len - 1] = '=';
        out[out_len - 2] = '=';
    } else if (mod == 2) {
        out[out_len - 1] = '=';
    }
    out[out_len] = '\0';
    return out;
}

static int dec_val(char c) {
    if (c >= 'A' && c <= 'Z')
        return c - 'A';
    if (c >= 'a' && c <= 'z')
        return c - 'a' + 26;
    if (c >= '0' && c <= '9')
        return c - '0' + 52;
    if (c == '+')
        return 62;
    if (c == '/')
        return 63;
    return -1;
}

unsigned char *b64_decode(const char *s, size_t *len_out) {
    size_t n = strlen(s);
    if (n == 0) {
        unsigned char *out = xmalloc(1);
        if (out && len_out)
            *len_out = 0;
        return out;
    }
    if (n % 4 != 0)
        return NULL;
    size_t pad = 0;
    if (s[n - 1] == '=')
        pad++;
    if (n > 1 && s[n - 2] == '=')
        pad++;
    if (pad > 2)
        return NULL;
    /* padding only at the very end; no interior '=' */
    for (size_t i = 0; i < n - pad; i++) {
        if (s[i] == '=' || dec_val(s[i]) < 0)
            return NULL;
    }
    size_t out_len = n / 4 * 3 - pad;
    unsigned char *out = xmalloc(out_len ? out_len : 1);
    if (!out)
        return NULL;
    size_t o = 0;
    for (size_t i = 0; i < n; i += 4) {
        int a = dec_val(s[i]);
        int b = dec_val(s[i + 1]);
        int c = (i + 2 < n - pad) ? dec_val(s[i + 2]) : 0;
        int d = (i + 3 < n - pad) ? dec_val(s[i + 3]) : 0;
        if (a < 0 || b < 0 || c < 0 || d < 0) {
            free(out);
            return NULL;
        }
        /* last quantum must be zero in its pad bits */
        if (i + 4 == n) {
            if (pad == 1 && (d & 3) != 0) {
                /* one pad char: 2 bytes encoded, low 2 bits of d unused */
                free(out);
                return NULL;
            }
            if (pad == 2 && (c & 15) != 0) {
                /* two pad chars: 1 byte encoded, low 4 bits of c unused */
                free(out);
                return NULL;
            }
        }
        unsigned int triple = ((unsigned)a << 18) | ((unsigned)b << 12) |
                              ((unsigned)c << 6) | (unsigned)d;
        if (o < out_len)
            out[o++] = (unsigned char)((triple >> 16) & 0xff);
        if (o < out_len)
            out[o++] = (unsigned char)((triple >> 8) & 0xff);
        if (o < out_len)
            out[o++] = (unsigned char)(triple & 0xff);
    }
    if (len_out)
        *len_out = out_len;
    return out;
}
