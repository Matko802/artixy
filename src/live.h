#ifndef ARTIXY_LIVE_H
#define ARTIXY_LIVE_H

#include "discord.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct live_map live_map_t;

live_map_t *live_new(void);
void live_free(live_map_t *lm);

/*
 * Start a `run` session. The ack message (already posted by the caller)
 * becomes the live-updating message. Previous session of the same
 * (channel, author) is torn down. Returns immediately; work continues on
 * a detached thread.
 */
void live_begin_run(discord_client_t *dc, live_map_t *lm, uint64_t channel_id,
                    uint64_t ack_msg_id, uint64_t author_id,
                    const char *author_name, const char *vm, const char *cmd,
                    const char *runas /* "" = none */, bool scrub_ip);

/*
 * Find the session whose live message is msg_id in channel_id.
 * On hit returns true with *author_out set and *fifo_out malloc'd
 * (or NULL when the session has no fifo). Callers free *fifo_out.
 */
bool live_session_for_msg(live_map_t *lm, uint64_t channel_id, uint64_t msg_id,
                          uint64_t *author_out, char **fifo_out);

/* Append payload to the guest fifo. True when delivered (rc 0). */
bool live_forward_input(const char *vm, const char *fifo, const char *runas,
                        const char *payload);

/* Remove guest-side stale live files (call once at startup). */
void live_cleanup_stale(const char *vm);

/* Pure helpers (unit-tested). Expand ;key sequences + \n escapes. */
char *live_expand_typed_input(const char *text);
/* Build the guest runner script. runas "" = none. Caller frees. */
char *live_build_runner(const char *shell, const char *b64, const char *out_f,
                        const char *code_f, const char *input,
                        const char *runas);
char *live_mkfifo_script(const char *path, const char *runas);

#endif
