/* Discord gateway (WebSocket) over libcurl's WS API. JSON, no compression. */
#include "discord.h"
#include "discord_internal.h"
#include "util.h"

#include <curl/curl.h>
#include <jansson.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

typedef struct {
    CURL *ws;
    long long seq; /* -1 until first dispatch */
    char session_id[160];
    char resume_url[512];
    int hb_interval_ms;
    long long last_hb_ms;
    bool acked;
    bool hello_ok;
} gw_t;

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000LL + ts.tv_nsec / 1000000LL;
}

/* Send one TEXT frame. Returns 0 on success. */
static int ws_send(CURL *ws, const char *s) {
    size_t sent = 0;
    size_t left = strlen(s);
    while (left) {
        CURLcode rc = curl_ws_send(ws, s + (strlen(s) - left), left, &sent, 0,
                                   CURLWS_TEXT);
        if (rc != CURLE_OK) {
            fprintf(stderr, "artixy: gateway send failed: %s\n",
                    curl_easy_strerror(rc));
            return -1;
        }
        left -= sent;
        if (sent == 0)
            break;
    }
    return 0;
}

/*
 * Receive one complete TEXT message (reassembling fragments).
 * timeout_ms bounds the whole call. Returns 0 with *out malloc'd,
 * 1 on timeout (no data), -1 on error/close (close code in *close_code).
 */
static int ws_recv(CURL *ws, char **out, long timeout_ms, int *close_code) {
    *out = NULL;
    *close_code = 0;
    char buf[16384];
    size_t total = 0, cap = 0;
    char *msg = NULL;
    long long deadline = now_ms() + timeout_ms;
    for (;;) {
        long long left = deadline - now_ms();
        if (left <= 0) {
            free(msg);
            return 1;
        }
        size_t rlen = 0;
        const struct curl_ws_frame *meta = NULL;
        CURLcode rc = curl_ws_recv(ws, buf, sizeof buf, &rlen, &meta);
        if (rc != CURLE_OK) {
            if (rc == CURLE_AGAIN) {
                struct timespec ts = { 0, 20 * 1000000L };
                nanosleep(&ts, NULL);
                continue;
            }
            free(msg);
            return -1;
        }
        if (!meta)
            continue;
        if (meta->flags & CURLWS_PING) {
            size_t sent = 0;
            curl_ws_send(ws, buf, rlen, &sent, 0, CURLWS_PONG);
            continue;
        }
        if (meta->flags & CURLWS_CLOSE) {
            int code = 0;
            if (rlen >= 2)
                code = ((unsigned char)buf[0] << 8) | (unsigned char)buf[1];
            *close_code = code;
            free(msg);
            return -1;
        }
        if (!(meta->flags & (CURLWS_TEXT | CURLWS_BINARY)))
            continue;
        size_t off = (size_t)meta->offset;
        if (off == 0 && !(meta->flags & CURLWS_CONT)) {
            /* new message */
            free(msg);
            msg = NULL;
            total = 0;
            cap = 0;
        }
        if (total + rlen + 1 > cap) {
            size_t ncap = (total + rlen + 1) * 2;
            char *nm = xrealloc(msg, ncap);
            if (!nm) {
                free(msg);
                return -1;
            }
            msg = nm;
            cap = ncap;
        }
        memcpy(msg + total, buf, rlen);
        total += rlen;
        if (meta->bytesleft == 0) {
            msg[total] = '\0';
            *out = msg;
            return 0;
        }
    }
}

static int ws_connect(const char *url, CURL **out) {
    CURL *h = curl_easy_init();
    if (!h)
        return -1;
    curl_easy_setopt(h, CURLOPT_URL, url);
    curl_easy_setopt(h, CURLOPT_USERAGENT, "DiscordBot (artixy-c, 0.3.0)");
    curl_easy_setopt(h, CURLOPT_CONNECT_ONLY, 2L);
    curl_easy_setopt(h, CURLOPT_TIMEOUT, 30L);
    CURLcode rc = curl_easy_perform(h);
    if (rc != CURLE_OK) {
        fprintf(stderr, "artixy: gateway connect failed: %s\n",
                curl_easy_strerror(rc));
        curl_easy_cleanup(h);
        return -1;
    }
    *out = h;
    return 0;
}

static int send_identify(CURL *ws, const char *token) {
    json_t *o = json_pack(
        "{s:i, s:{s:s, s:i, s:{s:s, s:s, s:s}}}",
        "op", 2, "d", "token", token, "intents", (int)DISCORD_INTENTS,
        "properties", "os", "linux", "browser", "artixy-c", "device",
        "artixy-c");
    char *s = json_dumps(o, JSON_COMPACT);
    json_decref(o);
    int rc = ws_send(ws, s);
    free(s);
    return rc;
}

static int send_resume(CURL *ws, const char *token, const char *session,
                       long long seq) {
    json_t *o = json_pack("{s:i, s:{s:s, s:s, s:I}}", "op", 6, "d", "token",
                          token, "session_id", session, "seq",
                          (json_int_t)seq);
    char *s = json_dumps(o, JSON_COMPACT);
    json_decref(o);
    int rc = ws_send(ws, s);
    free(s);
    return rc;
}

static int send_heartbeat(CURL *ws, long long seq) {
    char buf[64];
    if (seq < 0)
        snprintf(buf, sizeof buf, "{\"op\":1,\"d\":null}");
    else
        snprintf(buf, sizeof buf, "{\"op\":1,\"d\":%lld}", seq);
    return ws_send(ws, buf);
}

/* Fetch GET /gateway/bot. Returns 0 with *url_out malloc'd. */
typedef struct {
    char *p;
    size_t n, cap;
} gw_cap_t;

static size_t gw_write_cb(char *ptr, size_t sz, size_t nm, void *ud) {
    gw_cap_t *c2 = ud;
    size_t n = sz * nm;
    if (c2->n + n + 1 > c2->cap) {
        size_t nc = (c2->n + n + 1) * 2;
        char *np = xrealloc(c2->p, nc);
        if (!np)
            return 0;
        c2->p = np;
        c2->cap = nc;
    }
    memcpy(c2->p + c2->n, ptr, n);
    c2->n += n;
    c2->p[c2->n] = '\0';
    return n;
}

static int fetch_gateway(discord_client_t *c, char **url_out) {
    *url_out = NULL;
    char url[256];
    snprintf(url, sizeof url, "%s/gateway/bot", DISCORD_API);
    CURL *h = curl_easy_init();
    if (!h)
        return -1;
    char auth[512];
    snprintf(auth, sizeof auth, "Authorization: Bot %s", discord_token(c));
    struct curl_slist *hdrs = curl_slist_append(NULL, auth);
    gw_cap_t cc = { NULL, 0, 0 };
    curl_easy_setopt(h, CURLOPT_URL, url);
    curl_easy_setopt(h, CURLOPT_HTTPHEADER, hdrs);
    curl_easy_setopt(h, CURLOPT_USERAGENT, "DiscordBot (artixy-c, 0.3.0)");
    curl_easy_setopt(h, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, gw_write_cb);
    curl_easy_setopt(h, CURLOPT_WRITEDATA, &cc);
    CURLcode rc = curl_easy_perform(h);
    long code = 0;
    if (rc == CURLE_OK)
        curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, &code);
    curl_slist_free_all(hdrs);
    curl_easy_cleanup(h);
    if (rc != CURLE_OK || code == 401) {
        fprintf(stderr, "artixy: gateway/bot failed (%s, HTTP %ld) — bad token?\n",
                curl_easy_strerror(rc), code);
        free(cc.p);
        return code == 401 ? -2 : -1;
    }
    if (code < 200 || code >= 300 || !cc.p) {
        free(cc.p);
        return -1;
    }
    json_error_t e;
    json_t *r = json_loads(cc.p, 0, &e);
    free(cc.p);
    if (!r) {
        fprintf(stderr, "artixy: gateway/bot JSON error: %s\n", e.text);
        return -1;
    }
    json_t *u = json_object_get(r, "url");
    const char *us = json_is_string(u) ? json_string_value(u) : NULL;
    if (!us) {
        json_decref(r);
        return -1;
    }
    *url_out = xstrdup(us);
    json_t *sess = json_object_get(r, "session_start_limit");
    if (json_is_object(sess)) {
        json_t *rem = json_object_get(sess, "remaining");
        if (json_is_integer(rem))
            fprintf(stderr, "artixy: gateway session starts remaining: %lld\n",
                    (long long)json_integer_value(rem));
    }
    json_decref(r);
    return *url_out ? 0 : -1;
}

/* outcome of one connection */
typedef enum { CONN_OK, CONN_RESUME, CONN_FATAL } conn_result_t;

static conn_result_t serve(discord_client_t *c, const char *base_url, gw_t *g,
                           bool *want_resume) {
    char url[768];
    const char *use = *want_resume && g->resume_url[0] ? g->resume_url : base_url;
    snprintf(url, sizeof url, "%s?v=10&encoding=json", use);
    CURL *ws = NULL;
    if (ws_connect(url, &ws) != 0)
        return CONN_RESUME;
    g->ws = ws;
    g->hello_ok = false;
    g->acked = true;

    /* first frame must be HELLO */
    char *msg = NULL;
    int code = 0;
    if (ws_recv(ws, &msg, 30000, &code) != 0) {
        fprintf(stderr, "artixy: no HELLO from gateway\n");
        curl_easy_cleanup(ws);
        return CONN_RESUME;
    }
    json_error_t e;
    json_t *hello = json_loads(msg, 0, &e);
    free(msg);
    if (!hello)
        return CONN_RESUME;
    json_t *hop = json_object_get(hello, "op");
    json_t *hd = json_object_get(hello, "d");
    int interval = 0;
    if (json_is_integer(hop) && json_integer_value(hop) == 10 && json_is_object(hd)) {
        json_t *iv = json_object_get(hd, "heartbeat_interval");
        if (json_is_integer(iv))
            interval = (int)json_integer_value(iv);
    }
    json_decref(hello);
    if (interval <= 0) {
        curl_easy_cleanup(ws);
        return CONN_RESUME;
    }
    g->hb_interval_ms = interval;
    g->hello_ok = true;
    g->last_hb_ms = now_ms() - interval; /* heartbeat immediately */
    /* first heartbeat with jitter (Discord requirement) */
    {
        unsigned int r = (unsigned int)(now_ms() & 0x7fffffff);
        r = r * 1103515245u + 12345u;
        long jitter = (long)((r >> 8) % (unsigned)interval);
        struct timespec ts = { jitter / 1000, (jitter % 1000) * 1000000L };
        nanosleep(&ts, NULL);
    }

    bool resumed_session = *want_resume && g->session_id[0] && g->seq >= 0;
    int rc;
    if (resumed_session)
        rc = send_resume(ws, discord_token(c), g->session_id, g->seq);
    else
        rc = send_identify(ws, discord_token(c));
    if (rc != 0) {
        curl_easy_cleanup(ws);
        return CONN_RESUME;
    }
    g->last_hb_ms = now_ms();
    if (send_heartbeat(ws, g->seq) != 0) {
        curl_easy_cleanup(ws);
        return CONN_RESUME;
    }

    bool result_resume = true;
    conn_result_t out = CONN_RESUME;
    int missed = 0;

    for (;;) {
        if (!discord_is_running(c)) {
            out = CONN_OK;
            break;
        }
        /* heartbeat due? */
        if (now_ms() - g->last_hb_ms >= g->hb_interval_ms) {
            if (!g->acked)
                missed++;
            else
                missed = 0;
            if (missed >= 2) {
                fprintf(stderr, "artixy: gateway heartbeat ACKs missed, reconnecting\n");
                break; /* resume */
            }
            g->acked = false;
            if (send_heartbeat(ws, g->seq) != 0)
                break;
            g->last_hb_ms = now_ms();
        }
        char *frame = NULL;
        int r = ws_recv(ws, &frame, 1000, &code);
        if (r == 1)
            continue; /* timeout: loop back for heartbeat/stop checks */
        if (r != 0) {
            if (code >= 4000) {
                fprintf(stderr, "artixy: gateway close %d\n", code);
                if (code == 4004 || code == 4010 || code == 4011 ||
                    code == 4012 || code == 4013 || code == 4014) {
                    fprintf(stderr, "artixy: fatal gateway close %d\n", code);
                    curl_easy_cleanup(ws);
                    return CONN_FATAL;
                }
                if (code == 4009) {
                    /* session timeout: fresh identify */
                    g->session_id[0] = '\0';
                    g->seq = -1;
                }
            }
            break;
        }
        json_t *m = json_loads(frame, 0, &e);
        free(frame);
        if (!m)
            continue;
        json_t *opj = json_object_get(m, "op");
        int op = json_is_integer(opj) ? (int)json_integer_value(opj) : -1;
        if (op == 1) {
            g->acked = false;
            send_heartbeat(ws, g->seq);
            g->last_hb_ms = now_ms();
        } else if (op == 7) {
            json_decref(m); /* RECONNECT */
            break;
        } else if (op == 9) {
            json_t *d = json_object_get(m, "d");
            bool resumable = json_is_true(d);
            json_decref(m);
            if (!resumable) {
                g->session_id[0] = '\0';
                g->seq = -1;
            }
            break;
        } else if (op == 11) {
            g->acked = true;
        } else if (op == 0) {
            json_t *sj = json_object_get(m, "s");
            if (json_is_integer(sj))
                g->seq = (long long)json_integer_value(sj);
            json_t *tj = json_object_get(m, "t");
            const char *t = json_is_string(tj) ? json_string_value(tj) : "";
            if (strcmp(t, "READY") != 0 && strcmp(t, "RESUMED") != 0)
                fprintf(stderr, "artixy: dispatch t=%s\n", t);
            json_t *d = json_object_get(m, "d");
            if (strcmp(t, "READY") == 0 && json_is_object(d)) {
                json_t *sid = json_object_get(d, "session_id");
                if (json_is_string(sid)) {
                    snprintf(g->session_id, sizeof g->session_id, "%s",
                             json_string_value(sid));
                }
                json_t *ru = json_object_get(d, "resume_gateway_url");
                if (json_is_string(ru)) {
                    snprintf(g->resume_url, sizeof g->resume_url, "%s",
                             json_string_value(ru));
                }
                uint64_t uid = 0, app = 0;
                json_t *u = json_object_get(d, "user");
                if (json_is_object(u)) {
                    json_t *id = json_object_get(u, "id");
                    if (json_is_string(id))
                        uid = (uint64_t)strtoull(json_string_value(id), NULL, 10);
                }
                json_t *ap = json_object_get(d, "application");
                if (json_is_object(ap)) {
                    json_t *id = json_object_get(ap, "id");
                    if (json_is_string(id))
                        app = (uint64_t)strtoull(json_string_value(id), NULL, 10);
                }
                discord_set_ids(c, uid, app);
                discord_emit_ready(c);
                result_resume = true;
            } else if (strcmp(t, "RESUMED") == 0) {
                result_resume = true;
            } else if (strcmp(t, "MESSAGE_CREATE") == 0 && json_is_object(d)) {
                disc_message_t dm;
                if (disc_parse_message(d, &dm) == 0) {
                    discord_emit_message(c, &dm);
                    disc_message_free(&dm);
                }
            } else if (strcmp(t, "INTERACTION_CREATE") == 0 && json_is_object(d)) {
                disc_interaction_t in;
                if (disc_parse_interaction(d, &in) == 0) {
                    discord_emit_interaction(c, &in);
                    disc_interaction_free(&in);
                }
            }
        }
        json_decref(m);
    }
    curl_easy_cleanup(ws);
    g->ws = NULL;
    *want_resume = result_resume;
    return out;
}

int discord_run(discord_client_t *c) {
    char *base = NULL;
    int frc = fetch_gateway(c, &base);
    if (frc == -2)
        return -1; /* bad token */
    if (frc != 0)
        return -1;
    gw_t g;
    memset(&g, 0, sizeof g);
    g.seq = -1;
    bool want_resume = false;
    long backoff_ms = 1000;
    int rc = 0;
    while (discord_is_running(c)) {
        conn_result_t r = serve(c, base, &g, &want_resume);
        if (r == CONN_FATAL) {
            rc = -1;
            break;
        }
        if (r == CONN_OK)
            break;
        if (!discord_is_running(c))
            break;
        /* reconnect with capped exponential backoff */
        struct timespec ts = { backoff_ms / 1000,
                               (backoff_ms % 1000) * 1000000L };
        nanosleep(&ts, NULL);
        if (backoff_ms < 30000)
            backoff_ms *= 2;
    }
    free(base);
    return rc;
}

void discord_stop(discord_client_t *c) {
    c->running = false;
}
