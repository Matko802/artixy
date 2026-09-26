/* Raw Discord REST client over libcurl. */
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

/* struct discord_client is defined in discord_internal.h. */

/* ------------------------------------------------------------------ */
/* message / interaction free + parse                                    */
/* ------------------------------------------------------------------ */

static void free_attachments(disc_attachment_t *a, size_t n) {
    for (size_t i = 0; i < n; i++) {
        free(a[i].filename);
        free(a[i].url);
    }
    free(a);
}

void disc_message_free(disc_message_t *m) {
    if (!m)
        return;
    free(m->author_name);
    free(m->global_name);
    free(m->member_nick);
    free(m->content);
    free(m->mentions);
    free_attachments(m->attachments, m->n_attachments);
    free(m->ref_msg_author_name);
    free(m->ref_msg_content);
    memset(m, 0, sizeof *m);
}

static char *dup_str(const char *s) {
    if (!s)
        return NULL;
    return xstrdup(s);
}

int disc_message_clone(const disc_message_t *src, disc_message_t *dst) {
    memset(dst, 0, sizeof *dst);
    dst->id = src->id;
    dst->channel_id = src->channel_id;
    dst->guild_id = src->guild_id;
    dst->author_id = src->author_id;
    dst->author_name = dup_str(src->author_name);
    dst->global_name = dup_str(src->global_name);
    dst->member_nick = dup_str(src->member_nick);
    dst->author_bot = src->author_bot;
    dst->from_webhook = src->from_webhook;
    dst->content = dup_str(src->content);
    if ((src->author_name && !dst->author_name) ||
        (src->content && !dst->content))
        goto fail;
    if (src->n_mentions) {
        dst->mentions = xmalloc(src->n_mentions * sizeof *dst->mentions);
        if (!dst->mentions)
            goto fail;
        memcpy(dst->mentions, src->mentions,
               src->n_mentions * sizeof *dst->mentions);
        dst->n_mentions = src->n_mentions;
    }
    if (src->n_attachments) {
        dst->attachments =
            xmalloc(src->n_attachments * sizeof *dst->attachments);
        if (!dst->attachments)
            goto fail;
        memset(dst->attachments, 0,
               src->n_attachments * sizeof *dst->attachments);
        for (size_t i = 0; i < src->n_attachments; i++) {
            dst->attachments[i].id = src->attachments[i].id;
            dst->attachments[i].filename =
                dup_str(src->attachments[i].filename);
            dst->attachments[i].url = dup_str(src->attachments[i].url);
            dst->attachments[i].size = src->attachments[i].size;
        }
        dst->n_attachments = src->n_attachments;
    }
    dst->has_reference = src->has_reference;
    dst->ref_channel_id = src->ref_channel_id;
    dst->ref_message_id = src->ref_message_id;
    dst->has_ref_msg = src->has_ref_msg;
    dst->ref_msg_id = src->ref_msg_id;
    dst->ref_msg_author_id = src->ref_msg_author_id;
    dst->ref_msg_author_name = dup_str(src->ref_msg_author_name);
    dst->ref_msg_content = dup_str(src->ref_msg_content);
    return 0;
fail:
    disc_message_free(dst);
    return -1;
}

void disc_interaction_free(disc_interaction_t *in) {
    if (!in)
        return;
    free(in->token);
    free(in->author_name);
    free(in->member_nick);
    free(in->command);
    for (size_t i = 0; i < in->n_options; i++) {
        free(in->options[i].name);
        free(in->options[i].str_val);
    }
    free(in->options);
    for (size_t i = 0; i < in->n_resolved_users; i++)
        free(in->resolved_users[i].username);
    free(in->resolved_users);
    free_attachments(in->resolved_attachments, in->n_resolved_attachments);
    memset(in, 0, sizeof *in);
}

int disc_interaction_clone(const disc_interaction_t *src,
                           disc_interaction_t *dst) {
    memset(dst, 0, sizeof *dst);
    dst->id = src->id;
    dst->token = dup_str(src->token);
    dst->type = src->type;
    dst->channel_id = src->channel_id;
    dst->guild_id = src->guild_id;
    dst->author_id = src->author_id;
    dst->author_name = dup_str(src->author_name);
    dst->member_nick = dup_str(src->member_nick);
    dst->command = dup_str(src->command);
    if ((src->token && !dst->token) ||
        (src->author_name && !dst->author_name))
        goto fail;
    if (src->n_options) {
        dst->options = xmalloc(src->n_options * sizeof *dst->options);
        if (!dst->options)
            goto fail;
        memset(dst->options, 0, src->n_options * sizeof *dst->options);
        for (size_t i = 0; i < src->n_options; i++) {
            dst->options[i].name = dup_str(src->options[i].name);
            dst->options[i].type = src->options[i].type;
            dst->options[i].str_val = dup_str(src->options[i].str_val);
            dst->options[i].int_val = src->options[i].int_val;
            dst->options[i].bool_val = src->options[i].bool_val;
            dst->options[i].user_id = src->options[i].user_id;
            dst->options[i].attachment_id = src->options[i].attachment_id;
            if (!dst->options[i].name)
                goto fail;
        }
        dst->n_options = src->n_options;
    }
    if (src->n_resolved_users) {
        dst->resolved_users =
            xmalloc(src->n_resolved_users * sizeof *dst->resolved_users);
        if (!dst->resolved_users)
            goto fail;
        memset(dst->resolved_users, 0,
               src->n_resolved_users * sizeof *dst->resolved_users);
        for (size_t i = 0; i < src->n_resolved_users; i++) {
            dst->resolved_users[i].id = src->resolved_users[i].id;
            dst->resolved_users[i].username =
                dup_str(src->resolved_users[i].username);
        }
        dst->n_resolved_users = src->n_resolved_users;
    }
    if (src->n_resolved_attachments) {
        dst->resolved_attachments = xmalloc(src->n_resolved_attachments *
                                            sizeof *dst->resolved_attachments);
        if (!dst->resolved_attachments)
            goto fail;
        memset(dst->resolved_attachments, 0,
               src->n_resolved_attachments * sizeof *dst->resolved_attachments);
        for (size_t i = 0; i < src->n_resolved_attachments; i++) {
            dst->resolved_attachments[i].id = src->resolved_attachments[i].id;
            dst->resolved_attachments[i].filename =
                dup_str(src->resolved_attachments[i].filename);
            dst->resolved_attachments[i].url =
                dup_str(src->resolved_attachments[i].url);
            dst->resolved_attachments[i].size =
                src->resolved_attachments[i].size;
        }
        dst->n_resolved_attachments = src->n_resolved_attachments;
    }
    return 0;
fail:
    disc_interaction_free(dst);
    return -1;
}

static uint64_t get_snowflake(json_t *obj, const char *key) {
    json_t *v = json_object_get(obj, key);
    if (json_is_string(v))
        return (uint64_t)strtoull(json_string_value(v), NULL, 10);
    if (json_is_integer(v) && json_integer_value(v) > 0)
        return (uint64_t)json_integer_value(v);
    return 0;
}

static char *get_str(json_t *obj, const char *key) {
    json_t *v = json_object_get(obj, key);
    if (json_is_string(v))
        return xstrdup(json_string_value(v));
    return NULL;
}

static int parse_attachments(json_t *arr, disc_attachment_t **out, size_t *nout) {
    *out = NULL;
    *nout = 0;
    if (!json_is_array(arr))
        return 0;
    size_t n = json_array_size(arr);
    disc_attachment_t *a = xmalloc((n ? n : 1) * sizeof *a);
    if (!a)
        return -1;
    memset(a, 0, (n ? n : 1) * sizeof *a);
    for (size_t i = 0; i < n; i++) {
        json_t *e = json_array_get(arr, i);
        a[i].id = get_snowflake(e, "id");
        a[i].filename = get_str(e, "filename");
        a[i].url = get_str(e, "url");
        json_t *sz = json_object_get(e, "size");
        if (json_is_integer(sz) && json_integer_value(sz) > 0)
            a[i].size = (uint64_t)json_integer_value(sz);
    }
    *out = a;
    *nout = n;
    return 0;
}

static int parse_mentions(json_t *arr, uint64_t **out, size_t *nout) {
    *out = NULL;
    *nout = 0;
    if (!json_is_array(arr))
        return 0;
    size_t n = json_array_size(arr);
    uint64_t *v = xmalloc((n ? n : 1) * sizeof *v);
    if (!v)
        return -1;
    for (size_t i = 0; i < n; i++)
        v[i] = get_snowflake(json_array_get(arr, i), "id");
    *out = v;
    *nout = n;
    return 0;
}

int disc_parse_message(void *json_obj, disc_message_t *out) {
    json_t *o = json_obj;
    memset(out, 0, sizeof *out);
    if (!json_is_object(o))
        return -1;
    out->id = get_snowflake(o, "id");
    out->channel_id = get_snowflake(o, "channel_id");
    out->guild_id = get_snowflake(o, "guild_id");
    json_t *au = json_object_get(o, "author");
    if (json_is_object(au)) {
        out->author_id = get_snowflake(au, "id");
        out->author_name = get_str(au, "username");
        out->global_name = get_str(au, "global_name");
        json_t *b = json_object_get(au, "bot");
        out->author_bot = json_is_true(b);
    }
    out->from_webhook = json_object_get(o, "webhook_id") != NULL;
    if (!out->author_name)
        out->author_name = xstrdup("someone");
    out->content = get_str(o, "content");
    if (!out->content)
        out->content = xstrdup("");
    json_t *mem = json_object_get(o, "member");
    if (json_is_object(mem))
        out->member_nick = get_str(mem, "nick");
    if (parse_mentions(json_object_get(o, "mentions"), &out->mentions,
                        &out->n_mentions) != 0)
        goto fail;
    if (parse_attachments(json_object_get(o, "attachments"), &out->attachments,
                           &out->n_attachments) != 0)
        goto fail;
    json_t *ref = json_object_get(o, "message_reference");
    if (json_is_object(ref)) {
        uint64_t mid = get_snowflake(ref, "message_id");
        if (mid) {
            out->has_reference = true;
            out->ref_message_id = mid;
            out->ref_channel_id = get_snowflake(ref, "channel_id");
            if (!out->ref_channel_id)
                out->ref_channel_id = out->channel_id;
        }
    }
    json_t *rm = json_object_get(o, "referenced_message");
    if (json_is_object(rm)) {
        out->has_ref_msg = true;
        out->ref_msg_id = get_snowflake(rm, "id");
        json_t *rau = json_object_get(rm, "author");
        if (json_is_object(rau)) {
            out->ref_msg_author_id = get_snowflake(rau, "id");
            out->ref_msg_author_name = get_str(rau, "username");
        }
        out->ref_msg_content = get_str(rm, "content");
    }
    if (!out->author_name || !out->content)
        goto fail;
    return 0;
fail:
    disc_message_free(out);
    return -1;
}

static int parse_options(json_t *arr, disc_option_t **out, size_t *nout) {
    *out = NULL;
    *nout = 0;
    if (!json_is_array(arr))
        return 0;
    size_t n = json_array_size(arr);
    disc_option_t *o = xmalloc((n ? n : 1) * sizeof *o);
    if (!o)
        return -1;
    memset(o, 0, (n ? n : 1) * sizeof *o);
    for (size_t i = 0; i < n; i++) {
        json_t *e = json_array_get(arr, i);
        o[i].name = get_str(e, "name");
        json_t *t = json_object_get(e, "type");
        o[i].type = json_is_integer(t) ? (int)json_integer_value(t) : 0;
        json_t *v = json_object_get(e, "value");
        if (json_is_string(v))
            o[i].str_val = xstrdup(json_string_value(v));
        else if (json_is_integer(v))
            o[i].int_val = json_integer_value(v);
        else if (json_is_boolean(v))
            o[i].bool_val = json_is_true(v);
        if (o[i].type == 6 && json_is_string(v))
            o[i].user_id = (uint64_t)strtoull(json_string_value(v), NULL, 10);
        if (!o[i].name)
            goto fail;
    }
    *out = o;
    *nout = n;
    return 0;
fail:
    for (size_t i = 0; i < n; i++) {
        free(o[i].name);
        free(o[i].str_val);
    }
    free(o);
    return -1;
}

int disc_parse_interaction(void *json_obj, disc_interaction_t *out) {
    json_t *o = json_obj;
    memset(out, 0, sizeof *out);
    if (!json_is_object(o))
        return -1;
    out->id = get_snowflake(o, "id");
    out->token = get_str(o, "token");
    json_t *t = json_object_get(o, "type");
    out->type = json_is_integer(t) ? (int)json_integer_value(t) : 0;
    out->channel_id = get_snowflake(o, "channel_id");
    out->guild_id = get_snowflake(o, "guild_id");
    /* author: member.user in guilds, user in DMs */
    json_t *mem = json_object_get(o, "member");
    json_t *u = NULL;
    if (json_is_object(mem)) {
        out->member_nick = get_str(mem, "nick");
        u = json_object_get(mem, "user");
    }
    if (!json_is_object(u))
        u = json_object_get(o, "user");
    if (json_is_object(u)) {
        out->author_id = get_snowflake(u, "id");
        out->author_name = get_str(u, "username");
    }
    if (!out->author_name)
        out->author_name = xstrdup("someone");
    json_t *d = json_object_get(o, "data");
    if (json_is_object(d)) {
        out->command = get_str(d, "name");
        if (parse_options(json_object_get(d, "options"), &out->options,
                           &out->n_options) != 0)
            goto fail;
        json_t *res = json_object_get(d, "resolved");
        if (json_is_object(res)) {
            json_t *users = json_object_get(res, "users");
            if (json_is_object(users)) {
                const char *k;
                json_t *v;
                size_t n = 0;
                json_object_foreach(users, k, v) { (void)k; (void)v; n++; }
                out->resolved_users = xmalloc((n ? n : 1) * sizeof *out->resolved_users);
                if (!out->resolved_users)
                    goto fail;
                memset(out->resolved_users, 0, (n ? n : 1) * sizeof *out->resolved_users);
                size_t i = 0;
                json_object_foreach(users, k, v) {
                    out->resolved_users[i].id = (uint64_t)strtoull(k, NULL, 10);
                    out->resolved_users[i].username = get_str(v, "username");
                    i++;
                }
                out->n_resolved_users = n;
            }
            json_t *atts = json_object_get(res, "attachments");
            if (json_is_object(atts)) {
                /* object keyed by attachment id */
                const char *k;
                json_t *v;
                size_t n = 0;
                json_object_foreach(atts, k, v) { (void)k; (void)v; n++; }
                disc_attachment_t *a = xmalloc((n ? n : 1) * sizeof *a);
                if (!a)
                    goto fail;
                memset(a, 0, (n ? n : 1) * sizeof *a);
                size_t i = 0;
                json_object_foreach(atts, k, v) {
                    a[i].id = (uint64_t)strtoull(k, NULL, 10);
                    a[i].filename = get_str(v, "filename");
                    a[i].url = get_str(v, "url");
                    json_t *sz = json_object_get(v, "size");
                    if (json_is_integer(sz) && json_integer_value(sz) > 0)
                        a[i].size = (uint64_t)json_integer_value(sz);
                    i++;
                }
                out->resolved_attachments = a;
                out->n_resolved_attachments = n;
            }
        }
    }
    if (!out->token)
        goto fail;
    return 0;
fail:
    disc_interaction_free(out);
    return -1;
}

/* ------------------------------------------------------------------ */
/* client                                                                */
/* ------------------------------------------------------------------ */

discord_client_t *discord_new(const char *token) {
    discord_client_t *c = xmalloc(sizeof *c);
    if (!c)
        return NULL;
    memset(c, 0, sizeof *c);
    c->token = xstrdup(token ? token : "");
    c->running = true;
    if (!c->token) {
        free(c);
        return NULL;
    }
    return c;
}

void discord_free(discord_client_t *c) {
    if (!c)
        return;
    free(c->token);
    free(c);
}

void discord_on_message(discord_client_t *c, disc_msg_cb cb, void *ud) {
    c->on_message = cb;
    c->msg_ud = ud;
}

void discord_on_interaction(discord_client_t *c, disc_interaction_cb cb, void *ud) {
    c->on_interaction = cb;
    c->interaction_ud = ud;
}

void discord_on_ready(discord_client_t *c, disc_ready_cb cb, void *ud) {
    c->on_ready = cb;
    c->ready_ud = ud;
}

uint64_t discord_bot_id(discord_client_t *c) {
    return c->bot_id;
}

uint64_t discord_app_id(discord_client_t *c) {
    return c->app_id;
}

/* internal accessors for gateway.c */
const char *discord_token(const discord_client_t *c) {
    return c->token;
}
void discord_set_ids(discord_client_t *c, uint64_t bot, uint64_t app) {
    c->bot_id = bot;
    c->app_id = app;
}
void discord_emit_message(discord_client_t *c, const disc_message_t *m) {
    if (c->on_message)
        c->on_message(c, m, c->msg_ud);
}
void discord_emit_interaction(discord_client_t *c, const disc_interaction_t *in) {
    if (c->on_interaction)
        c->on_interaction(c, in, c->interaction_ud);
}
void discord_emit_ready(discord_client_t *c) {
    if (c->on_ready)
        c->on_ready(c, c->bot_id, c->app_id, c->ready_ud);
}
bool discord_is_running(discord_client_t *c) {
    return c->running;
}

/* ------------------------------------------------------------------ */
/* REST transport                                                        */
/* ------------------------------------------------------------------ */

typedef struct {
    char *data;
    size_t len;
} resp_buf_t;

static size_t write_cb(char *ptr, size_t size, size_t nmemb, void *ud) {
    resp_buf_t *b = ud;
    size_t n = size * nmemb;
    char *nb = xrealloc(b->data, b->len + n + 1);
    if (!nb)
        return 0; /* abort transfer */
    b->data = nb;
    memcpy(b->data + b->len, ptr, n);
    b->len += n;
    b->data[b->len] = '\0';
    return n;
}

static CURL *rest_handle(discord_client_t *c, const char *url) {
    CURL *h = curl_easy_init();
    if (!h)
        return NULL;
    curl_easy_setopt(h, CURLOPT_URL, url);
    curl_easy_setopt(h, CURLOPT_USERAGENT, "DiscordBot (artixy-c, 0.3.0)");
    curl_easy_setopt(h, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(h, CURLOPT_FOLLOWLOCATION, 1L);
    struct curl_slist *hdrs = NULL;
    char auth[512];
    snprintf(auth, sizeof auth, "Authorization: Bot %s", c->token);
    hdrs = curl_slist_append(hdrs, auth);
    curl_easy_setopt(h, CURLOPT_HTTPHEADER, hdrs);
    /* stash list pointer for cleanup via private data */
    curl_easy_setopt(h, CURLOPT_PRIVATE, hdrs);
    return h;
}

static void rest_cleanup(CURL *h) {
    struct curl_slist *hdrs = NULL;
    curl_easy_getinfo(h, CURLINFO_PRIVATE, &hdrs);
    curl_slist_free_all(hdrs);
    curl_easy_cleanup(h);
}

/*
 * Perform the request described by (method, url, mime-or-NULL, json-or-NULL).
 * On success returns 0 with *resp_out holding parsed JSON (or NULL for
 * empty/204 responses). Handles 429 (single retry after retry_after).
 */
static int rest_perform(discord_client_t *c, const char *method, const char *url,
                        curl_mime *mime, const char *json_body, json_t **resp_out) {
    (void)c;
    if (resp_out)
        *resp_out = NULL;
    int attempt = 0;
    long http_code = 0;
    resp_buf_t body = { NULL, 0 };
    CURL *h = NULL;
    int rc = -1;

    for (attempt = 0; attempt < 2; attempt++) {
        free(body.data);
        body.data = NULL;
        body.len = 0;
        h = rest_handle(c, url);
        if (!h)
            return -1;
        curl_easy_setopt(h, CURLOPT_CUSTOMREQUEST, method);
        curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, write_cb);
        curl_easy_setopt(h, CURLOPT_WRITEDATA, &body);
        if (mime) {
            curl_easy_setopt(h, CURLOPT_MIMEPOST, mime);
        } else if (json_body) {
            struct curl_slist *hdrs = NULL;
            curl_easy_getinfo(h, CURLINFO_PRIVATE, &hdrs);
            hdrs = curl_slist_append(hdrs, "Content-Type: application/json");
            curl_easy_setopt(h, CURLOPT_HTTPHEADER, hdrs);
            curl_easy_setopt(h, CURLOPT_PRIVATE, hdrs);
            curl_easy_setopt(h, CURLOPT_POSTFIELDS, json_body);
        }
        CURLcode res = curl_easy_perform(h);
        if (res != CURLE_OK) {
            fprintf(stderr, "artixy: REST %s %s failed: %s\n", method, url,
                    curl_easy_strerror(res));
            rest_cleanup(h);
            h = NULL;
            return -1;
        }
        curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, &http_code);
        rest_cleanup(h);
        h = NULL;

        if (http_code == 429 && body.data) {
            json_error_t e;
            json_t *r = json_loads(body.data, 0, &e);
            double wait = 1.0;
            if (r) {
                json_t *ra = json_object_get(r, "retry_after");
                if (json_is_number(ra))
                    wait = json_number_value(ra);
                json_decref(r);
            }
            if (wait < 0)
                wait = 1.0;
            if (wait > 10.0)
                wait = 10.0;
            fprintf(stderr, "artixy: REST 429, retrying in %.1fs\n", wait);
            struct timespec rts = { (time_t)wait,
                                    (long)((wait - (time_t)wait) * 1000000000.0) };
            nanosleep(&rts, NULL);
            continue;
        }
        break;
    }

    if (http_code < 200 || http_code >= 300) {
        fprintf(stderr, "artixy: REST %s %s failed HTTP %ld: %s\n", method,
                url, http_code, body.data ? body.data : "(empty)");
        free(body.data);
        return -1;
    }
    if (resp_out) {
        if (body.data && *body.data) {
            json_error_t e;
            *resp_out = json_loads(body.data, 0, &e);
            if (!*resp_out) {
                fprintf(stderr, "artixy: REST response JSON error: %s\n", e.text);
                free(body.data);
                return -1;
            }
        } else {
            *resp_out = json_object();
        }
    }
    free(body.data);
    rc = 0;
    return rc;
}

/* Last n<=100 messages helper above builds its URL inline; no url_path needed. */

/* POST/PUT/PATCH with optional JSON body; files==NULL unless multipart. */
static int rest_json(discord_client_t *c, const char *method, const char *path,
                     const char *json_body, json_t **resp) {
    char url[1024];
    snprintf(url, sizeof url, "%s%s", DISCORD_API, path);
    return rest_perform(c, method, url, NULL, json_body, resp);
}

static int rest_multipart(discord_client_t *c, const char *method, const char *path,
                          const char *payload_json, const disc_file_t *files,
                          size_t n_files, json_t **resp) {
    char url[1024];
    snprintf(url, sizeof url, "%s%s", DISCORD_API, path);
    curl_mime *mime = curl_mime_init(NULL);
    if (!mime)
        return -1;
    curl_mimepart *p = curl_mime_addpart(mime);
    curl_mime_name(p, "payload_json");
    curl_mime_data(p, payload_json ? payload_json : "{}", CURL_ZERO_TERMINATED);
    curl_mime_type(p, "application/json");
    for (size_t i = 0; i < n_files; i++) {
        char field[32];
        snprintf(field, sizeof field, "files[%zu]", i);
        curl_mimepart *f = curl_mime_addpart(mime);
        curl_mime_name(f, field);
        curl_mime_filename(f, files[i].name ? files[i].name : "file.bin");
        curl_mime_data(f, files[i].data, files[i].len);
    }
    /* rest_perform takes ownership pattern: perform then free mime */
    int rc;
    {
        /* inline perform to keep mime alive during the call */
        char full[1024];
        snprintf(full, sizeof full, "%s", url);
        /* temporarily reuse rest_perform via direct curl */
        resp_buf_t body = { NULL, 0 };
        CURL *h = rest_handle(c, full);
        if (!h) {
            curl_mime_free(mime);
            return -1;
        }
        curl_easy_setopt(h, CURLOPT_CUSTOMREQUEST, method);
        curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, write_cb);
        curl_easy_setopt(h, CURLOPT_WRITEDATA, &body);
        curl_easy_setopt(h, CURLOPT_MIMEPOST, mime);
        CURLcode res = curl_easy_perform(h);
        long code = 0;
        if (res == CURLE_OK)
            curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, &code);
        rest_cleanup(h);
        curl_mime_free(mime);
        if (res != CURLE_OK) {
            fprintf(stderr, "artixy: REST %s %s failed: %s\n", method, url,
                    curl_easy_strerror(res));
            free(body.data);
            return -1;
        }
        if (code < 200 || code >= 300) {
            fprintf(stderr, "artixy: REST %s %s failed HTTP %ld: %s\n", method,
                    url, code, body.data ? body.data : "(empty)");
            free(body.data);
            return -1;
        }
        if (resp) {
            if (body.data && *body.data) {
                json_error_t e;
                *resp = json_loads(body.data, 0, &e);
                free(body.data);
                if (!*resp) {
                    fprintf(stderr, "artixy: REST response JSON error: %s\n", e.text);
                    return -1;
                }
            } else {
                free(body.data);
                *resp = json_object();
            }
        } else {
            free(body.data);
        }
        rc = 0;
    }
    return rc;
}

static uint64_t resp_id(json_t *r) {
    if (!r)
        return 0;
    json_t *v = json_object_get(r, "id");
    if (json_is_string(v))
        return (uint64_t)strtoull(json_string_value(v), NULL, 10);
    return 0;
}

int discord_send_message(discord_client_t *c, uint64_t channel_id,
                         const char *content, const disc_file_t *files,
                         size_t n_files, uint64_t *out_id) {
    char path[128];
    snprintf(path, sizeof path, "/channels/%llu/messages",
             (unsigned long long)channel_id);
    json_t *resp = NULL;
    int rc;
    if (n_files) {
        json_t *pl = json_pack("{s:s}", "content", content ? content : "");
        char *s = json_dumps(pl, JSON_COMPACT);
        json_decref(pl);
        rc = rest_multipart(c, "POST", path, s, files, n_files, &resp);
        free(s);
    } else {
        json_t *pl = json_pack("{s:s}", "content", content ? content : "");
        char *s = json_dumps(pl, JSON_COMPACT);
        json_decref(pl);
        rc = rest_json(c, "POST", path, s, &resp);
        free(s);
    }
    if (rc == 0 && out_id)
        *out_id = resp_id(resp);
    json_decref(resp);
    return rc;
}

int discord_send_reply(discord_client_t *c, uint64_t channel_id, uint64_t reply_to,
                       const char *content, const disc_file_t *files,
                       size_t n_files, uint64_t *out_id) {
    char path[128];
    snprintf(path, sizeof path, "/channels/%llu/messages",
             (unsigned long long)channel_id);
    /* {"content": ..., "message_reference": {"message_id": ...}} */
    json_t *pl = json_pack("{s:s, s:{s:s}}", "content", content ? content : "",
                           "message_reference", "message_id", "");
    char mid[32];
    snprintf(mid, sizeof mid, "%llu", (unsigned long long)reply_to);
    json_object_set_new(json_object_get(pl, "message_reference"), "message_id",
                        json_string(mid));
    char *s = json_dumps(pl, JSON_COMPACT);
    json_decref(pl);
    json_t *resp = NULL;
    int rc;
    if (n_files)
        rc = rest_multipart(c, "POST", path, s, files, n_files, &resp);
    else
        rc = rest_json(c, "POST", path, s, &resp);
    free(s);
    if (rc == 0 && out_id)
        *out_id = resp_id(resp);
    json_decref(resp);
    return rc;
}

int discord_edit_message(discord_client_t *c, uint64_t channel_id,
                         uint64_t message_id, const char *content,
                         const disc_file_t *files, size_t n_files) {
    char path[160];
    snprintf(path, sizeof path, "/channels/%llu/messages/%llu",
             (unsigned long long)channel_id, (unsigned long long)message_id);
    json_t *resp = NULL;
    int rc;
    if (n_files) {
        /* multipart replaces attachments */
        json_t *pl = json_pack("{s:s}", "content", content ? content : "");
        char *s = json_dumps(pl, JSON_COMPACT);
        json_decref(pl);
        rc = rest_multipart(c, "PATCH", path, s, files, n_files, &resp);
        free(s);
    } else {
        /* explicit empty attachments list: clears previously attached
         * files (a content-only PATCH would keep them) */
        json_t *pl = json_pack("{s:s, s:[]}", "content", content ? content : "",
                               "attachments");
        char *s = json_dumps(pl, JSON_COMPACT);
        json_decref(pl);
        rc = rest_json(c, "PATCH", path, s, &resp);
        free(s);
    }
    json_decref(resp);
    return rc;
}

int discord_delete_message(discord_client_t *c, uint64_t channel_id,
                           uint64_t message_id) {
    char path[160];
    snprintf(path, sizeof path, "/channels/%llu/messages/%llu",
             (unsigned long long)channel_id, (unsigned long long)message_id);
    return rest_json(c, "DELETE", path, NULL, NULL);
}

int discord_trigger_typing(discord_client_t *c, uint64_t channel_id) {
    char path[128];
    snprintf(path, sizeof path, "/channels/%llu/typing",
             (unsigned long long)channel_id);
    return rest_json(c, "POST", path, NULL, NULL);
}

int discord_channel_messages(discord_client_t *c, uint64_t channel_id, int n,
                             disc_message_t **out, size_t *out_n) {
    *out = NULL;
    if (out_n)
        *out_n = 0;
    if (n < 1)
        n = 1;
    if (n > 100)
        n = 100;
    char path[160];
    snprintf(path, sizeof path, "/channels/%llu/messages?limit=%d",
             (unsigned long long)channel_id, n);
    char url[1024];
    snprintf(url, sizeof url, "%s%s", DISCORD_API, path);
    json_t *resp = NULL;
    if (rest_perform(c, "GET", url, NULL, NULL, &resp) != 0)
        return -1;
    if (!json_is_array(resp)) {
        json_decref(resp);
        return -1;
    }
    size_t count = json_array_size(resp);
    disc_message_t *arr = xmalloc((count ? count : 1) * sizeof *arr);
    if (!arr) {
        json_decref(resp);
        return -1;
    }
    memset(arr, 0, (count ? count : 1) * sizeof *arr);
    for (size_t i = 0; i < count; i++) {
        if (disc_parse_message(json_array_get(resp, i), &arr[i]) != 0) {
            /* skip malformed entries, keep the rest */
            memset(&arr[i], 0, sizeof arr[i]);
        }
    }
    json_decref(resp);
    *out = arr;
    if (out_n)
        *out_n = count;
    return 0;
}

void disc_message_array_free(disc_message_t *arr, size_t n) {
    if (!arr)
        return;
    for (size_t i = 0; i < n; i++)
        disc_message_free(&arr[i]);
    free(arr);
}

int discord_get_message(discord_client_t *c, uint64_t channel_id,
                        uint64_t message_id, disc_message_t *out) {
    memset(out, 0, sizeof *out);
    char path[160];
    snprintf(path, sizeof path, "/channels/%llu/messages/%llu",
             (unsigned long long)channel_id, (unsigned long long)message_id);
    char url[1024];
    snprintf(url, sizeof url, "%s%s", DISCORD_API, path);
    json_t *resp = NULL;
    if (rest_perform(c, "GET", url, NULL, NULL, &resp) != 0)
        return -1;
    int rc = disc_parse_message(resp, out);
    json_decref(resp);
    return rc;
}

int discord_get_user(discord_client_t *c, uint64_t user_id, char **name_out,
                     char **global_out) {
    *name_out = NULL;
    if (global_out)
        *global_out = NULL;
    char path[128];
    if (user_id == 0)
        snprintf(path, sizeof path, "/users/@me");
    else
        snprintf(path, sizeof path, "/users/%llu", (unsigned long long)user_id);
    char url[1024];
    snprintf(url, sizeof url, "%s%s", DISCORD_API, path);
    json_t *resp = NULL;
    if (rest_perform(c, "GET", url, NULL, NULL, &resp) != 0)
        return -1;
    *name_out = get_str(resp, "username");
    if (global_out)
        *global_out = get_str(resp, "global_name");
    json_decref(resp);
    if (!*name_out)
        *name_out = xstrdup("someone");
    return *name_out ? 0 : -1;
}

int discord_create_dm(discord_client_t *c, uint64_t user_id, uint64_t *out_ch) {
    json_t *pl = json_pack("{s:s}", "recipient_id", "");
    char id[32];
    snprintf(id, sizeof id, "%llu", (unsigned long long)user_id);
    json_object_set_new(pl, "recipient_id", json_string(id));
    char *s = json_dumps(pl, JSON_COMPACT);
    json_decref(pl);
    json_t *resp = NULL;
    int rc = rest_json(c, "POST", "/users/@me/channels", s, &resp);
    free(s);
    if (rc != 0)
        return -1;
    *out_ch = get_snowflake(resp, "id");
    json_decref(resp);
    return *out_ch ? 0 : -1;
}

int discord_interaction_defer(discord_client_t *c, const disc_interaction_t *in,
                              bool ephemeral) {
    char path[256];
    snprintf(path, sizeof path, "/interactions/%llu/%s/callback",
             (unsigned long long)in->id, in->token);
    /* type 5 = DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE, flags 64 = ephemeral */
    char body[128];
    if (ephemeral)
        snprintf(body, sizeof body,
                 "{\"type\":5,\"data\":{\"flags\":64}}");
    else
        snprintf(body, sizeof body, "{\"type\":5}");
    return rest_json(c, "POST", path, body, NULL);
}

int discord_interaction_reply(discord_client_t *c, const disc_interaction_t *in,
                              const char *content, bool ephemeral) {
    char path[256];
    snprintf(path, sizeof path, "/interactions/%llu/%s/callback",
             (unsigned long long)in->id, in->token);
    json_t *pl;
    if (ephemeral)
        pl = json_pack("{s:i, s:{s:s, s:i}}", "type", 4, "data", "content",
                       content ? content : "", "flags", 64);
    else
        pl = json_pack("{s:i, s:{s:s}}", "type", 4, "data", "content",
                       content ? content : "");
    char *s = json_dumps(pl, JSON_COMPACT);
    json_decref(pl);
    int rc = rest_json(c, "POST", path, s, NULL);
    free(s);
    return rc;
}

int discord_interaction_followup(discord_client_t *c,
                                 const disc_interaction_t *in,
                                 const char *content, bool ephemeral,
                                 const disc_file_t *files, size_t n_files,
                                 uint64_t *out_id) {
    char path[320];
    snprintf(path, sizeof path, "/webhooks/%llu/%s?wait=true",
             (unsigned long long)discord_app_id(c), in->token);
    json_t *pl;
    if (ephemeral)
        pl = json_pack("{s:s, s:i}", "content", content ? content : "",
                       "flags", 64);
    else
        pl = json_pack("{s:s}", "content", content ? content : "");
    char *s = json_dumps(pl, JSON_COMPACT);
    json_decref(pl);
    json_t *resp = NULL;
    int rc;
    if (n_files)
        rc = rest_multipart(c, "POST", path, s, files, n_files, &resp);
    else
        rc = rest_json(c, "POST", path, s, &resp);
    free(s);
    if (rc == 0 && out_id)
        *out_id = resp_id(resp);
    json_decref(resp);
    return rc;
}

int discord_register_commands(discord_client_t *c, uint64_t app_id,
                              const char *commands_json) {
    char path[128];
    snprintf(path, sizeof path, "/applications/%llu/commands",
             (unsigned long long)app_id);
    return rest_json(c, "PUT", path, commands_json, NULL);
}

int discord_guild_active_threads(discord_client_t *c, uint64_t guild_id,
                                 uint64_t parent_channel, uint64_t *ids,
                                 size_t cap, size_t *n_out) {
    *n_out = 0;
    char path[128];
    snprintf(path, sizeof path, "/guilds/%llu/threads/active",
             (unsigned long long)guild_id);
    char url[1024];
    snprintf(url, sizeof url, "%s%s", DISCORD_API, path);
    json_t *resp = NULL;
    if (rest_perform(c, "GET", url, NULL, NULL, &resp) != 0)
        return -1;
    json_t *threads = json_object_get(resp, "threads");
    if (json_is_array(threads)) {
        for (size_t i = 0; i < json_array_size(threads) && *n_out < cap; i++) {
            json_t *t = json_array_get(threads, i);
            uint64_t parent = get_snowflake(t, "parent_id");
            if (parent == parent_channel)
                ids[(*n_out)++] = get_snowflake(t, "id");
        }
    }
    json_decref(resp);
    return 0;
}

/* End of REST client. All endpoint URLs are built inline at call sites. */
