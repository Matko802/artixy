#ifndef ARTIXY_BOT_H
#define ARTIXY_BOT_H

#include "config.h"
#include "discord.h"

#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>

/* Live session map (live.h); forward-declared to avoid an include cycle. */
typedef struct live_map live_map_t;

/* Live shared state (hot-reloaded from config.jsonc). */
typedef struct {
    pthread_rwlock_t mu;
    uint64_t owner;
    uint64_t *users; /* managers */
    size_t n_users;
    strmap_t linux; /* discord id string -> linux username */
    uint64_t *blocked;
    size_t n_blocked;
    uint64_t *admins;
    size_t n_admins;
    char vm[256];
    char libvirt_uri[1024];
    /* settings */
    bool has_notify;
    uint64_t notify_channel;
    bool war_mode;
    bool sayas_enabled;
    bool ai_enabled;
    char ai_model[160];
    char ollama_host[512];
    char *ai_prompt; /* malloc'd */
    double ai_temperature;
    bool ai_think;
    strmap_t shells; /* discord id string -> fish|bash */
    live_map_t *live; /* not owned; set by main (may be NULL in tests) */
} bot_state_t;

int bot_state_init(bot_state_t *st, const file_config_t *cfg);
void bot_state_free(bot_state_t *st);

/* Replace live state from a freshly loaded file config. */
void bot_state_apply(bot_state_t *st, const file_config_t *cfg);

/* Background hot-reload watcher (2s poll). on_change (may be NULL) runs
 * after every successful apply. Returns 0 on thread start. */
int bot_state_watch(bot_state_t *st, void (*on_change)(const file_config_t *cfg));

/* Access checks (by string or numeric id). */
bool bot_is_blocked(bot_state_t *st, uint64_t id);
bool bot_is_authed(bot_state_t *st, uint64_t id);
bool bot_is_owner(bot_state_t *st, uint64_t id);
bool bot_is_elevated(bot_state_t *st, uint64_t id);
/* Linked linux username, "" when none. Returned pointer valid until next
 * state write; copy it if you need it across awaits (threads). */
const char *bot_linked_user(bot_state_t *st, uint64_t id);

/* Persist live access lists/settings/shells back to config.jsonc.
 * Returns 0 on success, -1 on error (refuses to clobber parse errors). */
int bot_persist(bot_state_t *st);

/* Unified prefix/slash invocation context. */
typedef struct {
    discord_client_t *dc;
    bot_state_t *st;
    bool is_slash;
    const disc_message_t *msg;       /* prefix only */
    const disc_interaction_t *inter; /* slash only */
    uint64_t channel_id;
    uint64_t guild_id;
    uint64_t author_id;
    char author_name[128];
} cmd_ctx_t;

/* id parsing helpers */
uint64_t parse_u64(const char *s);
/* <@123>, <@!123> or raw id -> numeric id. Returns 0 when invalid. */
uint64_t parse_target_id(const char *s);
/* message id or full link -> (channel, message). current used for bare ids. */
bool parse_message_ref(const char *s, uint64_t current_channel,
                       uint64_t *ch_out, uint64_t *msg_out);

/* Reply helpers. Slash path assumes the interaction was already deferred. */
void ctx_reply(cmd_ctx_t *ctx, const char *text);
void ctx_reply_ephemeral(cmd_ctx_t *ctx, const char *text);
void ctx_reply_files(cmd_ctx_t *ctx, const char *text, const disc_file_t *files,
                     size_t n);
void ctx_deny(cmd_ctx_t *ctx, const char *text);
bool ctx_need_auth(cmd_ctx_t *ctx);
bool ctx_need_public(cmd_ctx_t *ctx);
/* VM name or "" (with the friendly not-configured reply). */
const char *ctx_require_vm(cmd_ctx_t *ctx);

/* Validators (ported 1:1, unit-tested). */
bool bot_sensitive_send_name(const char *name);
bool bot_share_has_dot(const char *rel);
bool bot_normalize_guest_dir(const char *dir, char *out, size_t n);
bool bot_upload_allowed(const char *normalized, const char *linked);
void bot_sh_escape(const char *s, char *out, size_t n);
/* discord display name -> linux username candidate. "" when unusable. */
void bot_sanitize_discord_name(const char *s, char *out, size_t n);
bool bot_valid_runas(const char *name);
/* sudoers setup script for user. Returns malloc'd string or NULL. */
char *bot_sudoers_script(const char *user);

/* codeblock / plain-tail text formatting (ported 1:1). */
void bot_codeblock(const char *s, char *out, size_t n);
void bot_plain_tail(const char *body, char *out, size_t n);
/* strip SGR escape sequences. Returns malloc'd string. */
char *bot_strip_sgr(const char *s);

/* Download URL (attachments) up to max_bytes. Returns 0 with malloc'd *data. */
int bot_download(const char *url, size_t max_bytes, unsigned char **data,
                 size_t *len_out);

/* Download message attachments (each <=25MB, name-sanitized).
 * Returns parallel files[]/bufs[]; free with bot_free_dl_files. */
int bot_download_atts(const disc_attachment_t *atts, size_t n,
                      disc_file_t **out_files, size_t *out_n, char ***out_bufs);
void bot_free_dl_files(disc_file_t *files, size_t n, char **bufs);

#endif
