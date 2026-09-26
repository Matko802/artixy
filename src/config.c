#include "config.h"
#include "util.h"

#include <ctype.h>
#include <jansson.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* ------------------------------------------------------------------ */
/* strmap                                                               */
/* ------------------------------------------------------------------ */

void strmap_init(strmap_t *m) {
    m->keys = NULL;
    m->vals = NULL;
    m->len = 0;
    m->cap = 0;
}

void strmap_free(strmap_t *m) {
    for (size_t i = 0; i < m->len; i++) {
        free(m->keys[i]);
        free(m->vals[i]);
    }
    free(m->keys);
    free(m->vals);
    strmap_init(m);
}

int strmap_set(strmap_t *m, const char *key, const char *val) {
    for (size_t i = 0; i < m->len; i++) {
        if (strcmp(m->keys[i], key) == 0) {
            char *nv = xstrdup(val);
            if (!nv)
                return -1;
            free(m->vals[i]);
            m->vals[i] = nv;
            return 0;
        }
    }
    if (m->len == m->cap) {
        size_t ncap = m->cap ? m->cap * 2 : 8;
        char **nk = xrealloc(m->keys, ncap * sizeof *nk);
        char **nv = xrealloc(m->vals, ncap * sizeof *nv);
        if (!nk || !nv) {
            free(nk);
            free(nv);
            return -1;
        }
        m->keys = nk;
        m->vals = nv;
        m->cap = ncap;
    }
    /* NOTE: on partial failure (second xstrdup fails) the key slot is
     * already appended; keep it simple and report failure — caller treats
     * OOM as fatal. */
    m->keys[m->len] = xstrdup(key);
    m->vals[m->len] = xstrdup(val);
    if (!m->keys[m->len] || !m->vals[m->len]) {
        free(m->keys[m->len]);
        free(m->vals[m->len]);
        return -1;
    }
    m->len++;
    return 0;
}

const char *strmap_get(const strmap_t *m, const char *key) {
    for (size_t i = 0; i < m->len; i++) {
        if (strcmp(m->keys[i], key) == 0)
            return m->vals[i];
    }
    return NULL;
}

bool strmap_equal(const strmap_t *a, const strmap_t *b) {
    if (a->len != b->len)
        return false;
    for (size_t i = 0; i < a->len; i++) {
        const char *v = strmap_get(b, a->keys[i]);
        if (!v || strcmp(v, a->vals[i]) != 0)
            return false;
    }
    return true;
}

/* ------------------------------------------------------------------ */
/* defaults                                                             */
/* ------------------------------------------------------------------ */

const char *config_default_model(void) {
    return "llama3.1";
}

const char *config_default_host(void) {
    return "http://127.0.0.1:11434";
}

double config_clamp_temperature(double t) {
    if (t != t)
        return 0.8; /* NaN */
    if (t < 0.0)
        return 0.0;
    if (t > 2.0)
        return 2.0;
    return t;
}

bool config_valid_model_name(const char *s) {
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
    /* trailing blank check: whole string must be the single token */
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
    if (strstr(s, "..") || strstr(s, "//"))
        return false;
    return true;
}

/* ------------------------------------------------------------------ */
/* path + template                                                      */
/* ------------------------------------------------------------------ */

const char *config_path(void) {
    static char buf[4096];
    const char *xdg = getenv("XDG_CONFIG_HOME");
    if (xdg && *xdg) {
        snprintf(buf, sizeof buf, "%s/artixy/config.jsonc", xdg);
    } else {
        const char *home = getenv("HOME");
        if (!home || !*home) {
            snprintf(buf, sizeof buf, "config.jsonc");
        } else {
            snprintf(buf, sizeof buf, "%s/.config/artixy/config.jsonc", home);
        }
    }
    return buf;
}

static const char *config_template =
    "// artixy config — edits hot-apply within seconds, no restart needed.\n"
    "// File location: ~/.config/artixy/config.jsonc (NOT the project dir).\n"
    "// This is JSONC: plain JSON plus // and /* */ comments and trailing commas.\n"
    "{\n"
    "  // Your Discord user ID (number). Get it via Discord: Settings > Advanced > Developer Mode,\n"
    "  // then right-click your name > Copy User ID.\n"
    "  \"owner_id\": 0,\n"
    "  // Bot token from https://discord.com/developers/applications. Keep secret, never git.\n"
    "  \"discord_token\": \"\",\n"
    "  // libvirt domain name, e.g. \"artix\". VM commands error out until this is set.\n"
    "  \"vm_name\": \"\",\n"
    "  \"blocked_ids\": [],\n"
    "  \"war_mode\": false,\n"
    "  \"sayas_enabled\": false,\n"
    "  // Channel ID for boot messages, or null to turn them off.\n"
    "  \"notify_channel\": null,\n"
    "  \"managers\": [],\n"
    "  \"admin_ids\": [],\n"
    "  // Discord user ID (as string) -> linux username in the VM.\n"
    "  \"linux\": {},\n"
    "  // Discord user ID (as string) -> \"fish\" or \"bash\".\n"
    "  \"shells\": {},\n"
    "  \"ai_enabled\": false,\n"
    "  \"ai_model\": \"llama3.1\",\n"
    "  \"ollama_host\": \"http://127.0.0.1:11434\",\n"
    "  // Backstory/system prompt. Use \\n for newlines; long text is fine on one line.\n"
    "  \"ai_prompt\": \"\",\n"
    "  \"ai_temperature\": 0.8,\n"
    "  \"ai_think\": false,\n"
    "}\n";

static void lock_private(const char *path) {
    chmod(path, 0600);
}

void config_ensure_template(void) {
    const char *path = config_path();
    struct stat st;
    if (stat(path, &st) == 0)
        return;
    /* mkdir -p dirname (best-effort; fopen failure surfaces below) */
    char dir[4096];
    snprintf(dir, sizeof dir, "%s", path);
    char *slash = strrchr(dir, '/');
    if (slash) {
        *slash = '\0';
        (void)mkdir_p(dir);
    }
    FILE *f = fopen(path, "w");
    if (!f)
        return;
    fputs(config_template, f);
    fclose(f);
    lock_private(path);
}

/* ------------------------------------------------------------------ */
/* JSONC stripping: drop // comments, block comments and trailing       */
/* commas, honouring double-quoted strings with backslash escapes.      */
/* ------------------------------------------------------------------ */

char *config_strip_jsonc(const char *src, size_t len) {
    char *out = xmalloc(len + 1);
    if (!out)
        return NULL;
    size_t w = 0; /* write index */
    size_t i = 0;
    while (i < len) {
        char c = src[i];
        if (c == '"') {
            out[w++] = c;
            i++;
            while (i < len) {
                out[w++] = src[i];
                if (src[i] == '\\' && i + 1 < len) {
                    out[w++] = src[i + 1];
                    i += 2;
                    continue;
                }
                if (src[i] == '"') {
                    i++;
                    break;
                }
                i++;
            }
            continue;
        }
        if (c == '/' && i + 1 < len && src[i + 1] == '/') {
            i += 2;
            while (i < len && src[i] != '\n')
                i++;
            continue;
        }
        if (c == '/' && i + 1 < len && src[i + 1] == '*') {
            i += 2;
            while (i < len) {
                if (src[i] == '*' && i + 1 < len && src[i + 1] == '/') {
                    i += 2;
                    break;
                }
                i++;
            }
            continue;
        }
        if (c == '}' || c == ']') {
            /*
             * Drop every trailing comma (whitespace-separated) before the
             * bracket. Only commas directly preceding the bracket (past
             * whitespace) can match, so real separators (followed by a
             * value) are never touched — and repeated runs are stable.
             */
            for (;;) {
                size_t j = w;
                while (j > 0 && (out[j - 1] == ' ' || out[j - 1] == '\t' ||
                                 out[j - 1] == '\n' || out[j - 1] == '\r'))
                    j--;
                if (j > 0 && out[j - 1] == ',')
                    w = j - 1;
                else
                    break;
            }
            out[w++] = c;
            i++;
            continue;
        }
        out[w++] = c;
        i++;
    }
    out[w] = '\0';
    return out;
}

/* ------------------------------------------------------------------ */
/* loading                                                              */
/* ------------------------------------------------------------------ */

void file_config_init(file_config_t *c) {
    memset(c, 0, sizeof *c);
    strmap_init(&c->linux);
    strmap_init(&c->shells);
}

void file_config_free(file_config_t *c) {
    free(c->blocked_ids);
    free(c->discord_token);
    free(c->vm_name);
    free(c->managers);
    free(c->admin_ids);
    strmap_free(&c->linux);
    strmap_free(&c->shells);
    free(c->ai_model);
    free(c->ollama_host);
    free(c->ai_prompt);
    file_config_init(c);
}

static int load_u64_array(json_t *arr, uint64_t **out, size_t *nout) {
    if (!json_is_array(arr))
        return -1;
    size_t n = json_array_size(arr);
    uint64_t *v = NULL;
    if (n) {
        v = xmalloc(n * sizeof *v);
        if (!v)
            return -1;
        for (size_t i = 0; i < n; i++) {
            json_t *e = json_array_get(arr, i);
            if (!json_is_integer(e)) {
                free(v);
                return -1;
            }
            long long ll = json_integer_value(e);
            if (ll < 0) {
                free(v);
                return -1;
            }
            v[i] = (uint64_t)ll;
        }
    }
    *out = v;
    *nout = n;
    return 0;
}

static int load_strmap(json_t *obj, strmap_t *m) {
    if (!json_is_object(obj))
        return -1;
    const char *k;
    json_t *v;
    json_object_foreach(obj, k, v) {
        if (!json_is_string(v))
            return -1;
        if (strmap_set(m, k, json_string_value(v)) != 0)
            return -1;
    }
    return 0;
}

static char *dup_nonempty(const char *s) {
    if (!s)
        return NULL;
    while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r')
        s++;
    if (!*s)
        return NULL;
    return xstrdup(s);
}

static char *rtrim_slash(char *s) {
    size_t n = strlen(s);
    while (n > 0 && s[n - 1] == '/') {
        s[n - 1] = '\0';
        n--;
    }
    return s;
}

int config_load(file_config_t *out) {
    file_config_init(out);
    char *raw = NULL;
    size_t rawlen = 0;
    if (read_file(config_path(), &raw, &rawlen) != 0)
        goto defaults; /* missing/unreadable -> defaults */
    char *stripped = config_strip_jsonc(raw, rawlen);
    free(raw);
    if (!stripped)
        return -1;
    json_error_t err;
    json_t *root = json_loads(stripped, 0, &err);
    free(stripped);
    if (!root || !json_is_object(root)) {
        fprintf(stderr,
                "config: parse error in %s: %s — keeping current settings; "
                "fix the JSON (check commas and quotes)\n",
                config_path(), root ? "not an object" : err.text);
        json_decref(root);
        return -2;
    }

    json_t *v;
    v = json_object_get(root, "owner_id");
    if (json_is_integer(v) && json_integer_value(v) > 0) {
        out->has_owner_id = true;
        out->owner_id = (uint64_t)json_integer_value(v);
    }
    v = json_object_get(root, "blocked_ids");
    if (v && load_u64_array(v, &out->blocked_ids, &out->n_blocked) != 0)
        goto bad;
    v = json_object_get(root, "discord_token");
    if (json_is_string(v))
        out->discord_token = dup_nonempty(json_string_value(v));
    v = json_object_get(root, "vm_name");
    if (json_is_string(v))
        out->vm_name = dup_nonempty(json_string_value(v));
    v = json_object_get(root, "war_mode");
    if (json_is_boolean(v))
        out->war_mode = json_is_true(v);
    v = json_object_get(root, "sayas_enabled");
    if (json_is_boolean(v))
        out->sayas_enabled = json_is_true(v);
    v = json_object_get(root, "notify_channel");
    if (json_is_integer(v) && json_integer_value(v) > 0) {
        out->has_notify_channel = true;
        out->notify_channel = (uint64_t)json_integer_value(v);
    }
    v = json_object_get(root, "managers");
    if (v && load_u64_array(v, &out->managers, &out->n_managers) != 0)
        goto bad;
    v = json_object_get(root, "admin_ids");
    if (v && load_u64_array(v, &out->admin_ids, &out->n_admins) != 0)
        goto bad;
    v = json_object_get(root, "linux");
    if (v && load_strmap(v, &out->linux) != 0)
        goto bad;
    v = json_object_get(root, "shells");
    if (v && load_strmap(v, &out->shells) != 0)
        goto bad;
    v = json_object_get(root, "ai_enabled");
    if (json_is_boolean(v))
        out->ai_enabled = json_is_true(v);
    v = json_object_get(root, "ai_model");
    if (json_is_string(v) && config_valid_model_name(json_string_value(v)))
        out->ai_model = xstrdup(json_string_value(v));
    if (!out->ai_model)
        out->ai_model = xstrdup(config_default_model());
    v = json_object_get(root, "ollama_host");
    if (json_is_string(v)) {
        char *h = xstrdup(json_string_value(v));
        if (h) {
            /* trim spaces then trailing slashes */
            while (*h == ' ' || *h == '\t')
                memmove(h, h + 1, strlen(h));
            rtrim_slash(h);
            if (!*h) {
                free(h);
                h = xstrdup(config_default_host());
            }
            out->ollama_host = h;
        }
    }
    if (!out->ollama_host)
        out->ollama_host = xstrdup(config_default_host());
    v = json_object_get(root, "ai_prompt");
    if (json_is_string(v))
        out->ai_prompt = xstrdup(json_string_value(v));
    if (!out->ai_prompt)
        out->ai_prompt = xstrdup("");
    v = json_object_get(root, "ai_temperature");
    if (json_is_number(v))
        out->ai_temperature = config_clamp_temperature(json_number_value(v));
    else
        out->ai_temperature = 0.8;
    v = json_object_get(root, "ai_think");
    if (json_is_boolean(v))
        out->ai_think = json_is_true(v);

    if (!out->ai_model || !out->ollama_host || !out->ai_prompt)
        goto bad;
    json_decref(root);
    lock_private(config_path());
    return 0;

bad:
    json_decref(root);
    file_config_free(out);
    fprintf(stderr, "config: invalid value types in %s — keeping current settings\n",
            config_path());
    return -2;

defaults:
    out->ai_model = xstrdup(config_default_model());
    out->ollama_host = xstrdup(config_default_host());
    out->ai_prompt = xstrdup("");
    out->ai_temperature = 0.8;
    if (!out->ai_model || !out->ollama_host || !out->ai_prompt) {
        file_config_free(out);
        return -1;
    }
    return 0;
}
