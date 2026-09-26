/* Shared bot state, invocation context, validators, formatting. */
#include "bot.h"
#include "util.h"

#include <ctype.h>
#include <curl/curl.h>
#include <fcntl.h>
#include <jansson.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

/* ------------------------------------------------------------------ */
/* state                                                                */
/* ------------------------------------------------------------------ */

static char *bot_config_serialize(const file_config_t *c);

static uint64_t *dup_u64(const uint64_t *v, size_t n) {
    if (!n)
        return NULL;
    uint64_t *c = xmalloc(n * sizeof *c);
    if (c)
        memcpy(c, v, n * sizeof *c);
    return c;
}

int bot_state_init(bot_state_t *st, const file_config_t *cfg) {
    memset(st, 0, sizeof *st);
    if (pthread_rwlock_init(&st->mu, NULL) != 0)
        return -1;
    strmap_init(&st->linux);
    strmap_init(&st->shells);
    bot_state_apply(st, cfg);
    return 0;
}

void bot_state_free(bot_state_t *st) {
    free(st->users);
    free(st->blocked);
    free(st->admins);
    strmap_free(&st->linux);
    free(st->ai_prompt);
    strmap_free(&st->shells);
    pthread_rwlock_destroy(&st->mu);
    memset(st, 0, sizeof *st);
}

void bot_state_apply(bot_state_t *st, const file_config_t *cfg) {
    pthread_rwlock_wrlock(&st->mu);
    if (cfg->has_owner_id && cfg->owner_id != 0)
        st->owner = cfg->owner_id;
    free(st->users);
    st->users = dup_u64(cfg->managers, cfg->n_managers);
    st->n_users = cfg->n_managers;
    strmap_free(&st->linux);
    strmap_init(&st->linux);
    for (size_t i = 0; i < cfg->linux.len; i++)
        strmap_set(&st->linux, cfg->linux.keys[i], cfg->linux.vals[i]);
    free(st->blocked);
    st->blocked = dup_u64(cfg->blocked_ids, cfg->n_blocked);
    st->n_blocked = cfg->n_blocked;
    free(st->admins);
    st->admins = dup_u64(cfg->admin_ids, cfg->n_admins);
    st->n_admins = cfg->n_admins;
    snprintf(st->vm, sizeof st->vm, "%s", cfg->vm_name ? cfg->vm_name : "");
    st->has_notify = cfg->has_notify_channel;
    st->notify_channel = cfg->notify_channel;
    st->war_mode = cfg->war_mode;
    st->sayas_enabled = cfg->sayas_enabled;
    st->ai_enabled = cfg->ai_enabled;
    snprintf(st->ai_model, sizeof st->ai_model, "%s",
             cfg->ai_model ? cfg->ai_model : config_default_model());
    snprintf(st->ollama_host, sizeof st->ollama_host, "%s",
             cfg->ollama_host ? cfg->ollama_host : config_default_host());
    free(st->ai_prompt);
    st->ai_prompt = xstrdup(cfg->ai_prompt ? cfg->ai_prompt : "");
    st->ai_temperature = config_clamp_temperature(cfg->ai_temperature);
    st->ai_think = cfg->ai_think;
    strmap_free(&st->shells);
    strmap_init(&st->shells);
    for (size_t i = 0; i < cfg->shells.len; i++)
        strmap_set(&st->shells, cfg->shells.keys[i], cfg->shells.vals[i]);
    pthread_rwlock_unlock(&st->mu);
}

static bool contains_u64(const uint64_t *v, size_t n, uint64_t id) {
    for (size_t i = 0; i < n; i++) {
        if (v[i] == id)
            return true;
    }
    return false;
}

bool bot_is_blocked(bot_state_t *st, uint64_t id) {
    pthread_rwlock_rdlock(&st->mu);
    bool b = contains_u64(st->blocked, st->n_blocked, id);
    pthread_rwlock_unlock(&st->mu);
    return b;
}

bool bot_is_authed(bot_state_t *st, uint64_t id) {
    pthread_rwlock_rdlock(&st->mu);
    bool ok = !contains_u64(st->blocked, st->n_blocked, id) &&
              (id == st->owner || contains_u64(st->users, st->n_users, id));
    pthread_rwlock_unlock(&st->mu);
    return ok;
}

bool bot_is_owner(bot_state_t *st, uint64_t id) {
    pthread_rwlock_rdlock(&st->mu);
    bool ok = !contains_u64(st->blocked, st->n_blocked, id) && id == st->owner;
    pthread_rwlock_unlock(&st->mu);
    return ok;
}

bool bot_is_elevated(bot_state_t *st, uint64_t id) {
    pthread_rwlock_rdlock(&st->mu);
    bool ok = !contains_u64(st->blocked, st->n_blocked, id) &&
              (id == st->owner || contains_u64(st->admins, st->n_admins, id));
    pthread_rwlock_unlock(&st->mu);
    return ok;
}

/* Scratch for bot_linked_user: pointer valid until next state write;
 * callers needing it longer must copy. */
static _Thread_local char linked_buf[64];

const char *bot_linked_user(bot_state_t *st, uint64_t id) {
    /* id string on the stack: ids are <= 20 digits */
    char key[32];
    snprintf(key, sizeof key, "%llu", (unsigned long long)id);
    pthread_rwlock_rdlock(&st->mu);
    const char *v = strmap_get(&st->linux, key);
    if (v) {
        snprintf(linked_buf, sizeof linked_buf, "%s", v);
        v = linked_buf;
    } else {
        v = "";
    }
    pthread_rwlock_unlock(&st->mu);
    return v;
}

/* ------------------------------------------------------------------ */
/* persist + watcher                                                    */
/* ------------------------------------------------------------------ */

int bot_persist(bot_state_t *st) {
    /* refuse to clobber a file with a parse error */
    struct stat sbuf;
    if (stat(config_path(), &sbuf) == 0) {
        file_config_t probe;
        int rc = config_load(&probe);
        file_config_free(&probe);
        if (rc == -2)
            return -1;
    }
    file_config_t cur;
    file_config_init(&cur);
    {
        file_config_t tmp;
        if (config_load(&tmp) == 0) {
            /* keep file's owner/token/vm; overlay live state below */
            cur.has_owner_id = tmp.has_owner_id;
            cur.owner_id = tmp.owner_id;
            free(cur.discord_token);
            cur.discord_token = tmp.discord_token;
            tmp.discord_token = NULL;
            free(cur.vm_name);
            cur.vm_name = tmp.vm_name;
            tmp.vm_name = NULL;
            file_config_free(&tmp);
        }
    }
    pthread_rwlock_rdlock(&st->mu);
    cur.managers = dup_u64(st->users, st->n_users);
    cur.n_managers = st->n_users;
    for (size_t i = 0; i < st->linux.len; i++)
        strmap_set(&cur.linux, st->linux.keys[i], st->linux.vals[i]);
    cur.admin_ids = dup_u64(st->admins, st->n_admins);
    cur.n_admins = st->n_admins;
    cur.has_notify_channel = st->has_notify;
    cur.notify_channel = st->notify_channel;
    cur.war_mode = st->war_mode;
    cur.sayas_enabled = st->sayas_enabled;
    cur.ai_enabled = st->ai_enabled;
    free(cur.ai_model);
    cur.ai_model = xstrdup(st->ai_model);
    free(cur.ollama_host);
    cur.ollama_host = xstrdup(st->ollama_host);
    free(cur.ai_prompt);
    cur.ai_prompt = xstrdup(st->ai_prompt ? st->ai_prompt : "");
    cur.ai_temperature = st->ai_temperature;
    cur.ai_think = st->ai_think;
    for (size_t i = 0; i < st->shells.len; i++)
        strmap_set(&cur.shells, st->shells.keys[i], st->shells.vals[i]);
    pthread_rwlock_unlock(&st->mu);

    /* serialize as JSON (jansson) with the persist header comment */
    char *body = bot_config_serialize(&cur);
    file_config_free(&cur);
    if (!body)
        return -1;
    const char *header = "// artixy config — edits hot-apply within seconds, no restart needed.\n"
                         "// This is JSONC: plain JSON plus // and /* */ comments and trailing commas.\n";
    size_t total = strlen(header) + strlen(body) + 2;
    char *full = xmalloc(total);
    if (!full) {
        free(body);
        return -1;
    }
    snprintf(full, total, "%s%s\n", header, body);
    free(body);
    /* atomic 0600 write */
    char tmpath[4096];
    snprintf(tmpath, sizeof tmpath, "%s.tmp-XXXXXX", config_path());
    /* mkstemp needs a mutable template without extra suffix */
    int fd = mkstemp(tmpath);
    if (fd < 0) {
        free(full);
        return -1;
    }
    fchmod(fd, 0600);
    size_t wlen = strlen(full);
    ssize_t w = write(fd, full, wlen);
    free(full);
    if (w < 0 || (size_t)w != wlen) {
        close(fd);
        unlink(tmpath);
        return -1;
    }
    fsync(fd);
    close(fd);
    if (rename(tmpath, config_path()) != 0) {
        unlink(tmpath);
        return -1;
    }
    return 0;
}

/* Serialize live config for persist (jansson, pretty). */
static char *bot_config_serialize(const file_config_t *c) {
    json_t *o = json_object();
    if (!o)
        return NULL;
    if (c->has_owner_id)
        json_object_set_new(o, "owner_id", json_integer((json_int_t)c->owner_id));
    else
        json_object_set_new(o, "owner_id", json_integer(0));
    json_t *b = json_array();
    for (size_t i = 0; i < c->n_blocked; i++)
        json_array_append_new(b, json_integer((json_int_t)c->blocked_ids[i]));
    json_object_set_new(o, "blocked_ids", b);
    if (c->discord_token)
        json_object_set_new(o, "discord_token", json_string(c->discord_token));
    else
        json_object_set_new(o, "discord_token", json_string(""));
    if (c->vm_name)
        json_object_set_new(o, "vm_name", json_string(c->vm_name));
    else
        json_object_set_new(o, "vm_name", json_string(""));
    json_object_set_new(o, "war_mode", json_boolean(c->war_mode));
    json_object_set_new(o, "sayas_enabled", json_boolean(c->sayas_enabled));
    if (c->has_notify_channel)
        json_object_set_new(o, "notify_channel",
                            json_integer((json_int_t)c->notify_channel));
    else
        json_object_set_new(o, "notify_channel", json_null());
    json_t *m = json_array();
    for (size_t i = 0; i < c->n_managers; i++)
        json_array_append_new(m, json_integer((json_int_t)c->managers[i]));
    json_object_set_new(o, "managers", m);
    json_t *a = json_array();
    for (size_t i = 0; i < c->n_admins; i++)
        json_array_append_new(a, json_integer((json_int_t)c->admin_ids[i]));
    json_object_set_new(o, "admin_ids", a);
    json_t *lx = json_object();
    for (size_t i = 0; i < c->linux.len; i++)
        json_object_set_new(lx, c->linux.keys[i], json_string(c->linux.vals[i]));
    json_object_set_new(o, "linux", lx);
    json_t *sh = json_object();
    for (size_t i = 0; i < c->shells.len; i++)
        json_object_set_new(sh, c->shells.keys[i], json_string(c->shells.vals[i]));
    json_object_set_new(o, "shells", sh);
    json_object_set_new(o, "ai_enabled", json_boolean(c->ai_enabled));
    json_object_set_new(o, "ai_model",
                        json_string(c->ai_model ? c->ai_model : "llama3.1"));
    json_object_set_new(o, "ollama_host",
                        json_string(c->ollama_host ? c->ollama_host
                                                   : "http://127.0.0.1:11434"));
    json_object_set_new(o, "ai_prompt",
                        json_string(c->ai_prompt ? c->ai_prompt : ""));
    json_object_set_new(o, "ai_temperature", json_real(c->ai_temperature));
    json_object_set_new(o, "ai_think", json_boolean(c->ai_think));
    char *s = json_dumps(o, JSON_INDENT(2) | JSON_PRESERVE_ORDER);
    json_decref(o);
    return s;
}

static time_t file_mtime(const char *path, bool *ok) {
    struct stat st;
    if (stat(path, &st) != 0) {
        *ok = false;
        return 0;
    }
    *ok = true;
    return st.st_mtime;
}

static void *watch_thread(void *arg) {
    bot_state_t *st = arg;
    bool has_last = false;
    time_t last = 0;
    for (;;) {
        sleep(2);
        bool ok = false;
        time_t cur = file_mtime(config_path(), &ok);
        if (ok == has_last && (!ok || cur == last))
            continue;
        struct timespec half = { 0, 500 * 1000000L };
        nanosleep(&half, NULL);
        cur = file_mtime(config_path(), &ok);
        has_last = ok;
        last = cur;
        if (!ok) {
            fprintf(stderr, "config: file missing, recreating template\n");
            config_ensure_template();
            has_last = false;
            continue;
        }
        file_config_t cfg;
        if (config_load(&cfg) != 0)
            continue; /* parse error already reported; keep settings */
        bot_state_apply(st, &cfg);
        file_config_free(&cfg);
        fprintf(stderr, "config: hot-applied\n");
    }
    return NULL;
}

int bot_state_watch(bot_state_t *st) {
    pthread_t th;
    pthread_attr_t at;
    pthread_attr_init(&at);
    pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
    int rc = pthread_create(&th, &at, watch_thread, st);
    pthread_attr_destroy(&at);
    return rc;
}

/* ------------------------------------------------------------------ */
/* parsing helpers                                                      */
/* ------------------------------------------------------------------ */

uint64_t parse_u64(const char *s) {
    if (!s)
        return 0;
    while (*s == ' ' || *s == '\t')
        s++;
    unsigned long long v = 0;
    if (sscanf(s, "%llu", &v) != 1)
        return 0;
    return (uint64_t)v;
}

uint64_t parse_target_id(const char *s) {
    if (!s)
        return 0;
    while (*s == ' ' || *s == '\t')
        s++;
    char buf[64];
    size_t n = 0;
    if (s[0] == '<' && s[1] == '@') {
        s += 2;
        if (*s == '!')
            s++;
        while (*s && *s != '>' && n + 1 < sizeof buf)
            buf[n++] = *s++;
        if (*s != '>')
            return 0;
        buf[n] = '\0';
    } else {
        while (*s && *s != ' ' && *s != '\t' && n + 1 < sizeof buf)
            buf[n++] = *s++;
        buf[n] = '\0';
    }
    /* digits only */
    if (!buf[0])
        return 0;
    for (size_t i = 0; buf[i]; i++) {
        if (buf[i] < '0' || buf[i] > '9')
            return 0;
    }
    uint64_t v = parse_u64(buf);
    return v ? v : 0;
}

bool parse_message_ref(const char *s, uint64_t current_channel,
                       uint64_t *ch_out, uint64_t *msg_out) {
    if (!s)
        return false;
    while (*s == ' ' || *s == '\t' || *s == '<' || *s == '>')
        s++;
    /* cut query string */
    const char *q = strchr(s, '?');
    size_t len = q ? (size_t)(q - s) : strlen(s);
    while (len > 0 && (s[len - 1] == ' ' || s[len - 1] == '\t' ||
                       s[len - 1] == '<' || s[len - 1] == '>'))
        len--;
    char tmp[512];
    if (len >= sizeof tmp)
        return false;
    memcpy(tmp, s, len);
    tmp[len] = '\0';
    const char *marker = "/channels/";
    char *at = strstr(tmp, marker);
    if (at) {
        unsigned long long ch = 0, msg = 0;
        char extra = 0;
        /* guild/channel/message with optional trailing junk rejected */
        if (sscanf(at + strlen(marker), "%*[^/]/%llu/%llu%c", &ch, &msg,
                   &extra) < 2)
            return false;
        if (extra || !ch || !msg)
            return false;
        /* ensure exactly 3 parts: count slashes */
        int slashes = 0;
        for (const char *p = at + strlen(marker); *p; p++) {
            if (*p == '/')
                slashes++;
            if (*p == '?' || *p == ' ')
                break;
        }
        if (slashes != 2)
            return false;
        *ch_out = (uint64_t)ch;
        *msg_out = (uint64_t)msg;
        return true;
    }
    for (size_t i = 0; tmp[i]; i++) {
        if (tmp[i] < '0' || tmp[i] > '9')
            return false;
    }
    uint64_t v = parse_u64(tmp);
    if (!v)
        return false;
    *ch_out = current_channel;
    *msg_out = v;
    return true;
}

/* ------------------------------------------------------------------ */
/* context replies                                                      */
/* ------------------------------------------------------------------ */

static void truncate2000(const char *s, char *out) {
    /* copy up to 2000 bytes, backing off to a UTF-8 boundary */
    size_t n = strlen(s);
    if (n <= 2000) {
        memcpy(out, s, n + 1);
        return;
    }
    size_t cut = 2000;
    while (cut > 0 && (s[cut] & 0xc0) == 0x80)
        cut--;
    memcpy(out, s, cut);
    out[cut] = '\0';
}

void ctx_reply(cmd_ctx_t *ctx, const char *text) {
    char buf[2048];
    truncate2000(text, buf);
    if (ctx->is_slash) {
        discord_interaction_followup(ctx->dc, ctx->inter, buf, false, NULL, 0,
                                     NULL);
        return;
    }
    discord_send_message(ctx->dc, ctx->channel_id, buf, NULL, 0, NULL);
}

void ctx_reply_ephemeral(cmd_ctx_t *ctx, const char *text) {
    char buf[2048];
    truncate2000(text, buf);
    if (ctx->is_slash) {
        discord_interaction_followup(ctx->dc, ctx->inter, buf, true, NULL, 0,
                                     NULL);
        return;
    }
    uint64_t dm = 0;
    if (discord_create_dm(ctx->dc, ctx->author_id, &dm) == 0 && dm)
        discord_send_message(ctx->dc, dm, buf, NULL, 0, NULL);
    else
        discord_send_message(ctx->dc, ctx->channel_id, buf, NULL, 0, NULL);
}

void ctx_reply_files(cmd_ctx_t *ctx, const char *text, const disc_file_t *files,
                     size_t n) {
    char buf[2048];
    truncate2000(text ? text : "", buf);
    if (ctx->is_slash) {
        discord_interaction_followup(ctx->dc, ctx->inter, buf, false, files, n,
                                     NULL);
        return;
    }
    discord_send_message(ctx->dc, ctx->channel_id, buf, files, n, NULL);
}

void ctx_deny(cmd_ctx_t *ctx, const char *text) {
    char buf[2048];
    snprintf(buf, sizeof buf, "<@%llu> %s", (unsigned long long)ctx->author_id,
             text);
    if (ctx->is_slash) {
        discord_interaction_followup(ctx->dc, ctx->inter, buf, true, NULL, 0,
                                     NULL);
        return;
    }
    discord_send_message(ctx->dc, ctx->channel_id, buf, NULL, 0, NULL);
}

bool ctx_need_auth(cmd_ctx_t *ctx) {
    if (bot_is_authed(ctx->st, ctx->author_id))
        return true;
    fprintf(stderr, "denied: %s (id %llu)\n", ctx->author_name,
            (unsigned long long)ctx->author_id);
    ctx_deny(ctx, "Not authorized. Ask the owner to run `/user add @you`.");
    return false;
}

bool ctx_need_public(cmd_ctx_t *ctx) {
    if (!bot_is_blocked(ctx->st, ctx->author_id))
        return true;
    fprintf(stderr, "denied (blocked): %s (id %llu)\n", ctx->author_name,
            (unsigned long long)ctx->author_id);
    ctx_deny(ctx, "Not allowed.");
    return false;
}

static _Thread_local char require_vm_buf[256];

const char *ctx_require_vm(cmd_ctx_t *ctx) {
    pthread_rwlock_rdlock(&ctx->st->mu);
    snprintf(require_vm_buf, sizeof require_vm_buf, "%s", ctx->st->vm);
    pthread_rwlock_unlock(&ctx->st->mu);
    if (!require_vm_buf[0]) {
        char msg[512];
        snprintf(msg, sizeof msg,
                 "VM not configured — set `vm_name` in `%s` or `VM_NAME` env "
                 "(config edits hot-apply, no restart needed).",
                 config_path());
        ctx_reply(ctx, msg);
        return "";
    }
    return require_vm_buf;
}

/* ------------------------------------------------------------------ */
/* validators                                                           */
/* ------------------------------------------------------------------ */

bool bot_valid_runas(const char *name) {
    if (!name || !*name || strlen(name) > 32 || strcmp(name, "root") == 0)
        return false;
    if (!((name[0] >= 'a' && name[0] <= 'z') || name[0] == '_'))
        return false;
    for (size_t i = 1; name[i]; i++) {
        char c = name[i];
        if (!((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' ||
              c == '-'))
            return false;
    }
    return true;
}

bool bot_sensitive_send_name(const char *name) {
    if (!name)
        return true;
    char lower[256];
    size_t i = 0;
    for (; name[i] && i + 1 < sizeof lower; i++)
        lower[i] = (char)tolower((unsigned char)name[i]);
    lower[i] = '\0';
    /* trim spaces */
    char *n = lower;
    while (*n == ' ')
        n++;
    size_t nl = strlen(n);
    while (nl > 0 && n[nl - 1] == ' ')
        n[--nl] = '\0';
    if (!*n || *n == '.')
        return true;
    if (strcmp(n, "config.jsonc") == 0 || strcmp(n, ".env") == 0 ||
        strcmp(n, "token") == 0)
        return true;
    if (strstr(n, ".env") || strstr(n, "config.jsonc"))
        return true;
    static const char *suf[] = { ".pem", ".key", ".p12", ".pfx", ".token", NULL };
    for (size_t k = 0; suf[k]; k++) {
        size_t sl = strlen(suf[k]);
        if (nl >= sl && strcmp(n + nl - sl, suf[k]) == 0)
            return true;
    }
    static const char *pre[] = { "id_rsa", "id_ed25519", "id_ecdsa", "id_dsa",
                                 NULL };
    for (size_t k = 0; pre[k]; k++) {
        if (strncmp(n, pre[k], strlen(pre[k])) == 0)
            return true;
    }
    static const char *inf[] = { "secret",     "credential", "private_key",
                                 "token",      "webhook",    NULL };
    for (size_t k = 0; inf[k]; k++) {
        if (strstr(n, inf[k]))
            return true;
    }
    return false;
}

bool bot_share_has_dot(const char *rel) {
    if (!rel)
        return true;
    const char *p = rel;
    while (*p) {
        const char *seg = p;
        while (*p && *p != '/')
            p++;
        if (p > seg && seg[0] == '.')
            return true;
        if (*p == '/')
            p++;
    }
    return false;
}

bool bot_normalize_guest_dir(const char *dir, char *out, size_t n) {
    if (!dir || dir[0] != '/' || !out || n < 2)
        return false;
    if (strlen(dir) > 512)
        return false;
    char stack[64][129];
    size_t depth = 0;
    const char *p = dir;
    while (*p) {
        while (*p == '/')
            p++;
        if (!*p)
            break;
        const char *e = p;
        while (*e && *e != '/')
            e++;
        size_t len = (size_t)(e - p);
        if (len == 1 && p[0] == '.') {
            /* skip */
        } else if (len == 2 && p[0] == '.' && p[1] == '.') {
            if (depth == 0)
                return false;
            depth--;
        } else {
            if (len > 128 || depth >= 64)
                return false;
            memcpy(stack[depth], p, len);
            stack[depth][len] = '\0';
            depth++;
        }
        p = e;
    }
    size_t w = 0;
    if (depth == 0) {
        if (n < 2)
            return false;
        out[0] = '/';
        out[1] = '\0';
        return true;
    }
    out[0] = '\0';
    for (size_t i = 0; i < depth; i++) {
        size_t sl = strlen(stack[i]);
        if (w + 1 + sl + 1 > n)
            return false;
        out[w++] = '/';
        memcpy(out + w, stack[i], sl);
        w += sl;
    }
    out[w] = '\0';
    (void)w;
    return true;
}

bool bot_upload_allowed(const char *normalized, const char *linked) {
    if (!normalized)
        return false;
    if (strcmp(normalized, "/tmp/artixy-uploads") == 0 ||
        strncmp(normalized, "/tmp/artixy-uploads/", 20) == 0)
        return true;
    if (linked && bot_valid_runas(linked)) {
        char home[96];
        snprintf(home, sizeof home, "/home/%s", linked);
        if (strcmp(normalized, home) == 0)
            return true;
        size_t hl = strlen(home);
        if (strncmp(normalized, home, hl) == 0 && normalized[hl] == '/')
            return true;
    }
    return false;
}

void bot_sh_escape(const char *s, char *out, size_t n) {
    /* 'foo' -> 'foo', embedded ' -> '\'': writes 'x'\''y pattern */
    size_t w = 0;
    if (w + 1 < n)
        out[w++] = '\'';
    for (size_t i = 0; s[i] && w + 4 < n; i++) {
        if (s[i] == '\'') {
            memcpy(out + w, "'\\''", 4);
            w += 4;
        } else {
            out[w++] = s[i];
        }
    }
    if (w + 1 < n)
        out[w++] = '\'';
    out[w < n ? w : n - 1] = '\0';
}

void bot_sanitize_discord_name(const char *s, char *out, size_t n) {
    char tmp[64];
    size_t w = 0;
    if (s) {
        for (size_t i = 0; s[i] && w + 1 < sizeof tmp; i++) {
            char c = (char)tolower((unsigned char)s[i]);
            if ((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' ||
                c == '-')
                tmp[w++] = c;
        }
    }
    if (w > 32)
        w = 32;
    tmp[w] = '\0';
    size_t start = 0;
    while (tmp[start] && !(tmp[start] == '_' ||
                           (tmp[start] >= 'a' && tmp[start] <= 'z')))
        start++;
    snprintf(out, n, "%s", tmp + start);
}

char *bot_sudoers_script(const char *user) {
    if (!bot_valid_runas(user))
        return NULL;
    const char *fmt =
        "set -eu; u='%s'; f=\"/etc/sudoers.d/$u\"; "
        "printf '%%s ALL=(ALL) NOPASSWD: ALL\\n' \"$u\" >\"$f.tmp\"; "
        "printf 'Defaults:%%s !requiretty\\n' \"$u\" >>\"$f.tmp\"; "
        "chmod 0440 \"$f.tmp\"; mv -f \"$f.tmp\" \"$f\"; chmod 0440 \"$f\"; "
        "if command -v visudo >/dev/null 2>&1; then visudo -c -f \"$f\" >/dev/null; fi; "
        "if getent group wheel >/dev/null 2>&1; then usermod -aG wheel \"$u\" || true; "
        "elif getent group sudo >/dev/null 2>&1; then usermod -aG sudo \"$u\" || true; fi; "
        "h=\"$(getent passwd \"$u\" | cut -d: -f6)\"; "
        "if [ -n \"$h\" ] && [ -d \"$h\" ]; then chown \"$u\" \"$h\" 2>/dev/null || true; "
        "for d in \"$h/.cargo\" \"$h/.rustup\"; do [ -e \"$d\" ] && chown -R \"$u\" \"$d\" 2>/dev/null || true; done; fi; true";
    size_t need = strlen(fmt) + strlen(user) + 1;
    char *s = xmalloc(need);
    if (s)
        snprintf(s, need, fmt, user);
    return s;
}

/* ------------------------------------------------------------------ */
/* text formatting                                                      */
/* ------------------------------------------------------------------ */

char *bot_strip_sgr(const char *s) {
    /* drop CSI ... <letter>, OSC ... BEL/ESC\, lone ESC + char, and CR */
    size_t cap = strlen(s) + 1;
    char *out = xmalloc(cap);
    if (!out)
        return NULL;
    size_t w = 0, i = 0, n = strlen(s);
    while (i < n) {
        unsigned char b = (unsigned char)s[i];
        if (b == 0x1b && i + 1 < n && s[i + 1] == '[') {
            size_t j = i + 2;
            while (j < n && !((s[j] >= 'A' && s[j] <= 'Z') ||
                              (s[j] >= 'a' && s[j] <= 'z')))
                j++;
            i = (j + 1 <= n) ? j + 1 : n;
            continue;
        }
        if (b == 0x1b) {
            if (i + 1 < n && s[i + 1] == ']') {
                size_t j = i + 2;
                while (j < n && s[j] != '\a') {
                    if (s[j] == 0x1b && j + 1 < n && s[j + 1] == '\\') {
                        j += 2;
                        break;
                    }
                    j++;
                }
                if (j < n && s[j] == '\a')
                    j++;
                i = j;
                continue;
            }
            i += (i + 1 < n) ? 2 : 1;
            continue;
        }
        if (b == '\r') {
            i++;
            continue;
        }
        /* copy one UTF-8 char */
        size_t cl = 1;
        if ((b & 0x80) == 0)
            cl = 1;
        else if ((b & 0xe0) == 0xc0)
            cl = 2;
        else if ((b & 0xf0) == 0xe0)
            cl = 3;
        else
            cl = 4;
        if (i + cl > n)
            cl = n - i;
        memcpy(out + w, s + i, cl);
        w += cl;
        i += cl;
    }
    out[w] = '\0';
    return out;
}

static size_t utf8_chars(const char *s) {
    size_t c = 0;
    for (size_t i = 0; s[i];) {
        unsigned char b = (unsigned char)s[i];
        size_t cl = ((b & 0x80) == 0) ? 1 : ((b & 0xe0) == 0xc0) ? 2 :
                      ((b & 0xf0) == 0xe0)                        ? 3 :
                                                                   4;
        i += cl;
        c++;
    }
    return c;
}

void bot_codeblock(const char *s, char *out, size_t n) {
    char *clean = bot_strip_sgr(s ? s : "");
    if (!clean) {
        snprintf(out, n, "```\n(empty)\n```");
        return;
    }
    /* trim trailing newlines */
    size_t len = strlen(clean);
    while (len > 0 && (clean[len - 1] == '\n' || clean[len - 1] == '\r'))
        clean[--len] = '\0';
    char body[1900];
    if (len > 1800) {
        /* take first 1790 chars (byte scan back to boundary) */
        size_t cut = 1790;
        while (cut > 0 && (clean[cut] & 0xc0) == 0x80)
            cut--;
        memcpy(body, clean, cut);
        body[cut] = '\0';
        snprintf(body + cut, sizeof(body) - cut, "\n…truncated");
    } else {
        snprintf(body, sizeof body, "%s", clean);
    }
    free(clean);
    if (!body[0])
        snprintf(out, n, "```\n(empty)\n```");
    else
        snprintf(out, n, "```\n%s\n```", body);
    (void)utf8_chars;
}

/* last screen-clear sequence end offset, or 0 */
static size_t after_last_clear(const char *s) {
    size_t last = 0, i = 0, n = strlen(s);
    while (i < n) {
        unsigned char b = (unsigned char)s[i];
        if (b == 0x1b && i + 1 < n) {
            if (s[i + 1] == 'c') {
                last = i + 2;
                i += 2;
                continue;
            }
            if (s[i + 1] == '[') {
                size_t j = i + 2;
                while (j < n && ((s[j] >= '0' && s[j] <= '9') || s[j] == ';' ||
                                 s[j] == '?'))
                    j++;
                if (j < n && (s[j] == 'J' || s[j] == 'H' || s[j] == 'f')) {
                    last = j + 1;
                    i = j + 1;
                    continue;
                }
            }
        }
        unsigned char c2 = (unsigned char)s[i];
        size_t cl = ((c2 & 0x80) == 0) ? 1 : ((c2 & 0xe0) == 0xc0) ? 2 :
                        ((c2 & 0xf0) == 0xe0)                      ? 3 :
                                                                    4;
        i += cl;
        if (i > n)
            i = n;
    }
    return last;
}

void bot_plain_tail(const char *body, char *out, size_t n) {
    /* normalize newlines */
    size_t blen = strlen(body ? body : "");
    char *norm = xmalloc(blen + 1);
    if (!norm) {
        snprintf(out, n, "```\n(empty)\n```");
        return;
    }
    size_t w = 0;
    for (size_t i = 0; body && body[i]; i++) {
        if (body[i] == '\r' && body[i + 1] == '\n') {
            norm[w++] = '\n';
            i++;
        } else if (body[i] == '\r') {
            norm[w++] = '\n';
        } else {
            norm[w++] = body[i];
        }
    }
    norm[w] = '\0';
    /* trim trailing whitespace */
    while (w > 0 && (norm[w - 1] == ' ' || norm[w - 1] == '\t' ||
                     norm[w - 1] == '\n'))
        norm[--w] = '\0';
    char *clean = bot_strip_sgr(norm + after_last_clear(norm));
    free(norm);
    if (!clean) {
        snprintf(out, n, "```\n(empty)\n```");
        return;
    }
    /* fit bottom lines within 1750 chars (char count for limits,
     * byte length for copies) */
    char kept[1800];
    kept[0] = '\0';
    size_t klen = 0; /* bytes used in kept */
    size_t total = 0; /* chars used */
    bool truncated = false;
    /* collect line starts */
    const char *lines[4096];
    size_t nlines = 0;
    lines[nlines++] = clean;
    for (char *p = clean; *p && nlines < 4096; p++) {
        if (*p == '\n') {
            *p = '\0';
            if (nlines < 4096)
                lines[nlines++] = p + 1;
        }
    }
    /* walk from the bottom */
    for (size_t li = nlines; li-- > 0;) {
        size_t blen = strlen(lines[li]);
        size_t clen = utf8_chars(lines[li]);
        if (clen + 1 > 1750) {
            if (klen == 0 && blen > 0) {
                /* keep tail of the huge line (byte-based, char-boundary) */
                size_t start = blen > 1749 ? blen - 1749 : 0;
                while (start < blen && (lines[li][start] & 0xc0) == 0x80)
                    start++;
                size_t tail = blen - start;
                if (tail + 2 > sizeof kept)
                    tail = sizeof(kept) - 2;
                memcpy(kept, lines[li] + start, tail);
                kept[tail] = '\0';
                klen = tail;
            }
            truncated = true;
            break;
        }
        if (total + clen + 1 > 1750) {
            truncated = true;
            break;
        }
        /* prepend "line\n" */
        if (klen + blen + 2 > sizeof kept) {
            truncated = true;
            break;
        }
        memmove(kept + blen + 1, kept, klen + 1);
        memcpy(kept, lines[li], blen);
        kept[blen] = '\n';
        klen += blen + 1;
        total += clen + 1;
        if (li == 0)
            break;
    }
    free(clean);
    /* strip trailing newline, check empty */
    while (klen > 0 && kept[klen - 1] == '\n')
        kept[--klen] = '\0';
    if (klen == 0)
        snprintf(out, n, "```\n(empty)\n```");
    else if (truncated)
        snprintf(out, n, "```\n…\n%s\n```", kept);
    else
        snprintf(out, n, "```\n%s\n```", kept);
}

/* ------------------------------------------------------------------ */
/* download                                                               */
/* ------------------------------------------------------------------ */

typedef struct {
    unsigned char *p;
    size_t n, cap, max;
    bool overflow;
} dlbuf_t;

static size_t dl_write(char *ptr, size_t sz, size_t nm, void *ud) {
    dlbuf_t *d = ud;
    size_t n = sz * nm;
    if (d->n + n > d->max) {
        d->overflow = true;
        return 0;
    }
    if (d->n + n + 1 > d->cap) {
        size_t nc = (d->n + n + 1) * 2;
        unsigned char *np = xrealloc(d->p, nc);
        if (!np)
            return 0;
        d->p = np;
        d->cap = nc;
    }
    memcpy(d->p + d->n, ptr, n);
    d->n += n;
    return n;
}

int bot_download_atts(const disc_attachment_t *atts, size_t n,
                      disc_file_t **out_files, size_t *out_n, char ***out_bufs) {
    disc_file_t *files = xmalloc((n ? n : 1) * sizeof *files);
    char **bufs = xmalloc((n ? n : 1) * sizeof *bufs);
    if (!files || !bufs) {
        free(files);
        free(bufs);
        return -1;
    }
    size_t kept = 0;
    for (size_t i = 0; i < n; i++) {
        if (atts[i].size > 25u * 1024u * 1024u)
            continue;
        char name[128];
        const char *base =
            strrchr(atts[i].filename ? atts[i].filename : "", '/');
        base = base ? base + 1 : (atts[i].filename ? atts[i].filename : "");
        size_t w = 0;
        for (size_t k = 0; base[k] && w + 1 < sizeof name; k++) {
            char ch = base[k];
            if ((ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
                (ch >= '0' && ch <= '9') || ch == '.' || ch == '-' || ch == '_')
                name[w++] = ch;
        }
        name[w] = '\0';
        char *t = name;
        while (*t == '.')
            t++;
        if (!*t)
            snprintf(name, sizeof name, "file.bin");
        else if (t != name)
            memmove(name, t, strlen(t) + 1);
        if (strlen(name) > 100)
            name[100] = '\0';
        unsigned char *data = NULL;
        size_t len = 0;
        if (!atts[i].url ||
            bot_download(atts[i].url, 25u * 1024u * 1024u + 1, &data, &len) !=
                0 ||
            len > 25u * 1024u * 1024u) {
            free(data);
            continue;
        }
        char *nm = xstrdup(name);
        if (!nm) {
            free(data);
            continue;
        }
        files[kept].name = nm;
        files[kept].data = data;
        files[kept].len = len;
        bufs[kept] = (char *)data;
        kept++;
    }
    *out_files = files;
    *out_n = kept;
    *out_bufs = bufs;
    return 0;
}

void bot_free_dl_files(disc_file_t *files, size_t n, char **bufs) {
    if (!files || !bufs) {
        free(files);
        free(bufs);
        return;
    }
    for (size_t i = 0; i < n; i++) {
        free((void *)files[i].name);
        free(bufs[i]);
    }
    free(files);
    free(bufs);
}

int bot_download(const char *url, size_t max_bytes, unsigned char **data,
                 size_t *len_out) {
    *data = NULL;
    if (len_out)
        *len_out = 0;
    CURL *h = curl_easy_init();
    if (!h)
        return -1;
    dlbuf_t d = { NULL, 0, 0, max_bytes, false };
    curl_easy_setopt(h, CURLOPT_URL, url);
    curl_easy_setopt(h, CURLOPT_USERAGENT, "DiscordBot (artixy-c, 0.3.0)");
    curl_easy_setopt(h, CURLOPT_TIMEOUT, 60L);
    curl_easy_setopt(h, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, dl_write);
    curl_easy_setopt(h, CURLOPT_WRITEDATA, &d);
    CURLcode rc = curl_easy_perform(h);
    long code = 0;
    if (rc == CURLE_OK)
        curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, &code);
    curl_easy_cleanup(h);
    if (rc != CURLE_OK || code < 200 || code >= 300 || d.overflow) {
        free(d.p);
        return -1;
    }
    *data = d.p;
    if (len_out)
        *len_out = d.n;
    return 0;
}
