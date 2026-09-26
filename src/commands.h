#ifndef ARTIXY_COMMANDS_H
#define ARTIXY_COMMANDS_H

#include "bot.h"
#include "discord.h"
#include <stddef.h>

/* Full help text (mirrors the Rust/Go builds). */
extern const char *artixy_help_text;

/*
 * Parse a "/cmd args" or ";cmd args" prefix invocation.
 * On success returns 0 with name_out set (lowercase, "purgereplies"
 * normalized to "purge_replies") and *arg_out pointing at the trimmed
 * remainder (into content, may be ""). Returns -1 when not a command.
 */
int cmd_parse_prefix(const char *content, char *name_out, size_t name_cap,
                     const char **arg_out);

/* Build the bulk-overwrite slash command JSON array. Caller frees. */
char *commands_json(void);

/* Handlers invoked from main.c wiring (st = live bot state). */
void handle_prefix(discord_client_t *c, bot_state_t *st,
                   const disc_message_t *m);
void handle_interaction(discord_client_t *c, bot_state_t *st,
                        const disc_interaction_t *in);

/* Per-command handlers (shared prefix/slash). arg may be "". */
void cmd_help(cmd_ctx_t *ctx, const char *arg);
void cmd_ps(cmd_ctx_t *ctx, const char *arg);
void cmd_status(cmd_ctx_t *ctx, const char *arg);
void cmd_start(cmd_ctx_t *ctx, const char *arg);
void cmd_stop(cmd_ctx_t *ctx, const char *arg);
void cmd_restart_vm(cmd_ctx_t *ctx, const char *arg);
void cmd_info(cmd_ctx_t *ctx, const char *arg);
void cmd_shell(cmd_ctx_t *ctx, const char *arg);
void cmd_botrestart(cmd_ctx_t *ctx, const char *arg);
void cmd_user(cmd_ctx_t *ctx, const char *arg, uint64_t target_id,
              const char *target_name);
void cmd_admin(cmd_ctx_t *ctx, const char *arg, uint64_t target_id);
void cmd_notify(cmd_ctx_t *ctx, const char *arg);
void cmd_purge_replies(cmd_ctx_t *ctx, const char *target, int limit);
void cmd_warmode(cmd_ctx_t *ctx, const char *arg);
void cmd_sayas(cmd_ctx_t *ctx, const char *message, const char *reply_to,
               const disc_attachment_t *atts, size_t n_atts);
void cmd_send(cmd_ctx_t *ctx, const char *path);
void cmd_upload(cmd_ctx_t *ctx, const char *dir, const char *file_url,
                const char *file_name, uint64_t file_size);
/* Stubs until Phase 5/6 (still parse + auth correctly). */
void cmd_run(cmd_ctx_t *ctx, const char *arg);
void cmd_ai(cmd_ctx_t *ctx, const char *arg);
void cmd_ai_opts(cmd_ctx_t *ctx, bool has_enabled, bool enabled,
                 const char *model, const char *prompt, bool has_forget,
                 bool forget, bool has_think, bool think);
void cmd_ask(cmd_ctx_t *ctx, const char *arg);
/* Ambient @artixy mention handling. Returns true if it answered. */
bool ambient_mention(discord_client_t *c, bot_state_t *st,
                     const disc_message_t *m);

/* Slash option extraction (implemented in commands_admin.c). */
const char *cmd_opt_str(const disc_interaction_t *in, const char *name);
bool cmd_opt_bool(const disc_interaction_t *in, const char *name,
                  bool *present);
long long cmd_opt_int(const disc_interaction_t *in, const char *name,
                      bool *present);
uint64_t cmd_opt_user(const disc_interaction_t *in, const char *name,
                      const char **username_out);
const disc_attachment_t *cmd_opt_attachment(const disc_interaction_t *in,
                                            const char *name);

#endif
