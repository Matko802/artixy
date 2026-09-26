/* Public IP redaction (port of scrub.rs). */
#include "scrub.h"
#include "util.h"

#include <ctype.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

static bool is_digit(unsigned char c) {
    return c >= '0' && c <= '9';
}

static bool is_hex(unsigned char c) {
    return is_digit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}

static bool is_alnum(unsigned char c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || is_digit(c);
}

static bool public_ipv4(const unsigned char o[4]) {
    if (o[0] == 10)
        return false;
    if (o[0] == 172 && o[1] >= 16 && o[1] <= 31)
        return false;
    if (o[0] == 192 && o[1] == 168)
        return false;
    if (o[0] == 127)
        return false;
    if (o[0] == 169 && o[1] == 254)
        return false;
    if (o[0] == 100 && o[1] >= 64 && o[1] <= 127)
        return false;
    if (o[0] == 0)
        return false;
    if (o[0] == 255 && o[1] == 255 && o[2] == 255 && o[3] == 255)
        return false;
    return true;
}

/* scan dotted quad at b[i]; on success fills o/end, returns true */
static bool scan_ipv4(const unsigned char *b, size_t n, size_t i,
                      unsigned char o[4], size_t *end) {
    if (i > 0 && (is_alnum(b[i - 1]) || b[i - 1] == '.'))
        return false;
    size_t j = i;
    for (int k = 0; k < 4; k++) {
        size_t start = j;
        while (j < n && is_digit(b[j]))
            j++;
        size_t len = j - start;
        if (len == 0 || len > 3)
            return false;
        unsigned v = 0;
        for (size_t d = start; d < j; d++)
            v = v * 10 + (unsigned)(b[d] - '0');
        if (v > 255)
            return false;
        o[k] = (unsigned char)v;
        if (k < 3) {
            if (j >= n || b[j] != '.')
                return false;
            j++;
        }
    }
    if (j < n) {
        if (is_digit(b[j]))
            return false;
        if (b[j] == '.' && j + 1 < n && is_digit(b[j + 1]))
            return false;
    }
    *end = j;
    return true;
}

static size_t utf8_len(unsigned char c) {
    if (c <= 0x7f)
        return 1;
    if (c >= 0xc0 && c <= 0xdf)
        return 2;
    if (c >= 0xe0 && c <= 0xef)
        return 3;
    return 4;
}

static bool parse_quad(const char *s, size_t len, unsigned char o[4]) {
    int parts = 0;
    size_t i = 0;
    while (i < len && parts < 4) {
        size_t start = i;
        while (i < len && is_digit((unsigned char)s[i]))
            i++;
        size_t gl = i - start;
        if (gl == 0 || gl > 3)
            return false;
        unsigned v = 0;
        for (size_t d = start; d < i; d++)
            v = v * 10 + (unsigned)(s[d] - '0');
        if (v > 255)
            return false;
        o[parts++] = (unsigned char)v;
        if (parts < 4) {
            if (i >= len || s[i] != '.')
                return false;
            i++;
        }
    }
    return parts == 4 && i == len;
}

static bool valid_groups(char **parts, size_t n) {
    for (size_t i = 0; i < n; i++) {
        size_t l = strlen(parts[i]);
        if (!l || l > 4)
            return false;
        for (size_t k = 0; k < l; k++) {
            if (!is_hex((unsigned char)parts[i][k]))
                return false;
        }
    }
    return true;
}

/* classify tok (NUL-terminated scratch): true=valid IPv6, public set. */
static bool classify_ipv6(char *tok, bool *public_out) {
    char *head = tok;
    unsigned char tailq[4];
    bool has_tail = false;
    char *dot = strchr(tok, '.');
    if (dot) {
        char *cut = strrchr(tok, ':');
        if (!cut)
            return false;
        *cut = '\0';
        if (!parse_quad(cut + 1, strlen(cut + 1), tailq))
            return false;
        has_tail = true;
        head = tok;
    }
    /* count "::" */
    int dbl = 0;
    for (char *p = head; (p = strstr(p, "::")) != NULL; p += 2)
        dbl++;
    if (dbl > 1)
        return false;
    bool has_dbl = dbl == 1;
    int need = has_tail ? 2 : 0;
    /* split into explicit groups (max 8) */
    char *groups[8];
    size_t ng = 0;
    if (has_dbl) {
        char *sep = strstr(head, "::");
        *sep = '\0';
        char *left = head, *right = sep + 2;
        char *lparts[8], *rparts[8];
        size_t nl = 0, nr = 0;
        if (*left) {
            for (char *t = strtok(left, ":"); t && nl < 8;
                 t = strtok(NULL, ":")) {
                if (!*t)
                    return false;
                lparts[nl++] = t;
            }
        }
        if (*right) {
            for (char *t = strtok(right, ":"); t && nr < 8;
                 t = strtok(NULL, ":")) {
                if (!*t)
                    return false;
                rparts[nr++] = t;
            }
        }
        if (!valid_groups(lparts, nl) || !valid_groups(rparts, nr))
            return false;
        if (nl + nr + (size_t)need > 7)
            return false;
        for (size_t i = 0; i < nl; i++)
            groups[ng++] = lparts[i];
        for (size_t i = 0; i < nr; i++)
            groups[ng++] = rparts[i];
    } else {
        for (char *t = strtok(head, ":"); t && ng < 8; t = strtok(NULL, ":")) {
            if (!*t)
                return false;
            groups[ng++] = t;
        }
        if (!valid_groups(groups, ng) || (int)ng + need != 8)
            return false;
    }
    unsigned vals[8];
    for (size_t i = 0; i < ng; i++) {
        unsigned v = 0;
        for (char *p = groups[i]; *p; p++)
            v = v * 16 + (unsigned)(is_digit((unsigned char)*p)
                                        ? *p - '0'
                                        : tolower(*p) - 'a' + 10);
        vals[i] = v;
    }
    bool allzero = true;
    for (size_t i = 0; i < ng; i++) {
        if (vals[i]) {
            allzero = false;
            break;
        }
    }
    if (allzero) {
        *public_out = has_tail ? public_ipv4(tailq) : false;
        return true;
    }
    /* loopback ::1 */
    {
        size_t nz = 0;
        while (nz < ng && vals[nz] == 0)
            nz++;
        if (ng - nz == 1 && vals[ng - 1] == 1) {
            *public_out = false;
            return true;
        }
    }
    if (ng > 0) {
        unsigned g0 = vals[0];
        if ((g0 >= 0xfe80 && g0 <= 0xfebf) ||
            (g0 >= 0xfc00 && g0 <= 0xfdff)) {
            *public_out = false;
            return true;
        }
    }
    *public_out = true;
    return true;
}

/* scan candidate IPv6 token starting at s[i] (bytes). */
static bool scan_ipv6(const char *s, size_t n, size_t i, size_t *end,
                      bool *public_out) {
    unsigned char c = (unsigned char)s[i];
    if (!is_hex(c) && c != ':')
        return false;
    if (i > 0) {
        unsigned char p = (unsigned char)s[i - 1];
        if (is_hex(p) || p == ':' || p == '.')
            return false;
    }
    size_t j = i, colons = 0;
    while (j < n) {
        unsigned char b = (unsigned char)s[j];
        if (!(is_hex(b) || b == ':' || b == '.'))
            break;
        if (b == ':')
            colons++;
        j++;
    }
    if (colons < 2)
        return false;
    size_t tlen = j - i;
    while (tlen > 0 && s[i + tlen - 1] == ':' &&
           !(tlen >= 2 && s[i + tlen - 2] == ':')) {
        tlen--;
        j--;
    }
    char *tok = xmalloc(tlen + 1);
    if (!tok)
        return false;
    memcpy(tok, s + i, tlen);
    tok[tlen] = '\0';
    bool ok = classify_ipv6(tok, public_out);
    free(tok);
    if (!ok)
        return false;
    *end = j;
    return true;
}

char *scrub_public_ip(const char *s) {
    if (!s)
        return xstrdup("");
    size_t n = strlen(s);
    size_t cap = n + 1;
    char *out = xmalloc(cap);
    if (!out)
        return NULL;
    size_t w = 0, i = 0;
    const char *redact = "[redacted]";
    size_t rlen = strlen(redact);
    while (i < n) {
        unsigned char c = (unsigned char)s[i];
        if (is_hex(c) || c == ':') {
            size_t end = 0;
            bool pub = false;
            if (scan_ipv6(s, n, i, &end, &pub)) {
                size_t piece = pub ? rlen : end - i;
                while (w + piece + 1 > cap) {
                    cap *= 2;
                    char *no = xrealloc(out, cap);
                    if (!no) {
                        free(out);
                        return NULL;
                    }
                    out = no;
                }
                if (pub)
                    memcpy(out + w, redact, rlen);
                else
                    memcpy(out + w, s + i, end - i);
                w += piece;
                i = end;
                continue;
            }
        }
        if (c >= '0' && c <= '9') {
            unsigned char o[4];
            size_t end = 0;
            if (scan_ipv4((const unsigned char *)s, n, i, o, &end)) {
                size_t piece = public_ipv4(o) ? rlen : end - i;
                while (w + piece + 1 > cap) {
                    cap *= 2;
                    char *no = xrealloc(out, cap);
                    if (!no) {
                        free(out);
                        return NULL;
                    }
                    out = no;
                }
                if (public_ipv4(o))
                    memcpy(out + w, redact, rlen);
                else
                    memcpy(out + w, s + i, end - i);
                w += piece;
                i = end;
                continue;
            }
        }
        size_t l = utf8_len(c);
        if (i + l > n)
            l = n - i;
        while (w + l + 1 > cap) {
            cap *= 2;
            char *no = xrealloc(out, cap);
            if (!no) {
                free(out);
                return NULL;
            }
            out = no;
        }
        memcpy(out + w, s + i, l);
        w += l;
        i += l;
    }
    out[w] = '\0';
    return out;
}
