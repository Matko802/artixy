#ifndef ARTIXY_AI_H
#define ARTIXY_AI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

const char *ai_default_model(void);
const char *ai_default_host(void);
/* Effective Ollama host: OLLAMA_HOST/OLLAMA_URL env wins, then configured. */
void ai_resolve_host(const char *configured, char *out, size_t n);
bool ai_valid_model_name(const char *s);
double ai_clamp_temperature(double t);
double ai_default_temperature(void);

/* Reply cleaning (pure, unit-tested). Caller frees returns. */
char *ai_clean_reply(const char *text);
char *ai_strip_leading_speaker(const char *text, const char **names,
                               size_t n_names);
char *ai_strip_meta_preamble(const char *text);
char *ai_sanitize_reply(const char *raw, const char **names, size_t n_names);
char *ai_speaker_tag(const char *raw);
char *ai_strip_mention(const char *content, uint64_t bot_id);
bool ai_mentions_name(const char *content);
char *ai_strip_name(const char *content);
/* Split into <=1900-char chunks, max 4. Returns malloc'd array + strings. */
char **ai_chunk_reply(const char *s, size_t *n_out);
void ai_chunks_free(char **chunks, size_t n);

bool ai_is_rate_limit_err(const char *s);
bool ai_is_api_full_err(const char *s);
const char *ai_api_full_message(void);

/* History item for transcript building. */
typedef struct {
    const char *role;
    const char *content;
} ai_hist_item_t;

char *ai_build_transcript(const ai_hist_item_t *past, size_t n,
                          const char *tagged, const char **names,
                          size_t n_names);
bool ai_stale_history_line(const char *s);

/* Per-channel conversation history (thread-safe). */
void ai_history_push(uint64_t channel, const char *role, const char *content);
void ai_history_clear(uint64_t channel);
void ai_history_clear_all(void);
void ai_record_artixy(uint64_t channel, const char *text);
size_t ai_history_count(uint64_t channel); /* for tests/monitoring */

/*
 * Full chat turn: builds transcript from history, calls Ollama, sanitizes,
 * records both sides. Returns 0 with *out malloc'd reply; -1 on error.
 */
int ai_chat(const char *host, const char *model, uint64_t channel,
            const char *speaker, const char *prompt, const char *system,
            double temperature, bool think, char **out);
/* One-sentence glitch explanation (never fails; malloc'd). */
char *ai_glitch_text(const char *host, const char *model,
                     const char *system_prompt, double temperature, bool think);
/* 1 = present, 0 = absent, -1 = unknown (network error). */
int ai_model_present(const char *host, const char *model);

#endif
