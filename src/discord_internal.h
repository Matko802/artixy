/* Internal cross-module accessors (not part of the public API). */
#ifndef ARTIXY_DISCORD_INTERNAL_H
#define ARTIXY_DISCORD_INTERNAL_H

#include "discord.h"

#include <stdbool.h>
#include <stdint.h>

/* Client state. Defined here so rest.c (REST) and gateway.c (gateway loop)
 * share it. */
struct discord_client {
    char *token;
    uint64_t bot_id;
    uint64_t app_id;
    volatile bool running;
    disc_msg_cb on_message;
    disc_interaction_cb on_interaction;
    disc_ready_cb on_ready;
    void *msg_ud;
    void *interaction_ud;
    void *ready_ud;
};

/* rest.c accessors used by gateway.c */
const char *discord_token(const discord_client_t *c);
void discord_set_ids(discord_client_t *c, uint64_t bot, uint64_t app);
void discord_emit_message(discord_client_t *c, const disc_message_t *m);
void discord_emit_interaction(discord_client_t *c, const disc_interaction_t *in);
void discord_emit_ready(discord_client_t *c);
bool discord_is_running(discord_client_t *c);

#endif
