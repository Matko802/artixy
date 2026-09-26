#ifndef ARTIXY_DISCORD_H
#define ARTIXY_DISCORD_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Discord API v10 base. */
#define DISCORD_API "https://discord.com/api/v10"

/* Gateway intents: GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT */
#define DISCORD_INTENTS (1u | (1u << 9) | (1u << 12) | (1u << 15))

/* Attachment descriptor (received). */
typedef struct {
    uint64_t id;
    char *filename;
    char *url;
    uint64_t size;
} disc_attachment_t;

/* Outgoing file. */
typedef struct {
    const char *name;
    const void *data;
    size_t len;
} disc_file_t;

/* MESSAGE_CREATE payload (subset we use). */
typedef struct {
    uint64_t id;
    uint64_t channel_id;
    uint64_t guild_id; /* 0 in DMs */
    uint64_t author_id;
    char *author_name;
    char *global_name; /* may be NULL */
    char *member_nick; /* may be NULL */
    bool author_bot;
    bool from_webhook;
    char *content;
    uint64_t *mentions;
    size_t n_mentions;
    disc_attachment_t *attachments;
    size_t n_attachments;
    /* Reply target, if any: */
    bool has_reference;
    uint64_t ref_channel_id;
    uint64_t ref_message_id;
    /* Embedded referenced message (may be partial/NULL content). */
    bool has_ref_msg;
    uint64_t ref_msg_id;
    uint64_t ref_msg_author_id;
    char *ref_msg_author_name;
    char *ref_msg_content;
} disc_message_t;

void disc_message_free(disc_message_t *m);
/* Deep copy (for handing events to worker threads). Returns 0 on success. */
int disc_message_clone(const disc_message_t *src, disc_message_t *dst);

/* Interaction option value. */
typedef struct {
    char *name;
    int type; /* 3 string, 4 int, 5 bool, 6 user, 7 channel, 8 role, 11 attachment */
    char *str_val;   /* owned, for string/choice */
    long long int_val;
    bool bool_val;
    uint64_t user_id; /* for type 6 (from value or resolved) */
    uint64_t attachment_id;
} disc_option_t;

/* INTERACTION_CREATE payload (application command subset). */
typedef struct {
    uint64_t id;
    char *token;
    int type; /* 2 = application command */
    uint64_t channel_id;
    uint64_t guild_id; /* 0 in DMs */
    uint64_t author_id;
    char *author_name;
    char *member_nick; /* may be NULL */
    char *command;     /* root command name */
    disc_option_t *options;
    size_t n_options;
    /* Resolved users/attachments (only what handlers need). */
    struct {
        uint64_t id;
        char *username;
    } *resolved_users;
    size_t n_resolved_users;
    disc_attachment_t *resolved_attachments;
    size_t n_resolved_attachments;
} disc_interaction_t;

void disc_interaction_free(disc_interaction_t *in);
/* Deep copy (for handing events to worker threads). Returns 0 on success. */
int disc_interaction_clone(const disc_interaction_t *src,
                           disc_interaction_t *dst);

/*
 * Parse event JSON (jansson objects, already stripped of envelope).
 * Return 0 on success, -1 on malformed input (message left initialized).
 */
int disc_parse_message(void *json_obj, disc_message_t *out);
int disc_parse_interaction(void *json_obj, disc_interaction_t *out);

typedef struct discord_client discord_client_t;

typedef void (*disc_msg_cb)(discord_client_t *c, const disc_message_t *m, void *ud);
typedef void (*disc_interaction_cb)(discord_client_t *c, const disc_interaction_t *in, void *ud);
typedef void (*disc_ready_cb)(discord_client_t *c, uint64_t bot_id, uint64_t app_id, void *ud);

discord_client_t *discord_new(const char *token);
void discord_free(discord_client_t *c);
void discord_on_message(discord_client_t *c, disc_msg_cb cb, void *ud);
void discord_on_interaction(discord_client_t *c, disc_interaction_cb cb, void *ud);
void discord_on_ready(discord_client_t *c, disc_ready_cb cb, void *ud);

/*
 * Block serving the gateway with auto-reconnect. Returns only on fatal
 * error (e.g. 401 bad token) or discord_stop(). Returns 0 if stopped,
 * -1 on fatal error.
 */
int discord_run(discord_client_t *c);
void discord_stop(discord_client_t *c);

/* Self user/app ids once READY arrived (0 before). */
uint64_t discord_bot_id(discord_client_t *c);
uint64_t discord_app_id(discord_client_t *c);

/* ---- REST helpers (all return 0 on success) ---- */

/* channel message; files may be NULL/0. Returns new message id in *out_id. */
int discord_send_message(discord_client_t *c, uint64_t channel_id,
                         const char *content, const disc_file_t *files,
                         size_t n_files, uint64_t *out_id);
/* reply to a message (message_reference). */
int discord_send_reply(discord_client_t *c, uint64_t channel_id,
                       uint64_t reply_to, const char *content,
                       const disc_file_t *files, size_t n_files,
                       uint64_t *out_id);
int discord_edit_message(discord_client_t *c, uint64_t channel_id,
                         uint64_t message_id, const char *content,
                         const disc_file_t *files, size_t n_files);
int discord_delete_message(discord_client_t *c, uint64_t channel_id,
                           uint64_t message_id);
int discord_trigger_typing(discord_client_t *c, uint64_t channel_id);
/* last n<=100 messages, newest first. Caller frees array + contents. */
int discord_channel_messages(discord_client_t *c, uint64_t channel_id, int n,
                             disc_message_t **out, size_t *out_n);
void disc_message_array_free(disc_message_t *arr, size_t n);
int discord_get_message(discord_client_t *c, uint64_t channel_id,
                        uint64_t message_id, disc_message_t *out);
/* username and global_name of a user; caller frees both outputs.
 * user_id 0 means @me (the bot itself). */
int discord_get_user(discord_client_t *c, uint64_t user_id, char **name_out,
                     char **global_out);
/* DM channel id for a user (creates if needed). */
int discord_create_dm(discord_client_t *c, uint64_t user_id, uint64_t *out_ch);

/* Interaction acknowledgements. defer_ephemeral picks "thinking, only you". */
int discord_interaction_defer(discord_client_t *c, const disc_interaction_t *in,
                              bool ephemeral);
int discord_interaction_reply(discord_client_t *c,
                              const disc_interaction_t *in, const char *content,
                              bool ephemeral);
/* Followup to a deferred interaction. Returns followup message id. */
int discord_interaction_followup(discord_client_t *c,
                                 const disc_interaction_t *in,
                                 const char *content, bool ephemeral,
                                 const disc_file_t *files, size_t n_files,
                                 uint64_t *out_id);

/* Bulk-overwrite global application commands from a JSON array string.
 * Used once at READY. */
int discord_register_commands(discord_client_t *c, uint64_t app_id,
                              const char *commands_json);

/* Active threads of a guild parented to parent_channel (up to cap).
 * Returns 0 with *n_out thread ids. */
int discord_guild_active_threads(discord_client_t *c, uint64_t guild_id,
                                 uint64_t parent_channel, uint64_t *ids,
                                 size_t cap, size_t *n_out);

#endif
