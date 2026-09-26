#ifndef ARTIXY_EVENTS_H
#define ARTIXY_EVENTS_H

#include "bot.h"
#include "discord.h"

/* Full ambient message pipeline (ported 1:1 from events.rs):
 * war-mode blocked guards, prefix commands, @artixy mentions,
 * live-session reply input, sayas-auto, .ar suffix, war-mode boo. */
void events_handle_message(discord_client_t *c, bot_state_t *st,
                           const disc_message_t *m);

/* "boo" (case-insensitive, alphanumeric-tokenized) detector. */
bool events_is_boo(const char *s);
/* Trimmed text before a trailing ".ar", or NULL when absent/empty. */
char *events_artixy_text(const char *content);

#endif
