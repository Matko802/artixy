/* Ollama chat + reply sanitizers + per-channel history. */
#include "ai.h"
#include "util.h"

#include <ctype.h>
#include <curl/curl.h>
#include <jansson.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ------------------------------------------------------------------ */
/* defaults                                                             */
/* ------------------------------------------------------------------ */

const char *ai_default_model(void) {
    return "llama3.1";
}

const char *ai_default_host(void) {
    return "http://127.0.0.1:11434";
}

double ai_default_temperature(void) {
    return 0.8;
}

/* Resolve effective Ollama host: env override wins, then configured. */
void ai_resolve_host(const char *configured, char *out, size_t n) {
    const char *e = getenv("OLLAMA_HOST");
    if (!e || !*e)
        e = getenv("OLLAMA_URL");
    const char *v = (e && *e) ? e : (configured ? configured : "");
    while (*v == ' ' || *v == '\t')
        v++;
    snprintf(out, n, "%s", v);
    size_t m = strlen(out);
    while (m > 0 && (out[m - 1] == '/' || out[m - 1] == ' ' || out[m - 1] == '\t'))
        out[--m] = '\0';
    if (!out[0])
        snprintf(out, n, "%s", ai_default_host());
}

double ai_clamp_temperature(double t) {
    if (t != t)
        return 0.8;
    if (t < 0.0)
        return 0.0;
    if (t > 2.0)
        return 2.0;
    return t;
}

bool ai_valid_model_name(const char *s) {
    if (!s)
        return false;
    while (*s == ' ' || *s == '\t')
        s++;
    size_t len = 0;
    const char *p = s;
    while (*p && *p != ' ' && *p != '\t' && *p != '\n') {
        p++;
        len++;
    }
    const char *q = p;
    while (*q == ' ' || *q == '\t' || *q == '\n')
        q++;
    if (len == 0 || len > 128 || *q != '\0')
        return false;
    if (s[0] == '.' || s[0] == '-' || s[0] == '/' || s[0] == ':')
        return false;
    char last = s[len - 1];
    if (last == '.' || last == '-' || last == '/' || last == ':')
        return false;
    for (size_t i = 0; i < len; i++) {
        char c = s[i];
        bool ok = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
                  (c >= '0' && c <= '9') || c == '.' || c == '_' ||
                  c == '-' || c == ':' || c == '/';
        if (!ok)
            return false;
    }
    char tmp[160];
    if (len >= sizeof tmp)
        return false;
    memcpy(tmp, s, len);
    tmp[len] = '\0';
    if (strstr(tmp, "..") || strstr(tmp, "//"))
        return false;
    return true;
}

/* ------------------------------------------------------------------ */
/* reply cleaning                                                       */
/* ------------------------------------------------------------------ */

char *ai_clean_reply(const char *text) {
    if (!text)
        return xstrdup("");
    size_t cap = strlen(text) + 1;
    char *out = xmalloc(cap);
    if (!out)
        return NULL;
    size_t w = 0;
    int blanks = 0;
    const char *p = text;
    while (*p) {
        const char *e = strchr(p, '\n');
        size_t ll = e ? (size_t)(e - p) : strlen(p);
        bool empty = true;
        for (size_t i = 0; i < ll; i++) {
            if (p[i] != ' ' && p[i] != '\t' && p[i] != '\r') {
                empty = false;
                break;
            }
        }
        if (empty) {
            blanks++;
            if (blanks <= 1)
                out[w++] = '\n';
        } else {
            blanks = 0;
            memcpy(out + w, p, ll);
            w += ll;
            out[w++] = '\n';
        }
        p = e ? e + 1 : p + ll;
    }
    while (w > 0 && out[w - 1] == '\n')
        w--;
    /* trim leading newlines */
    size_t start = 0;
    while (start < w && out[start] == '\n')
        start++;
    if (start) {
        memmove(out, out + start, w - start + 1);
        w -= start;
    }
    out[w] = '\0';
    return out;
}

/* strip "[name]:" prefixes then "name:" prefixes (longest first) */
char *ai_strip_leading_speaker(const char *text, const char **names,
                               size_t n_names) {
    char *out = xstrdup(text ? text : "");
    if (!out)
        return NULL;
    for (;;) {
        char *t = out;
        while (*t == ' ' || *t == '\t' || *t == '\n' || *t == '\r')
            t++;
        if (*t != '[')
            break;
        char *end = strchr(t, ']');
        if (!end || end - t <= 1 || end - t > 65)
            break;
        char *after = end + 1;
        while (*after == ' ' || *after == '\t' || *after == '\n' || *after == '\r')
            after++;
        if (*after != ':')
            break;
        after++;
        while (*after == ' ' || *after == '\t' || *after == '\n' || *after == '\r')
            after++;
        memmove(out, after, strlen(after) + 1);
    }
    /* longest-first name strip */
    size_t *order = xmalloc((n_names ? n_names : 1) * sizeof *order);
    if (order) {
        for (size_t i = 0; i < n_names; i++)
            order[i] = i;
        for (size_t i = 0; i < n_names; i++) {
            for (size_t j = i + 1; j < n_names; j++) {
                const char *a = names[order[i]] ? names[order[i]] : "";
                const char *b = names[order[j]] ? names[order[j]] : "";
                while (*a == ' ' || *a == '\t')
                    a++;
                while (*b == ' ' || *b == '\t')
                    b++;
                if (strlen(b) > strlen(a)) {
                    size_t tmp = order[i];
                    order[i] = order[j];
                    order[j] = tmp;
                }
            }
        }
        for (;;) {
            char *t = out;
            while (*t == ' ' || *t == '\t' || *t == '\n' || *t == '\r')
                t++;
            bool hit = false;
            for (size_t k = 0; k < n_names && !hit; k++) {
                const char *nm = names[order[k]];
                if (!nm)
                    continue;
                while (*nm == ' ' || *nm == '\t')
                    nm++;
                /* rtrim a copy */
                char nb[80];
                snprintf(nb, sizeof nb, "%s", nm);
                size_t nl = strlen(nb);
                while (nl > 0 && (nb[nl - 1] == ' ' || nb[nl - 1] == '\t'))
                    nb[--nl] = '\0';
                if (!nl)
                    continue;
                if (strncmp(t, nb, nl) == 0 && t[nl] == ':') {
                    char *after = t + nl + 1;
                    while (*after == ' ' || *after == '\t' || *after == '\n' ||
                           *after == '\r')
                        after++;
                    memmove(out, after, strlen(after) + 1);
                    hit = true;
                }
            }
            if (!hit)
                break;
        }
        free(order);
    }
    return out;
}

static const char *meta_starters[] = {
    "let me summarize", "just to sum it up", "just to summarize",
    "let's simplify", "let me simplify", "to simplify,", "too much info",
    "i got carried away", "i think i got", "here is a summary",
    "here's a summary", "to summarize,", "in summary,", NULL
};

static long meta_preamble_len(const char *text) {
    while (*text == ' ' || *text == '\t' || *text == '\n' || *text == '\r')
        text++;
    char low[256];
    size_t i = 0;
    for (; text[i] && i + 1 < sizeof low; i++)
        low[i] = (char)tolower((unsigned char)text[i]);
    low[i] = '\0';
    for (size_t k = 0; meta_starters[k]; k++) {
        const char *s = meta_starters[k];
        size_t sl = strlen(s);
        if (strncmp(low, s, sl) != 0)
            continue;
        const char *rest = text + sl;
        bool ends_comma = sl > 0 && s[sl - 1] == ',';
        if (ends_comma || rest[0] == ':' || rest[0] == ',') {
            long cut = (long)sl + ((rest[0] == ':' || rest[0] == ',') ? 1 : 0);
            const char *tail = text + cut;
            while (*tail == ' ' || *tail == '\t' || *tail == '\n' || *tail == '\r')
                tail++;
            if (!*tail)
                return 0;
            return cut;
        }
        const char *dot = strpbrk(rest, ".!?");
        if (!dot)
            return 0;
        long cut = (long)(dot - text) + 1;
        const char *tail = text + cut;
        while (*tail == ' ' || *tail == '\t' || *tail == '\n' || *tail == '\r')
            tail++;
        if (!*tail)
            return 0;
        return cut;
    }
    return 0;
}

char *ai_strip_meta_preamble(const char *text) {
    char *out = xstrdup(text ? text : "");
    if (!out)
        return NULL;
    for (;;) {
        long cut = meta_preamble_len(out);
        if (cut <= 0)
            break;
        char *t = out;
        while (*t == ' ' || *t == '\t' || *t == '\n' || *t == '\r')
            t++;
        /* cut is relative to trimmed start */
        size_t off = (size_t)(t - out) + (size_t)cut;
        memmove(out, out + off, strlen(out + off) + 1);
        /* re-trim leading */
        t = out;
        while (*t == ' ' || *t == '\t' || *t == '\n' || *t == '\r')
            t++;
        if (t != out)
            memmove(out, t, strlen(t) + 1);
    }
    return out;
}

char *ai_sanitize_reply(const char *raw, const char **names, size_t n_names) {
    char *c = ai_clean_reply(raw);
    if (!c)
        return NULL;
    char *s = ai_strip_leading_speaker(c, names, n_names);
    free(c);
    if (!s)
        return NULL;
    char *m = ai_strip_meta_preamble(s);
    free(s);
    return m;
}

char *ai_speaker_tag(const char *raw) {
    if (!raw)
        return xstrdup("someone");
    /* join whitespace, drop [] and controls, cap 64 chars */
    size_t cap = strlen(raw) + 1;
    char *flat = xmalloc(cap);
    if (!flat)
        return NULL;
    size_t w = 0;
    bool in_ws = true;
    for (size_t i = 0; raw[i]; i++) {
        unsigned char ch = (unsigned char)raw[i];
        if (ch == '[' || ch == ']' || ch < 32 || ch == 127)
            ch = ' ';
        if (ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r') {
            if (!in_ws)
                flat[w++] = ' ';
            in_ws = true;
        } else {
            flat[w++] = (char)ch;
            in_ws = false;
        }
    }
    if (w > 0 && flat[w - 1] == ' ')
        w--;
    flat[w] = '\0';
    /* char-cap 64 (byte approx is fine for tags) */
    if (w > 64) {
        w = 64;
        while (w > 0 && (flat[w] & 0xc0) == 0x80)
            w--;
        flat[w] = '\0';
    }
    if (!flat[0]) {
        free(flat);
        return xstrdup("someone");
    }
    return flat;
}

char *ai_strip_mention(const char *content, uint64_t bot_id) {
    if (!content)
        return xstrdup("");
    char pat1[64], pat2[64];
    snprintf(pat1, sizeof pat1, "<@%llu>", (unsigned long long)bot_id);
    snprintf(pat2, sizeof pat2, "<@!%llu>", (unsigned long long)bot_id);
    size_t cap = strlen(content) + 1;
    char *out = xmalloc(cap);
    if (!out)
        return NULL;
    size_t w = 0;
    for (size_t i = 0; content[i];) {
        if (strncmp(content + i, pat1, strlen(pat1)) == 0) {
            i += strlen(pat1);
            continue;
        }
        if (strncmp(content + i, pat2, strlen(pat2)) == 0) {
            i += strlen(pat2);
            continue;
        }
        out[w++] = content[i++];
    }
    out[w] = '\0';
    /* trim */
    size_t s = 0;
    while (out[s] == ' ' || out[s] == '\t' || out[s] == '\n' || out[s] == '\r')
        s++;
    if (s)
        memmove(out, out + s, strlen(out + s) + 1);
    size_t n = strlen(out);
    while (n > 0 && (out[n - 1] == ' ' || out[n - 1] == '\t' ||
                     out[n - 1] == '\n' || out[n - 1] == '\r'))
        out[--n] = '\0';
    return out;
}

static void lower_of(const char *s, char *out, size_t n) {
    size_t i = 0;
    for (; s[i] && i + 1 < n; i++)
        out[i] = (char)tolower((unsigned char)s[i]);
    out[i] = '\0';
}

bool ai_mentions_name(const char *content) {
    if (!content)
        return false;
    size_t n = strlen(content) + 1;
    char *low = xmalloc(n);
    if (!low)
        return false;
    lower_of(content, low, n);
    bool hit = strstr(low, "artixy") != NULL;
    free(low);
    return hit;
}

char *ai_strip_name(const char *content) {
    if (!content)
        return xstrdup("");
    size_t cap = strlen(content) + 1;
    char *out = xmalloc(cap);
    if (!out)
        return NULL;
    size_t w = 0;
    bool first = true;
    const char *p = content;
    while (*p) {
        while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')
            p++;
        if (!*p)
            break;
        const char *e = p;
        while (*e && *e != ' ' && *e != '\t' && *e != '\n' && *e != '\r')
            e++;
        size_t wl = (size_t)(e - p);
        char low[256];
        size_t cl = wl < sizeof(low) - 1 ? wl : sizeof(low) - 1;
        for (size_t i = 0; i < cl; i++)
            low[i] = (char)tolower((unsigned char)p[i]);
        low[cl] = '\0';
        if (!strstr(low, "artixy")) {
            if (!first)
                out[w++] = ' ';
            memcpy(out + w, p, wl);
            w += wl;
            first = false;
        }
        p = e;
    }
    out[w] = '\0';
    return out;
}

char **ai_chunk_reply(const char *s, size_t *n_out) {
    const size_t max = 1900, max_chunks = 4;
    *n_out = 0;
    if (!s)
        s = "";
    while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r')
        s++;
    /* count chars */
    size_t total = 0;
    for (const char *p = s; *p;) {
        unsigned char b = (unsigned char)*p;
        p += ((b & 0x80) == 0) ? 1 : ((b & 0xe0) == 0xc0) ? 2 :
               ((b & 0xf0) == 0xe0)                        ? 3 :
                                                             4;
        total++;
    }
    char **chunks = xmalloc(max_chunks * sizeof *chunks);
    if (!chunks)
        return NULL;
    if (total <= max) {
        chunks[0] = xstrdup(s);
        /* rtrim copy */
        if (chunks[0]) {
            size_t n = strlen(chunks[0]);
            while (n > 0 && (chunks[0][n - 1] == ' ' || chunks[0][n - 1] == '\t' ||
                             chunks[0][n - 1] == '\n' || chunks[0][n - 1] == '\r'))
                chunks[0][--n] = '\0';
            *n_out = 1;
            return chunks;
        }
        free(chunks);
        return NULL;
    }
    /* line-based packing */
    size_t nch = 0;
    char *cur = xstrdup("");
    size_t cur_chars = 0;
    if (!cur) {
        free(chunks);
        return NULL;
    }
    const char *p = s;
    while (*p && nch < max_chunks) {
        const char *e = strchr(p, '\n');
        size_t bl = e ? (size_t)(e - p) : strlen(p);
        /* char count of line */
        size_t ll = 0;
        for (size_t i = 0; i < bl;) {
            unsigned char b = (unsigned char)p[i];
            i += ((b & 0x80) == 0) ? 1 : ((b & 0xe0) == 0xc0) ? 2 :
                   ((b & 0xf0) == 0xe0)                        ? 3 :
                                                                 4;
            ll++;
        }
        if (ll + 1 > max) {
            /* hard-split the long line by chars */
            if (cur_chars) {
                chunks[nch++] = cur;
                cur = xstrdup("");
                cur_chars = 0;
                if (!cur)
                    goto fail;
            }
            size_t off = 0;
            while (off < bl && nch < max_chunks) {
                /* take max chars */
                size_t take = 0, bytes = 0;
                while (take < max && off + bytes < bl) {
                    unsigned char b = (unsigned char)p[off + bytes];
                    size_t cl = ((b & 0x80) == 0) ? 1 :
                                ((b & 0xe0) == 0xc0) ? 2 :
                                ((b & 0xf0) == 0xe0) ? 3 :
                                                       4;
                    if (take + 1 > max)
                        break;
                    bytes += cl;
                    take++;
                }
                char *piece = xmalloc(bytes + 1);
                if (!piece)
                    goto fail;
                memcpy(piece, p + off, bytes);
                piece[bytes] = '\0';
                chunks[nch++] = piece;
                off += bytes;
            }
            p = e ? e + 1 : p + bl;
            continue;
        }
        if (cur_chars + ll + 1 > max) {
            chunks[nch++] = cur;
            cur = xstrdup("");
            cur_chars = 0;
            if (!cur || nch >= max_chunks)
                goto fail;
        }
        size_t cur_len = strlen(cur);
        char *nc = xrealloc(cur, cur_len + bl + 2);
        if (!nc)
            goto fail;
        cur = nc;
        memcpy(cur + cur_len, p, bl);
        cur[cur_len + bl] = '\n';
        cur[cur_len + bl + 1] = '\0';
        cur_chars += ll + 1;
        p = e ? e + 1 : p + bl;
    }
    /* rtrim cur */
    {
        size_t n = strlen(cur);
        while (n > 0 && (cur[n - 1] == ' ' || cur[n - 1] == '\t' ||
                         cur[n - 1] == '\n' || cur[n - 1] == '\r'))
            cur[--n] = '\0';
        if (n > 0 && nch < max_chunks)
            chunks[nch++] = cur;
        else
            free(cur);
    }
    *n_out = nch;
    if (!nch) {
        /* fallback: first max chars */
        size_t bytes = 0, take = 0;
        while (take < max && s[bytes]) {
            unsigned char b = (unsigned char)s[bytes];
            size_t cl = ((b & 0x80) == 0) ? 1 : ((b & 0xe0) == 0xc0) ? 2 :
                        ((b & 0xf0) == 0xe0)                        ? 3 :
                                                                      4;
            bytes += cl;
            take++;
        }
        char *one = xmalloc(bytes + 1);
        if (!one) {
            free(chunks);
            return NULL;
        }
        memcpy(one, s, bytes);
        one[bytes] = '\0';
        chunks[0] = one;
        *n_out = 1;
    }
    return chunks;
fail:
    free(cur);
    ai_chunks_free(chunks, nch);
    return NULL;
}

void ai_chunks_free(char **chunks, size_t n) {
    if (!chunks)
        return;
    for (size_t i = 0; i < n; i++)
        free(chunks[i]);
    free(chunks);
}

/* ------------------------------------------------------------------ */
/* error classification                                                 */
/* ------------------------------------------------------------------ */

static bool contains_ci(const char *hay, const char *needle) {
    if (!hay || !needle)
        return false;
    size_t hn = strlen(hay), nn = strlen(needle);
    if (!nn)
        return false;
    for (size_t i = 0; i + nn <= hn; i++) {
        size_t k = 0;
        while (k < nn &&
               tolower((unsigned char)hay[i + k]) == tolower((unsigned char)needle[k]))
            k++;
        if (k == nn)
            return true;
    }
    return false;
}

bool ai_is_rate_limit_err(const char *s) {
    return contains_ci(s, "429") || contains_ci(s, "rate limit") ||
           contains_ci(s, "rate_limit") || contains_ci(s, "rate-limited") ||
           contains_ci(s, "too many requests") || contains_ci(s, "quota");
}

bool ai_is_api_full_err(const char *s) {
    if (ai_is_rate_limit_err(s))
        return true;
    return contains_ci(s, "503") || contains_ci(s, "529") ||
           contains_ci(s, "overload") || contains_ci(s, "capacity") ||
           contains_ci(s, "server is busy") || contains_ci(s, "try again in a bit") ||
           contains_ci(s, "api full");
}

const char *ai_api_full_message(void) {
    return "Sorry, I'm running hot right now (API full/rate limited) — try again in a minute.";
}

bool ai_stale_history_line(const char *s) {
    return contains_ci(s, "running hot right now");
}

/* ------------------------------------------------------------------ */
/* history (mutex-guarded, max 200 channels)                            */
/* ------------------------------------------------------------------ */

#define HIST_MAX_CH 200
#define HIST_MAX_MSGS 10
#define HIST_MAX_CHARS 3000

typedef struct {
    char *role;
    char *content;
} hist_item_t;

typedef struct {
    uint64_t channel;
    hist_item_t *items;
    size_t len, cap;
} hist_ch_t;

static pthread_mutex_t hist_mu = PTHREAD_MUTEX_INITIALIZER;
static hist_ch_t *hist_chs = NULL;
static size_t hist_n = 0, hist_cap = 0;

static hist_ch_t *hist_find_locked(uint64_t ch) {
    for (size_t i = 0; i < hist_n; i++) {
        if (hist_chs[i].channel == ch)
            return &hist_chs[i];
    }
    return NULL;
}

static void hist_trim_locked(hist_ch_t *h) {
    while (h->len > HIST_MAX_MSGS) {
        free(h->items[0].role);
        free(h->items[0].content);
        memmove(h->items, h->items + 1, (h->len - 1) * sizeof *h->items);
        h->len--;
    }
    for (;;) {
        size_t total = 0;
        for (size_t i = 0; i < h->len; i++)
            total += strlen(h->items[i].content);
        if (total <= HIST_MAX_CHARS || !h->len)
            break;
        free(h->items[0].role);
        free(h->items[0].content);
        memmove(h->items, h->items + 1, (h->len - 1) * sizeof *h->items);
        h->len--;
    }
}

void ai_history_push(uint64_t channel, const char *role, const char *content) {
    pthread_mutex_lock(&hist_mu);
    hist_ch_t *h = hist_find_locked(channel);
    if (!h) {
        if (hist_n >= HIST_MAX_CH) {
            /* evict oldest (index 0) */
            for (size_t i = 0; i < hist_chs[0].len; i++) {
                free(hist_chs[0].items[i].role);
                free(hist_chs[0].items[i].content);
            }
            free(hist_chs[0].items);
            memmove(hist_chs, hist_chs + 1, (hist_n - 1) * sizeof *hist_chs);
            hist_n--;
        }
        if (hist_n == hist_cap) {
            size_t nc = hist_cap ? hist_cap * 2 : 16;
            hist_ch_t *nh = xrealloc(hist_chs, nc * sizeof *nh);
            if (!nh) {
                pthread_mutex_unlock(&hist_mu);
                return;
            }
            hist_chs = nh;
            hist_cap = nc;
        }
        h = &hist_chs[hist_n++];
        h->channel = channel;
        h->items = NULL;
        h->len = h->cap = 0;
    }
    if (h->len == h->cap) {
        size_t nc = h->cap ? h->cap * 2 : 8;
        hist_item_t *ni = xrealloc(h->items, nc * sizeof *ni);
        if (!ni) {
            pthread_mutex_unlock(&hist_mu);
            return;
        }
        h->items = ni;
        h->cap = nc;
    }
    h->items[h->len].role = xstrdup(role ? role : "");
    h->items[h->len].content = xstrdup(content ? content : "");
    if (h->items[h->len].role && h->items[h->len].content)
        h->len++;
    else {
        free(h->items[h->len].role);
        free(h->items[h->len].content);
    }
    hist_trim_locked(h);
    pthread_mutex_unlock(&hist_mu);
}

void ai_history_clear(uint64_t channel) {
    pthread_mutex_lock(&hist_mu);
    for (size_t i = 0; i < hist_n; i++) {
        if (hist_chs[i].channel == channel) {
            for (size_t k = 0; k < hist_chs[i].len; k++) {
                free(hist_chs[i].items[k].role);
                free(hist_chs[i].items[k].content);
            }
            free(hist_chs[i].items);
            memmove(hist_chs + i, hist_chs + i + 1,
                    (hist_n - i - 1) * sizeof *hist_chs);
            hist_n--;
            break;
        }
    }
    pthread_mutex_unlock(&hist_mu);
}

void ai_history_clear_all(void) {
    pthread_mutex_lock(&hist_mu);
    for (size_t i = 0; i < hist_n; i++) {
        for (size_t k = 0; k < hist_chs[i].len; k++) {
            free(hist_chs[i].items[k].role);
            free(hist_chs[i].items[k].content);
        }
        free(hist_chs[i].items);
    }
    free(hist_chs);
    hist_chs = NULL;
    hist_n = hist_cap = 0;
    pthread_mutex_unlock(&hist_mu);
}

void ai_record_artixy(uint64_t channel, const char *text) {
    if (!text)
        return;
    while (*text == ' ' || *text == '\t' || *text == '\n' || *text == '\r')
        text++;
    if (!*text)
        return;
    /* cap 1500 chars */
    size_t chars = 0;
    const char *p = text;
    while (*p && chars < 1500) {
        unsigned char b = (unsigned char)*p;
        p += ((b & 0x80) == 0) ? 1 : ((b & 0xe0) == 0xc0) ? 2 :
               ((b & 0xf0) == 0xe0)                        ? 3 :
                                                             4;
        chars++;
    }
    size_t bl = (size_t)(p - text);
    char *kept = xmalloc(bl + 1);
    if (!kept)
        return;
    memcpy(kept, text, bl);
    kept[bl] = '\0';
    ai_history_push(channel, "assistant", kept);
    free(kept);
}

size_t ai_history_count(uint64_t channel) {
    pthread_mutex_lock(&hist_mu);
    hist_ch_t *h = hist_find_locked(channel);
    size_t n = h ? h->len : 0;
    pthread_mutex_unlock(&hist_mu);
    return n;
}

/* ------------------------------------------------------------------ */
/* transcript                                                           */
/* ------------------------------------------------------------------ */

char *ai_build_transcript(const ai_hist_item_t *past, size_t n,
                          const char *tagged, const char **names,
                          size_t n_names) {
    size_t cap = 4096, len = 0;
    char *out = xmalloc(cap);
    if (!out)
        return NULL;
    out[0] = '\0';
    for (size_t i = 0; i < n; i++) {
        const char *piece = past[i].content ? past[i].content : "";
        char *use = NULL;
        if (past[i].role && strcmp(past[i].role, "assistant") == 0) {
            use = ai_sanitize_reply(piece, names, n_names);
            if (!use || !*use || ai_stale_history_line(use)) {
                free(use);
                continue;
            }
        } else {
            use = xstrdup(piece);
        }
        if (!use)
            goto fail;
        size_t ul = strlen(use);
        if (len + ul + 2 > cap) {
            cap = (len + ul + 2) * 2;
            char *no = xrealloc(out, cap);
            if (!no) {
                free(use);
                goto fail;
            }
            out = no;
        }
        memcpy(out + len, use, ul);
        len += ul;
        out[len++] = '\n';
        out[len] = '\0';
        free(use);
    }
    {
        size_t tl = strlen(tagged ? tagged : "");
        if (len + tl + 1 > cap) {
            char *no = xrealloc(out, len + tl + 1);
            if (!no)
                goto fail;
            out = no;
        }
        memcpy(out + len, tagged ? tagged : "", tl + 1);
    }
    return out;
fail:
    free(out);
    return NULL;
}

/* ------------------------------------------------------------------ */
/* Ollama HTTP                                                          */
/* ------------------------------------------------------------------ */

typedef struct {
    char *p;
    size_t n, cap;
} http_buf_t;

static size_t http_write(char *ptr, size_t sz, size_t nm, void *ud) {
    http_buf_t *b = ud;
    size_t n = sz * nm;
    if (b->n + n + 1 > b->cap) {
        size_t nc = (b->n + n + 1) * 2;
        char *np = xrealloc(b->p, nc);
        if (!np)
            return 0;
        b->p = np;
        b->cap = nc;
    }
    memcpy(b->p + b->n, ptr, n);
    b->n += n;
    b->p[b->n] = '\0';
    return n;
}

/* POST JSON, parse response JSON. Returns 0 ok; -1 transport/parse;
 * -2 HTTP error (detail set to "ollama HTTP <code>: <body>"). */
static int http_post_json(const char *url, const char *body, long timeout_s,
                          json_t **resp_out, char **detail) {
    if (detail)
        *detail = NULL;
    *resp_out = NULL;
    CURL *h = curl_easy_init();
    if (!h)
        return -1;
    struct curl_slist *hdrs = curl_slist_append(NULL, "Content-Type: application/json");
    http_buf_t b = { NULL, 0, 0 };
    curl_easy_setopt(h, CURLOPT_URL, url);
    curl_easy_setopt(h, CURLOPT_HTTPHEADER, hdrs);
    curl_easy_setopt(h, CURLOPT_USERAGENT, "DiscordBot (artixy-c, 0.3.0)");
    curl_easy_setopt(h, CURLOPT_TIMEOUT, timeout_s);
    curl_easy_setopt(h, CURLOPT_POSTFIELDS, body);
    curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, http_write);
    curl_easy_setopt(h, CURLOPT_WRITEDATA, &b);
    CURLcode rc = curl_easy_perform(h);
    long code = 0;
    if (rc == CURLE_OK)
        curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, &code);
    curl_slist_free_all(hdrs);
    curl_easy_cleanup(h);
    if (rc != CURLE_OK) {
        free(b.p);
        return -1;
    }
    if (code < 200 || code >= 300) {
        if (detail) {
            size_t bl = b.n > 300 ? 300 : b.n;
            *detail = xmalloc(bl + 64);
            if (*detail)
                snprintf(*detail, bl + 64, "ollama HTTP %ld: %.*s", code,
                         (int)bl, b.p ? b.p : "");
        }
        free(b.p);
        return -2;
    }
    json_error_t e;
    *resp_out = json_loads(b.p ? b.p : "", 0, &e);
    free(b.p);
    return *resp_out ? 0 : -1;
}

static int http_get_json(const char *url, long timeout_s, json_t **resp_out) {
    *resp_out = NULL;
    CURL *h = curl_easy_init();
    if (!h)
        return -1;
    http_buf_t b = { NULL, 0, 0 };
    curl_easy_setopt(h, CURLOPT_URL, url);
    curl_easy_setopt(h, CURLOPT_USERAGENT, "DiscordBot (artixy-c, 0.3.0)");
    curl_easy_setopt(h, CURLOPT_TIMEOUT, timeout_s);
    curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, http_write);
    curl_easy_setopt(h, CURLOPT_WRITEDATA, &b);
    CURLcode rc = curl_easy_perform(h);
    long code = 0;
    if (rc == CURLE_OK)
        curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, &code);
    curl_easy_cleanup(h);
    if (rc != CURLE_OK || code < 200 || code >= 300) {
        free(b.p);
        return -1;
    }
    json_error_t e;
    *resp_out = json_loads(b.p ? b.p : "", 0, &e);
    free(b.p);
    return *resp_out ? 0 : -1;
}

/* One /api/generate call. Returns malloc'd raw reply or NULL (errmsg set). */
static char *generate_once(const char *url, const char *model,
                           const char *system, const char *prompt,
                           double temperature, bool think, char **errmsg) {
    *errmsg = NULL;
    json_t *req = json_pack("{s:s, s:s, s:b, s:b, s:s, s:{s:f}}", "model",
                            model, "prompt", prompt, "stream", 0, "think",
                            think ? 1 : 0, "keep_alive", "10m", "options",
                            "temperature", ai_clamp_temperature(temperature));
    if (!req) {
        *errmsg = xstrdup("out of memory");
        return NULL;
    }
    if (system && *system)
        json_object_set_new(req, "system", json_string(system));
    char *body = json_dumps(req, JSON_COMPACT);
    json_decref(req);
    if (!body) {
        *errmsg = xstrdup("out of memory");
        return NULL;
    }
    json_t *resp = NULL;
    char *detail = NULL;
    int rc = http_post_json(url, body, 180, &resp, &detail);
    free(body);
    if (rc != 0) {
        if (rc == -2 && detail)
            *errmsg = detail;
        else {
            free(detail);
            *errmsg = xstrdup("ollama request failed");
        }
        json_decref(resp);
        return NULL;
    }
    char *out = NULL;
    json_t *r = json_object_get(resp, "response");
    if (json_is_string(r) && *json_string_value(r))
        out = xstrdup(json_string_value(r));
    else {
        json_t *m = json_object_get(resp, "message");
        if (json_is_object(m)) {
            json_t *cc = json_object_get(m, "content");
            if (json_is_string(cc) && *json_string_value(cc))
                out = xstrdup(json_string_value(cc));
        }
    }
    json_decref(resp);
    if (!out)
        *errmsg = xstrdup("ollama returned an empty reply");
    return out;
}

char *ai_glitch_text(const char *host, const char *model,
                     const char *system_prompt, double temperature, bool think) {
    /* host may be NULL in tests */
    char url[600];
    const char *h = host ? host : "";
    size_t hl = strlen(h);
    while (hl > 0 && h[hl - 1] == '/')
        hl--;
    snprintf(url, sizeof url, "%.*s/api/generate", (int)hl, h);
    char *err = NULL;
    char *raw = generate_once(url, model ? model : "", system_prompt,
                              "You just glitched out. Tell the user in one "
                              "short sentence, no details.",
                              temperature, think, &err);
    char *result = NULL;
    if (raw) {
        char *clean = ai_clean_reply(raw);
        char *strip = clean ? ai_strip_leading_speaker(clean, NULL, 0) : NULL;
        free(clean);
        char *fin = strip ? ai_strip_meta_preamble(strip) : NULL;
        free(strip);
        if (fin && *fin && !ai_is_api_full_err(fin)) {
            /* cap 300 chars */
            size_t chars = 0;
            const char *p = fin;
            while (*p && chars < 300) {
                unsigned char b = (unsigned char)*p;
                p += ((b & 0x80) == 0) ? 1 : ((b & 0xe0) == 0xc0) ? 2 :
                       ((b & 0xf0) == 0xe0)                        ? 3 :
                                                                     4;
                chars++;
            }
            size_t bl = (size_t)(p - fin);
            result = xmalloc(bl + 1);
            if (result) {
                memcpy(result, fin, bl);
                result[bl] = '\0';
            }
        }
        free(fin);
        free(raw);
    }
    if (err && ai_is_api_full_err(err)) {
        free(result);
        result = xstrdup(ai_api_full_message());
    }
    free(err);
    if (!result)
        result = xstrdup("sorry, glitched out — try again in a sec");
    return result ? result : xstrdup("sorry, glitched out — try again in a sec");
}

int ai_chat(const char *host, const char *model, uint64_t channel,
            const char *speaker, const char *prompt, const char *system,
            double temperature, bool think, char **out) {
    *out = NULL;
    char url[600];
    const char *h = host ? host : "";
    size_t hl = strlen(h);
    while (hl > 0 && h[hl - 1] == '/')
        hl--;
    snprintf(url, sizeof url, "%.*s/api/generate", (int)hl, h);
    char *stag = ai_speaker_tag(speaker);
    if (!stag)
        return -1;
    size_t tlen = strlen(stag) + strlen(prompt ? prompt : "") + 8;
    char *tagged = xmalloc(tlen);
    if (!tagged) {
        free(stag);
        return -1;
    }
    snprintf(tagged, tlen, "[%s]: %s", stag, prompt ? prompt : "");
    fprintf(stderr, "ai chat: model=%s temp=%g\n", model ? model : "?",
            ai_clamp_temperature(temperature));

    /* snapshot history + collect speaker names */
    pthread_mutex_lock(&hist_mu);
    size_t hn = 0;
    hist_ch_t *hc = hist_find_locked(channel);
    if (hc)
        hn = hc->len;
    ai_hist_item_t *past = NULL;
    const char **names = NULL;
    size_t nnames = 0, names_cap = 0;
    if (hn) {
        past = xmalloc(hn * sizeof *past);
        if (past) {
            for (size_t i = 0; i < hn; i++) {
                past[i].role = hc->items[i].role;
                past[i].content = hc->items[i].content;
            }
        }
    }
    /* seed names with current speaker, then scan user lines for [name] */
    names = xmalloc(8 * sizeof *names);
    if (names) {
        names_cap = 8;
        names[nnames++] = stag;
    }
    if (past && names) {
        for (size_t i = 0; i < hn; i++) {
            if (past[i].role && strcmp(past[i].role, "assistant") == 0)
                continue;
            const char *t = past[i].content;
            if (!t || t[0] != '[')
                continue;
            const char *end = strchr(t + 1, ']');
            if (!end || end - t - 1 <= 0 || end - t - 1 > 64)
                continue;
            char nb[72];
            size_t nl = (size_t)(end - t - 1);
            memcpy(nb, t + 1, nl);
            nb[nl] = '\0';
            /* trim */
            char *s = nb;
            while (*s == ' ' || *s == '\t')
                s++;
            size_t sl = strlen(s);
            while (sl > 0 && (s[sl - 1] == ' ' || s[sl - 1] == '\t'))
                s[--sl] = '\0';
            if (!sl)
                continue;
            bool dup = false;
            for (size_t k = 0; k < nnames; k++) {
                if (strcmp(names[k], s) == 0) {
                    dup = true;
                    break;
                }
            }
            if (dup)
                continue;
            if (nnames == names_cap) {
                size_t nc = names_cap * 2;
                const char **nn2 = xrealloc((void *)names, nc * sizeof *nn2);
                if (!nn2)
                    break;
                names = nn2;
                names_cap = nc;
            }
            char *cp = xstrdup(s);
            if (!cp)
                break;
            names[nnames++] = cp;
        }
    }
    pthread_mutex_unlock(&hist_mu);

    char *transcript = NULL;
    if (hn && !past) {
        /* snapshot alloc failed; proceed without history */
        transcript = xstrdup(tagged);
    } else {
        transcript = ai_build_transcript(past, hn, tagged, names, nnames);
    }
    free(past);
    if (!transcript) {
        for (size_t k = 1; k < nnames; k++)
            free((void *)names[k]);
        free(names);
        free(tagged);
        free(stag);
        return -1;
    }
    char *err = NULL;
    char *raw = generate_once(url, model ? model : "", system, transcript,
                              temperature, think, &err);
    free(transcript);
    if (!raw) {
        bool full = err && ai_is_api_full_err(err);
        fprintf(stderr, "ollama_chat: %s\n", err ? err : "unknown error");
        free(err);
        int rc = -1;
        if (full) {
            const char *fb = ai_api_full_message();
            ai_history_push(channel, "user", tagged);
            ai_history_push(channel, "assistant", fb);
            *out = xstrdup(fb);
            rc = *out ? 0 : -1;
        }
        for (size_t k = 1; k < nnames; k++)
            free((void *)names[k]);
        free(names);
        free(tagged);
        free(stag);
        return rc;
    }
    char *text = ai_sanitize_reply(raw, names, nnames);
    free(raw);
    for (size_t k = 1; k < nnames; k++)
        free((void *)names[k]);
    free(names);
    if (!text || !*text) {
        free(text);
        free(tagged);
        free(stag);
        return -1;
    }
    ai_history_push(channel, "user", tagged);
    free(tagged);
    ai_history_push(channel, "assistant", text);
    *out = text;
    free(stag);
    return 0;
}

int ai_model_present(const char *host, const char *model) {
    char url[600];
    const char *h = host ? host : "";
    size_t hl = strlen(h);
    while (hl > 0 && h[hl - 1] == '/')
        hl--;
    snprintf(url, sizeof url, "%.*s/api/tags", (int)hl, h);
    json_t *resp = NULL;
    if (http_get_json(url, 15, &resp) != 0)
        return -1;
    int found = 0;
    /* lowercase want + base */
    char want[160], base[160];
    snprintf(want, sizeof want, "%s", model ? model : "");
    for (char *p = want; *p; p++)
        *p = (char)tolower((unsigned char)*p);
    snprintf(base, sizeof base, "%s", want);
    char *colon = strchr(base, ':');
    if (colon)
        *colon = '\0';
    json_t *models = json_object_get(resp, "models");
    if (json_is_array(models)) {
        for (size_t i = 0; i < json_array_size(models); i++) {
            json_t *e = json_array_get(models, i);
            json_t *nm = json_object_get(e, "name");
            if (!json_is_string(nm))
                continue;
            char cur[160];
            snprintf(cur, sizeof cur, "%s", json_string_value(nm));
            for (char *p = cur; *p; p++)
                *p = (char)tolower((unsigned char)*p);
            char cbase[160];
            snprintf(cbase, sizeof cbase, "%s", cur);
            char *cc = strchr(cbase, ':');
            if (cc)
                *cc = '\0';
            if (strcmp(cur, want) == 0 || strcmp(cbase, base) == 0) {
                found = 1;
                break;
            }
        }
    }
    json_decref(resp);
    return found;
}
