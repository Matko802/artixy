#ifndef ARTIXY_CONFIG_H
#define ARTIXY_CONFIG_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Simple string->string map for [linux] and [shells] tables. */
typedef struct {
    char **keys;
    char **vals;
    size_t len;
    size_t cap;
} strmap_t;

void strmap_init(strmap_t *m);
void strmap_free(strmap_t *m);
/* Returns 0 on success, -1 on allocation failure. Replaces existing key. */
int strmap_set(strmap_t *m, const char *key, const char *val);
/* Returns value or NULL. */
const char *strmap_get(const strmap_t *m, const char *key);
bool strmap_equal(const strmap_t *a, const strmap_t *b);

/* On-disk ~/.config/artixy/config.jsonc schema. */
typedef struct {
    bool has_owner_id;
    uint64_t owner_id;
    uint64_t *blocked_ids;
    size_t n_blocked;
    char *discord_token; /* NULL when absent/empty */
    char *vm_name;       /* NULL when absent/empty */
    bool war_mode;
    bool sayas_enabled;
    bool has_notify_channel;
    uint64_t notify_channel;
    uint64_t *managers;
    size_t n_managers;
    uint64_t *admin_ids;
    size_t n_admins;
    strmap_t linux;
    strmap_t shells;
    bool ai_enabled;
    char *ai_model;
    char *ollama_host;
    char *ai_prompt;
    double ai_temperature;
    bool ai_think;
} file_config_t;

void file_config_init(file_config_t *c);
void file_config_free(file_config_t *c);

/* Resolved config path. Returned pointer is valid until next call. */
const char *config_path(void);

/* Create a commented template at config_path() if missing. */
void config_ensure_template(void);

/* Strip JSONC (comments + trailing commas) into a fresh NUL-terminated
 * buffer. Returns NULL on allocation failure. Exposed for unit tests. */
char *config_strip_jsonc(const char *src, size_t len);

/*
 * Load and normalize config_path().
 * Returns 0 on success (missing file -> defaults, like the Rust/Go builds).
 * Returns -1 on I/O error, -2 on JSON parse error (caller keeps old settings).
 */
int config_load(file_config_t *out);

/* Defaults shared with normalization. */
const char *config_default_model(void);
const char *config_default_host(void);
double config_clamp_temperature(double t);
bool config_valid_model_name(const char *s);

#endif
