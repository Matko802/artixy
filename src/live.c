/* Live `run` sessions: threaded guest execution with Discord edits. */
#include "live.h"
#include "b64.h"
#include "bot.h"
#include "discord.h"
#include "discord_internal.h"
#include "scrub.h"
#include "termrender.h"
#include "util.h"
#include "vm.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define LIVE_POLL_MS 180
#define LIVE_EDIT_MIN_MS 2000
#define LIVE_EDIT_MAX_FAILS 5
#define LIVE_GUEST_MAX_FAILS 15
#define LIVE_FRAME_BYTES "200000"
#define LIVE_GUEST_PATH "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
#define LIVE_TERM_COLS 120
#define LIVE_TERM_ROWS 40

typedef struct live_entry {
    uint64_t tag; /* == ack message id */
    uint64_t channel_id;
    uint64_t author_id;
    uint64_t msg_id;
    char *vm;
    char *cmd;
    char *runas; /* "" = none */
    char *out_f;
    char *code_f;
    char *in_f; /* NULL when fifo setup failed */
    long long pid;
    bool has_pid;
    bool dead;
    bool scrub_ip;
    discord_client_t *dc;
    live_map_t *map;
} live_entry_t;

struct live_map {
    pthread_mutex_t mu;
    live_entry_t **entries;
    size_t n, cap;
};

live_map_t *live_new(void) {
    live_map_t *lm = xmalloc(sizeof *lm);
    if (!lm)
        return NULL;
    pthread_mutex_init(&lm->mu, NULL);
    lm->entries = NULL;
    lm->n = lm->cap = 0;
    return lm;
}

static void entry_free(live_entry_t *e) {
    if (!e)
        return;
    free(e->vm);
    free(e->cmd);
    free(e->runas);
    free(e->out_f);
    free(e->code_f);
    free(e->in_f);
    free(e);
}

void live_free(live_map_t *lm) {
    if (!lm)
        return;
    pthread_mutex_lock(&lm->mu);
    for (size_t i = 0; i < lm->n; i++) {
        lm->entries[i]->dead = true;
        entry_free(lm->entries[i]);
    }
    free(lm->entries);
    pthread_mutex_unlock(&lm->mu);
    pthread_mutex_destroy(&lm->mu);
    free(lm);
}

/* Remove map entry only if it is still e (checked by tag+author+channel). */
static void remove_if_current(live_map_t *lm, live_entry_t *e) {
    pthread_mutex_lock(&lm->mu);
    for (size_t i = 0; i < lm->n; i++) {
        if (lm->entries[i] == e) {
            memmove(lm->entries + i, lm->entries + i + 1,
                    (lm->n - i - 1) * sizeof *lm->entries);
            lm->n--;
            break;
        }
    }
    pthread_mutex_unlock(&lm->mu);
}

/* ------------------------------------------------------------------ */
/* pure helpers                                                         */
/* ------------------------------------------------------------------ */

char *live_build_runner(const char *shell, const char *b64, const char *out_f,
                        const char *code_f, const char *input,
                        const char *runas) {
    char home_cd[256];
    if (runas && *runas)
        snprintf(home_cd, sizeof home_cd,
                 "cd ~%s 2>/dev/null || cd \"$HOME\" 2>/dev/null || cd /tmp; ",
                 runas);
    else
        snprintf(home_cd, sizeof home_cd,
                 "cd \"$HOME\" 2>/dev/null || cd /tmp; ");
    size_t need = strlen(b64) + strlen(out_f) + strlen(code_f) +
                  strlen(input) + strlen(shell) * 2 + strlen(home_cd) + 512;
    char *s = xmalloc(need);
    if (!s)
        return NULL;
    snprintf(s, need,
             "export CMD_DATA=\"$(echo %s | base64 -d)\"; "
             "if command -v script >/dev/null 2>&1; then "
             "script -qec \"export TERM=xterm-256color TERM_PROGRAM=rustyterm "
             "COLORTERM=truecolor; stty cols %d rows %d; %s -c 'export PATH=%s; "
             "%seval \\\"$CMD_DATA\\\"'\" /dev/null <> %s; "
             "else %s -c 'export TERM_PROGRAM=rustyterm COLORTERM=truecolor; "
             "export PATH=%s; %seval \"$CMD_DATA\"' <> %s; fi > %s 2>&1; "
             "echo $? > %s",
             b64, LIVE_TERM_COLS, LIVE_TERM_ROWS, shell, LIVE_GUEST_PATH,
             home_cd, input, shell, LIVE_GUEST_PATH, home_cd, input, out_f,
             code_f);
    return s;
}

char *live_mkfifo_script(const char *path, const char *runas) {
    size_t need = strlen(path) * 3 + (runas ? strlen(runas) : 0) + 64;
    char *s = xmalloc(need);
    if (!s)
        return NULL;
    if (runas && *runas)
        snprintf(s, need, "rm -f %s && mkfifo -m 600 %s && chown %s %s", path,
                 path, runas, path);
    else
        snprintf(s, need, "rm -f %s && mkfifo -m 600 %s", path, path);
    return s;
}

/* key tables (ported 1:1 from live.rs) */
static _Thread_local char ctrl_code[2];

static bool key_base(const char *word, const char **out) {
    char lower[32];
    size_t i = 0;
    for (; word[i] && i + 1 < sizeof lower; i++)
        lower[i] = (char)(word[i] >= 'A' && word[i] <= 'Z' ? word[i] + 32 : word[i]);
    lower[i] = '\0';
    if (strncmp(lower, ";ctrl+", 6) == 0) {
        const char *suf = lower + 6;
        if (suf[0] && !suf[1] && suf[0] >= 'a' && suf[0] <= 'z') {
            ctrl_code[0] = (char)(suf[0] - 'a' + 1);
            ctrl_code[1] = '\0';
            *out = ctrl_code;
            return true;
        }
        if (strcmp(suf, "return") == 0) {
            *out = "\x7f";
            return true;
        }
        if (strcmp(suf, "space") == 0) {
            *out = "\x00";
            return true;
        }
        if (strcmp(suf, "enter") == 0) {
            *out = "\n";
            return true;
        }
        if (strcmp(suf, "esc") == 0) {
            *out = "\x1b";
            return true;
        }
        if (strcmp(suf, "up") == 0) {
            *out = "\x1b[1;5A";
            return true;
        }
        if (strcmp(suf, "down") == 0) {
            *out = "\x1b[1;5B";
            return true;
        }
        if (strcmp(suf, "right") == 0) {
            *out = "\x1b[1;5C";
            return true;
        }
        if (strcmp(suf, "left") == 0) {
            *out = "\x1b[1;5D";
            return true;
        }
        return false;
    }
    if (strcmp(lower, ";return") == 0) {
        *out = "\x7f";
        return true;
    }
    if (strcmp(lower, ";space") == 0) {
        *out = " ";
        return true;
    }
    if (strcmp(lower, ";enter") == 0) {
        *out = "\r";
        return true;
    }
    if (strcmp(lower, ";esc") == 0) {
        *out = "\x1b";
        return true;
    }
    if (strcmp(lower, ";up") == 0) {
        *out = "\x1b[A";
        return true;
    }
    if (strcmp(lower, ";down") == 0) {
        *out = "\x1b[B";
        return true;
    }
    if (strcmp(lower, ";right") == 0) {
        *out = "\x1b[C";
        return true;
    }
    if (strcmp(lower, ";left") == 0) {
        *out = "\x1b[D";
        return true;
    }
    return false;
}

static void buf_put(char **buf, size_t *len, size_t *cap, const char *s,
                    size_t n) {
    if (*len + n + 1 > *cap) {
        size_t nc = (*len + n + 1) * 2;
        char *nb = xrealloc(*buf, nc);
        if (!nb)
            return;
        *buf = nb;
        *cap = nc;
    }
    memcpy(*buf + *len, s, n);
    *len += n;
    (*buf)[*len] = '\0';
}

/* expand one line: ;key [count] sequences, single spaces preserved */
static void expand_line(const char *line, char **buf, size_t *len,
                        size_t *cap) {
    /* split into words */
    const char *words[256];
    size_t wlen[256];
    size_t nw = 0;
    const char *p = line;
    while (*p && nw < 256) {
        while (*p == ' ' || *p == '\t')
            p++;
        if (!*p)
            break;
        words[nw] = p;
        while (*p && *p != ' ' && *p != '\t')
            p++;
        wlen[nw] = (size_t)(p - words[nw]);
        nw++;
    }
    bool first_tok = true;
    for (size_t i = 0; i < nw; i++) {
        char word[64];
        const char *key = NULL;
        if (wlen[i] < sizeof word) {
            memcpy(word, words[i], wlen[i]);
            word[wlen[i]] = '\0';
            if (!key_base(word, &key))
                key = NULL;
        }
        unsigned count = 1;
        if (key && i + 1 < nw) {
            /* purely-numeric next word within repeat range */
            bool numeric = wlen[i + 1] > 0;
            unsigned v = 0;
            for (size_t k = 0; k < wlen[i + 1] && numeric; k++) {
                char ch = words[i + 1][k];
                if (ch < '0' || ch > '9')
                    numeric = false;
                else
                    v = v * 10 + (unsigned)(ch - '0');
            }
            if (numeric && v >= 1 && v <= 100) {
                count = v;
                i++; /* consume the number */
            }
        }
        if (!first_tok)
            buf_put(buf, len, cap, " ", 1);
        first_tok = false;
        if (key) {
            size_t kl = strlen(key);
            for (unsigned k = 0; k < count; k++)
                buf_put(buf, len, cap, key, kl);
        } else {
            buf_put(buf, len, cap, words[i], wlen[i]);
        }
    }
}

char *live_expand_typed_input(const char *text) {
    if (!text)
        return xstrdup("");
    /* \n unescape */
    size_t cap = strlen(text) + 1;
    char *unesc = xmalloc(cap);
    if (!unesc)
        return NULL;
    size_t w = 0;
    for (size_t i = 0; text[i];) {
        if (text[i] == '\\' && text[i + 1] == 'n') {
            unesc[w++] = '\n';
            i += 2;
        } else {
            unesc[w++] = text[i++];
        }
    }
    unesc[w] = '\0';
    char *out = xmalloc(64);
    size_t len = 0, ocap = 0;
    if (!out) {
        free(unesc);
        return NULL;
    }
    ocap = 64;
    /* split lines, expand, rejoin */
    bool first = true;
    char *line = unesc;
    for (char *p = unesc;; p++) {
        if (*p == '\n' || !*p) {
            char save = *p;
            *p = '\0';
            if (!first)
                buf_put(&out, &len, &ocap, "\n", 1);
            first = false;
            expand_line(line, &out, &len, &ocap);
            if (!save)
                break;
            line = p + 1;
        }
    }
    free(unesc);
    if (!out)
        return xstrdup("");
    return out;
}

bool live_forward_input(const char *vm, const char *fifo, const char *runas,
                        const char *text) {
    char *b64 = b64_encode(text, strlen(text));
    if (!b64)
        return false;
    char inner[8192];
    snprintf(inner, sizeof inner, "echo %s | base64 -d >> %s", b64, fifo);
    free(b64);
    char script[8704];
    if (runas && *runas)
        snprintf(script, sizeof script, "timeout 8 su %s -s /bin/bash -c '%s'",
                 runas, inner);
    else
        snprintf(script, sizeof script, "timeout 8 bash -c '%s'", inner);
    char *args[] = { "-c", script, NULL };
    long long code = 0;
    int rc = vm_guest_exec(vm, "/bin/bash", args, false, 15, &code, NULL, NULL);
    if (rc != 0 || code != 0) {
        fprintf(stderr, "live: terminal input not delivered (rc %lld)\n", code);
        return false;
    }
    return true;
}

void live_cleanup_stale(const char *vm) {
    if (!vm || !*vm)
        return;
    char *args[] = {
        "-c",
        "rm -f /tmp/podbot-live-*; pkill -f 'podbot-live-' 2>/dev/null; true",
        NULL
    };
    long long code = 0;
    vm_guest_exec(vm, "/bin/bash", args, false, 15, &code, NULL, NULL);
}

/* ------------------------------------------------------------------ */
/* session map                                                          */
/* ------------------------------------------------------------------ */

bool live_session_for_msg(live_map_t *lm, uint64_t channel_id, uint64_t msg_id,
                          uint64_t *author_out, char **fifo_out) {
    bool hit = false;
    pthread_mutex_lock(&lm->mu);
    for (size_t i = 0; i < lm->n; i++) {
        live_entry_t *e = lm->entries[i];
        if (e->channel_id == channel_id && e->msg_id == msg_id && !e->dead) {
            *author_out = e->author_id;
            *fifo_out = e->in_f ? xstrdup(e->in_f) : NULL;
            hit = true;
            break;
        }
    }
    pthread_mutex_unlock(&lm->mu);
    return hit;
}

static uint64_t fnv1a(const char *s) {
    uint64_t h = 1469598103934665603ull;
    for (; *s; s++) {
        h ^= (unsigned char)*s;
        h *= 1099511628211ull;
    }
    return h;
}

static void cleanup_files(const char *vm, const char *out_f,
                          const char *code_f, const char *in_f) {
    long long code = 0;
    if (in_f) {
        char *args[] = { "-f", (char *)out_f, (char *)code_f, (char *)in_f,
                         NULL };
        vm_guest_exec(vm, "/bin/rm", args, false, 10, &code, NULL, NULL);
    } else {
        char *args[] = { "-f", (char *)out_f, (char *)code_f, NULL };
        vm_guest_exec(vm, "/bin/rm", args, false, 10, &code, NULL, NULL);
    }
}

static void close_live_message(discord_client_t *dc, uint64_t channel,
                               uint64_t msg) {
    char buf[256];
    bot_codeblock("This live session has been closed.", buf, sizeof buf);
    discord_edit_message(dc, channel, msg, buf, NULL, 0);
}

static void stall_note(discord_client_t *dc, uint64_t channel, const char *cmd,
                       const char *reason) {
    char combined[2048], out[2048];
    snprintf(combined, sizeof combined, "$ %s\n…live updates stopped: %s", cmd,
             reason);
    bot_plain_tail(combined, out, sizeof out);
    discord_send_message(dc, channel, out, NULL, 0, NULL);
}

/* Render one frame.
 * Preferred: short "$ cmd" caption + live.png rendering of the output.
 * Fallback (no font): plain-tail text of caption + output.
 * Sets *png_out NULL (and *png_len 0) when falling back. Caller frees both.
 * Returns 0 on success, -1 on OOM. */
static int render_frame(const char *cmd, const char *fetched, char **text_out,
                        unsigned char **png_out, size_t *png_len) {
    *text_out = NULL;
    *png_out = NULL;
    *png_len = 0;
    size_t caplen = strlen(cmd) + 3;
    char *caption = xmalloc(caplen);
    if (!caption)
        return -1;
    snprintf(caption, caplen, "$ %s", cmd);
    size_t fl = fetched ? strlen(fetched) : 0;
    unsigned char *png = NULL;
    size_t pn = 0;
    if (fl)
        png = termrender_png((const unsigned char *)fetched, fl, &pn);
    if (png && pn) {
        *text_out = caption;
        *png_out = png;
        *png_len = pn;
        return 0;
    }
    free(png);
    /* text fallback */
    size_t need = caplen + fl + 2;
    char *combined = xmalloc(need > 600000 ? 600000 : need);
    if (!combined) {
        free(caption);
        return -1;
    }
    if (need > 600000) {
        /* huge output: tail it crudely, plain_tail refines */
        size_t keep = 599000;
        const char *tail = fetched + (fl > keep ? fl - keep : 0);
        snprintf(combined, 600000, "$ %s\n%s", cmd, tail);
    } else {
        if (fl)
            snprintf(combined, need, "$ %s\n%s", cmd, fetched);
        else
            snprintf(combined, need, "$ %s", cmd);
    }
    char *tail = xmalloc(4096);
    if (!tail) {
        free(combined);
        free(caption);
        return -1;
    }
    bot_plain_tail(combined, tail, 4096);
    free(combined);
    free(caption);
    *text_out = tail;
    return 0;
}

static bool edit_frame(discord_client_t *dc, uint64_t channel, uint64_t msg,
                       const char *cmd, const char *output) {
    char *text = NULL;
    unsigned char *png = NULL;
    size_t pn = 0;
    if (render_frame(cmd, output, &text, &png, &pn) != 0 || !text)
        return false;
    bool ok;
    if (png && pn) {
        disc_file_t f = { "live.png", png, pn };
        ok = discord_edit_message(dc, channel, msg, text, &f, 1) == 0;
    } else {
        ok = discord_edit_message(dc, channel, msg, text, NULL, 0) == 0;
    }
    free(text);
    free(png);
    return ok;
}

static void *run_thread(void *arg) {
    live_entry_t *e = arg;
    discord_client_t *dc = e->dc;
    live_map_t *lm = e->map;

    if (e->runas[0] && !bot_valid_runas(e->runas)) {
        char buf[512];
        bot_plain_tail("linked linux account is invalid; ask the owner to "
                       "re-add you.",
                       buf, sizeof buf);
        discord_edit_message(dc, e->channel_id, e->msg_id, buf, NULL, 0);
        remove_if_current(lm, e);
        entry_free(e);
        return NULL;
    }

    char *b64 = b64_encode(e->cmd, strlen(e->cmd));
    if (!b64) {
        remove_if_current(lm, e);
        entry_free(e);
        return NULL;
    }
    const char *input = e->in_f ? e->in_f : "/dev/null";
    char *script = live_build_runner("bash", b64, e->out_f, e->code_f, input,
                                     e->runas);
    char *script_sh = live_build_runner("sh", b64, e->out_f, e->code_f, input,
                                        e->runas);
    free(b64);
    if (!script || !script_sh) {
        free(script);
        free(script_sh);
        remove_if_current(lm, e);
        entry_free(e);
        return NULL;
    }

    long long pid = -1;
    if (e->runas[0]) {
        char *args[] = { "-", e->runas, "-s", "/bin/bash", "-c", script, NULL };
        pid = vm_guest_launch_raw(e->vm, "su", args, false);
        if (pid < 0 && strstr(vm_error(), "No such file")) {
            char *a2[] = { "-", e->runas, "-s", "/bin/sh",
                           "-c", script_sh, NULL };
            pid = vm_guest_launch_raw(e->vm, "su", a2, false);
        }
    } else {
        char *args[] = { "-c", script, NULL };
        pid = vm_guest_launch_raw(e->vm, "/bin/bash", args, false);
        if (pid < 0 && strstr(vm_error(), "No such file")) {
            char *a2[] = { "-c", script_sh, NULL };
            pid = vm_guest_launch_raw(e->vm, "/bin/sh", a2, false);
        }
    }
    free(script);
    free(script_sh);
    if (pid < 0) {
        char buf[2048], eb[1024];
        bot_codeblock(vm_error(), eb, sizeof eb);
        snprintf(buf, sizeof buf, "%s", eb);
        char tail[2048];
        bot_plain_tail(buf, tail, sizeof tail);
        discord_edit_message(dc, e->channel_id, e->msg_id, tail, NULL, 0);
        cleanup_files(e->vm, e->out_f, e->code_f, e->in_f);
        remove_if_current(lm, e);
        entry_free(e);
        return NULL;
    }
    pthread_mutex_lock(&lm->mu);
    e->pid = pid;
    e->has_pid = true;
    pthread_mutex_unlock(&lm->mu);

    bool first = true;
    long long last_edit_ms = 0;
    int edit_fails = 0, guest_fails = 0;
    uint64_t posted_hash = 0;
    bool has_posted = false;
    struct timespec now_ts;
    clock_gettime(CLOCK_MONOTONIC, &now_ts);
    last_edit_ms = now_ts.tv_sec * 1000LL + now_ts.tv_nsec / 1000000LL;

    for (;;) {
        struct timespec wt = { 0, LIVE_POLL_MS * 1000000L };
        nanosleep(&wt, NULL);
        pthread_mutex_lock(&lm->mu);
        bool dead = e->dead;
        pthread_mutex_unlock(&lm->mu);
        if (dead || !discord_is_running(dc))
            break;
        /* fetch frame */
        char *fargs[] = { "-c", (char *)LIVE_FRAME_BYTES, e->out_f, NULL };
        long long fcode = 0;
        char *fetched = NULL, *ferr = NULL;
        int frc = vm_guest_exec(e->vm, "/usr/bin/tail", fargs, true, 15,
                                &fcode, &fetched, &ferr);
        free(ferr);
        if (frc != 0 || fcode != 0) {
            free(fetched);
            if (++guest_fails >= LIVE_GUEST_MAX_FAILS) {
                stall_note(dc, e->channel_id, e->cmd,
                           "guest agent stopped answering");
                close_live_message(dc, e->channel_id, e->msg_id);
                cleanup_files(e->vm, e->out_f, e->code_f, e->in_f);
                break;
            }
            first = false;
            struct timespec sl = { 2, 0 };
            nanosleep(&sl, NULL);
            continue;
        }
        char *frame = fetched ? fetched : xstrdup("");
        if (!frame) {
            guest_fails++;
            continue;
        }
        if (e->scrub_ip) {
            char *scrubbed = scrub_public_ip(frame);
            free(frame);
            frame = scrubbed ? scrubbed : xstrdup("");
            if (!frame) {
                guest_fails++;
                continue;
            }
        }
        long long exit_code = 0;
        int st = vm_guest_status(e->vm, pid, &exit_code);
        if (st < 0) {
            free(frame);
            if (++guest_fails >= LIVE_GUEST_MAX_FAILS) {
                stall_note(dc, e->channel_id, e->cmd,
                           "guest agent stopped answering");
                close_live_message(dc, e->channel_id, e->msg_id);
                cleanup_files(e->vm, e->out_f, e->code_f, e->in_f);
                break;
            }
            first = false;
            struct timespec sl = { 2, 0 };
            nanosleep(&sl, NULL);
            continue;
        }
        guest_fails = 0;
        if (st == 1) {
            /* finished: full output */
            char *gargs[] = { "-c", "500000", e->out_f, NULL };
            char *full = NULL, *fullerr = NULL;
            long long fc = 0;
            if (vm_guest_exec(e->vm, "/usr/bin/tail", gargs, true, 30, &fc,
                              &full, &fullerr) != 0 ||
                !full) {
                free(full);
                free(fullerr);
                full = frame;
                frame = NULL;
            } else {
                free(fullerr);
                free(frame);
                frame = full;
                if (e->scrub_ip) {
                    char *s2 = scrub_public_ip(frame);
                    free(frame);
                    frame = s2 ? s2 : xstrdup("");
                    if (!frame)
                        frame = xstrdup("(empty)");
                }
            }
            /* trim trailing newlines, append exit code */
            size_t fl = strlen(frame);
            while (fl > 0 && (frame[fl - 1] == '\n' || frame[fl - 1] == '\r'))
                frame[--fl] = '\0';
            char *output = frame;
            char *with_code = NULL;
            if (exit_code != 0) {
                size_t need = fl + 64;
                with_code = xmalloc(need);
                if (with_code) {
                    snprintf(with_code, need, "%s%s%sexit %lld", frame,
                             fl ? "\n" : "", "", exit_code);
                    free(frame);
                    output = with_code;
                }
            }
            if (first) {
                bool posted = edit_frame(dc, e->channel_id, e->msg_id, e->cmd,
                                         output);
                if (!posted)
                    stall_note(dc, e->channel_id, e->cmd,
                               "Discord kept rejecting message edits");
            } else if (exit_code == 0 && has_posted) {
                close_live_message(dc, e->channel_id, e->msg_id);
            } else {
                if (!edit_frame(dc, e->channel_id, e->msg_id, e->cmd, output))
                    stall_note(dc, e->channel_id, e->cmd,
                               "Discord kept rejecting message edits");
            }
            free(output);
            cleanup_files(e->vm, e->out_f, e->code_f, e->in_f);
            break;
        }
        /* still running: throttle */
        uint64_t digest = fnv1a(frame);
        clock_gettime(CLOCK_MONOTONIC, &now_ts);
        long long now_ms =
            now_ts.tv_sec * 1000LL + now_ts.tv_nsec / 1000000LL;
        bool due = !has_posted || digest != posted_hash;
        if (due && has_posted && now_ms - last_edit_ms < LIVE_EDIT_MIN_MS)
            due = false;
        if (!due) {
            free(frame);
            first = false;
            continue;
        }
        char *text = NULL;
        unsigned char *png = NULL;
        size_t pn = 0;
        bool ok = render_frame(e->cmd, frame, &text, &png, &pn) == 0 && text;
        free(frame);
        if (!ok) {
            free(text);
            free(png);
            first = false;
            continue;
        }
        bool posted;
        if (png && pn) {
            disc_file_t f = { "live.png", png, pn };
            posted = discord_edit_message(dc, e->channel_id, e->msg_id, text,
                                          &f, 1) == 0;
        } else {
            posted = discord_edit_message(dc, e->channel_id, e->msg_id, text,
                                          NULL, 0) == 0;
        }
        free(text);
        free(png);
        if (posted) {
            last_edit_ms = now_ms;
            edit_fails = 0;
            posted_hash = digest;
            has_posted = true;
        } else {
            edit_fails++;
            last_edit_ms = now_ms;
            fprintf(stderr, "live: edit %d/%d failed\n", edit_fails,
                    LIVE_EDIT_MAX_FAILS);
            if (edit_fails >= LIVE_EDIT_MAX_FAILS) {
                stall_note(dc, e->channel_id, e->cmd,
                           "Discord kept rejecting message edits");
                close_live_message(dc, e->channel_id, e->msg_id);
                cleanup_files(e->vm, e->out_f, e->code_f, e->in_f);
                break;
            }
        }
        first = false;
    }
    remove_if_current(lm, e);
    entry_free(e);
    return NULL;
}

/* teardown of a replaced session (runs on a helper thread) */
typedef struct {
    discord_client_t *dc;
    live_map_t *lm;
    live_entry_t *old;
    char *vm;
} teardown_t;

static void *teardown_thread(void *arg) {
    teardown_t *t = arg;
    if (t->old->has_pid)
        vm_guest_kill_tree(t->vm, t->old->pid);
    cleanup_files(t->vm, t->old->out_f, t->old->code_f, t->old->in_f);
    close_live_message(t->dc, t->old->channel_id, t->old->msg_id);
    /* the old run thread frees the entry when it observes dead */
    free(t->vm);
    free(t);
    return NULL;
}

void live_begin_run(discord_client_t *dc, live_map_t *lm, uint64_t channel_id,
                    uint64_t ack_msg_id, uint64_t author_id,
                    const char *author_name, const char *vm, const char *cmd,
                    const char *runas, bool scrub_ip) {
    if (runas && *runas && !bot_valid_runas(runas)) {
        char buf[512];
        bot_plain_tail("linked linux account is invalid; ask the owner to "
                       "re-add you.",
                       buf, sizeof buf);
        discord_send_message(dc, channel_id, buf, NULL, 0, NULL);
        return;
    }
    const char *ru = (runas && *runas) ? runas : "";
    char rand[24];
    {
        unsigned int r = (unsigned int)time(NULL) ^ (unsigned int)getpid() ^
                        (unsigned int)ack_msg_id;
        r ^= r << 13;
        r ^= r >> 17;
        r ^= r << 5;
        snprintf(rand, sizeof rand, "%08x", r);
    }
    char out_f[128], code_f[128], in_f[128];
    snprintf(out_f, sizeof out_f, "/tmp/podbot-live-%llu-%s.out",
             (unsigned long long)(ack_msg_id & 0xffffff), rand);
    snprintf(code_f, sizeof code_f, "/tmp/podbot-live-%llu-%s.code",
             (unsigned long long)(ack_msg_id & 0xffffff), rand);
    snprintf(in_f, sizeof in_f, "/tmp/podbot-live-%llu-%s.in",
             (unsigned long long)(ack_msg_id & 0xffffff), rand);
    char *mk = live_mkfifo_script(in_f, ru);
    char *has_fifo = NULL;
    if (mk) {
        char *args[] = { "-c", mk, NULL };
        long long code = 0;
        if (vm_guest_exec(vm, "/bin/bash", args, false, 10, &code, NULL,
                          NULL) == 0 &&
            code == 0)
            has_fifo = in_f;
        free(mk);
    }

    live_entry_t *e = xmalloc(sizeof *e);
    if (!e)
        return;
    memset(e, 0, sizeof *e);
    e->tag = ack_msg_id;
    e->channel_id = channel_id;
    e->author_id = author_id;
    e->msg_id = ack_msg_id;
    e->vm = xstrdup(vm);
    e->cmd = xstrdup(cmd);
    e->runas = xstrdup(ru);
    e->out_f = xstrdup(out_f);
    e->code_f = xstrdup(code_f);
    e->in_f = has_fifo ? xstrdup(in_f) : NULL;
    e->scrub_ip = scrub_ip;
    e->dc = dc;
    e->map = lm;
    if (!e->vm || !e->cmd || !e->runas || !e->out_f || !e->code_f ||
        (has_fifo && !e->in_f)) {
        entry_free(e);
        return;
    }
    live_entry_t *old = NULL;
    pthread_mutex_lock(&lm->mu);
    /* grow first so the failure path stays trivial */
    if (lm->n == lm->cap) {
        size_t nc = lm->cap ? lm->cap * 2 : 8;
        live_entry_t **ne = xrealloc(lm->entries, nc * sizeof *ne);
        if (!ne) {
            pthread_mutex_unlock(&lm->mu);
            entry_free(e);
            return;
        }
        lm->entries = ne;
        lm->cap = nc;
    }
    for (size_t i = 0; i < lm->n; i++) {
        live_entry_t *o = lm->entries[i];
        if (o->channel_id == channel_id && o->author_id == author_id) {
            o->dead = true;
            old = o;
            memmove(lm->entries + i, lm->entries + i + 1,
                    (lm->n - i - 1) * sizeof *lm->entries);
            lm->n--;
            break;
        }
    }
    lm->entries[lm->n++] = e;
    pthread_mutex_unlock(&lm->mu);

    if (old) {
        teardown_t *t = xmalloc(sizeof *t);
        if (t) {
            t->dc = dc;
            t->lm = lm;
            t->old = old;
            t->vm = xstrdup(vm);
            if (t->vm) {
                pthread_t th;
                pthread_attr_t at;
                pthread_attr_init(&at);
                pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
                if (pthread_create(&th, &at, teardown_thread, t) == 0) {
                    pthread_attr_destroy(&at);
                    t = NULL;
                } else {
                    pthread_attr_destroy(&at);
                }
            }
            if (t) {
                /* fallback: synchronous teardown */
                if (t->old->has_pid)
                    vm_guest_kill_tree(vm, t->old->pid);
                cleanup_files(vm, t->old->out_f, t->old->code_f, t->old->in_f);
                close_live_message(dc, t->old->channel_id, t->old->msg_id);
                entry_free(t->old);
                free(t->vm);
                free(t);
            }
        } else {
            entry_free(old);
        }
    }
    fprintf(stderr, "run started for %s (id %llu)\n", author_name,
            (unsigned long long)author_id);
    pthread_t th;
    pthread_attr_t at;
    pthread_attr_init(&at);
    pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&th, &at, run_thread, e) != 0) {
        remove_if_current(lm, e);
        entry_free(e);
    }
    pthread_attr_destroy(&at);
}
