/* artixy — Artix VM Discord bot (C port of the Rust original). */
#include "bot.h"
#include "commands.h"
#include "config.h"
#include "discord.h"
#include "events.h"
#include "live.h"
#include "util.h"

#include <curl/curl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static discord_client_t *g_client = NULL;

static void on_sigint(int sig) {
    (void)sig;
    if (g_client)
        discord_stop(g_client);
}

static void on_message(discord_client_t *c, const disc_message_t *m, void *ud) {
    bot_state_t *st = ud;
    events_handle_message(c, st, m);
}

static void on_interaction(discord_client_t *c, const disc_interaction_t *in,
                           void *ud) {
    bot_state_t *st = ud;
    if (in->type != 2)
        return;
    handle_interaction(c, st, in);
}

/* Boot art (ported 1:1 from the Rust/Go builds). */
static const char boot_art[] =
    "          .        :-------:\n"
    "        ^/ \\^      :Im here:\n"
    "        \xe2\x97\x8f   \xe2\x97\x8f     <:-------:\n"
    "       /  \xcf\x89  \\\n"
    "      /_/   \\_\\";

static void on_ready(discord_client_t *c, uint64_t bot_id, uint64_t app_id,
                     void *ud) {
    bot_state_t *st = ud;
    fprintf(stderr, "artixy: logged in (bot=%llu app=%llu)\n",
            (unsigned long long)bot_id, (unsigned long long)app_id);
    char vmname[256] = "";
    {
        pthread_rwlock_rdlock(&st->mu);
        snprintf(vmname, sizeof vmname, "%s", st->vm);
        pthread_rwlock_unlock(&st->mu);
    }
    live_cleanup_stale(vmname);
    char *cmds = commands_json();
    if (cmds) {
        if (discord_register_commands(c, app_id, cmds) == 0)
            fprintf(stderr, "artixy: slash commands registered\n");
        free(cmds);
    }
    pthread_rwlock_rdlock(&st->mu);
    bool has = st->has_notify;
    uint64_t ch = st->notify_channel;
    pthread_rwlock_unlock(&st->mu);
    if (has) {
        char boot[512];
        snprintf(boot, sizeof boot, "```\n%s\n```", boot_art);
        discord_send_message(c, ch, boot, NULL, 0, NULL);
    }
}

static void usage(void) {
    printf("artixy — run with no args to start the Discord bot.\n");
}

int main(int argc, char **argv) {
    if (argc > 1 && (strcmp(argv[1], "help") == 0 ||
                     strcmp(argv[1], "--help") == 0 || strcmp(argv[1], "-h") == 0)) {
        usage();
        return 0;
    }
    bool check_rest = argc > 1 && strcmp(argv[1], "--check-rest") == 0;

    config_ensure_template();

    file_config_t cfg;
    int rc = config_load(&cfg);
    if (rc == -2) {
        fprintf(stderr, "artixy: refusing to start with an unparsable config\n");
        return 1;
    }
    if (rc != 0) {
        fprintf(stderr, "artixy: out of memory while loading config\n");
        return 1;
    }

    const char *token = cfg.discord_token;
    if (!token) {
        token = getenv("DISCORD_TOKEN");
        if (token && *token)
            fprintf(stderr, "artixy: no discord_token in %s, using DISCORD_TOKEN env\n",
                    config_path());
    }
    if (!token || !*token) {
        fprintf(stderr, "artixy: error: set discord_token in %s or DISCORD_TOKEN env\n",
                config_path());
        file_config_free(&cfg);
        return 1;
    }

    uint64_t owner = 0;
    if (cfg.has_owner_id) {
        owner = cfg.owner_id;
    } else {
        const char *env = getenv("OWNER_ID");
        if (!env || sscanf(env, "%lu", (unsigned long *)&owner) != 1 || owner == 0) {
            fprintf(stderr, "artixy: error: set owner_id in %s or OWNER_ID env\n",
                    config_path());
            file_config_free(&cfg);
            return 1;
        }
    }
    if (!cfg.vm_name)
        fprintf(stderr,
                "artixy: warning: vm_name not set in %s — VM commands will "
                "reply with a friendly error until you set it\n",
                config_path());

    fprintf(stderr, "artixy: config ok (owner=%lu vm=%s)\n",
            (unsigned long)owner, cfg.vm_name ? cfg.vm_name : "(unset)");

    curl_global_init(CURL_GLOBAL_DEFAULT);

    discord_client_t *c = discord_new(token);
    if (!c) {
        fprintf(stderr, "artixy: out of memory\n");
        file_config_free(&cfg);
        return 1;
    }
    if (check_rest) {
        /* Read-only live probe: proves REST auth/TLS/JSON without
         * touching the gateway (which would disconnect the running bot). */
        char *name = NULL, *global = NULL;
        int ok = discord_get_user(c, 0, &name, &global);
        if (ok == 0)
            printf("artixy: REST ok as %s\n", name ? name : "?");
        else
            fprintf(stderr, "artixy: REST check failed\n");
        free(name);
        free(global);
        discord_free(c);
        file_config_free(&cfg);
        curl_global_cleanup();
        return ok != 0;
    }
    g_client = c;
    signal(SIGINT, on_sigint);
    signal(SIGTERM, on_sigint);
    bot_state_t state;
    if (bot_state_init(&state, &cfg) != 0) {
        fprintf(stderr, "artixy: out of memory\n");
        file_config_free(&cfg);
        discord_free(c);
        return 1;
    }
    file_config_free(&cfg);
    if (bot_state_watch(&state) != 0)
        fprintf(stderr, "artixy: warning: config watcher failed to start\n");
    live_map_t *live = live_new();
    if (!live)
        fprintf(stderr, "artixy: warning: live map alloc failed\n");
    state.live = live;
    discord_on_message(c, on_message, &state);
    discord_on_interaction(c, on_interaction, &state);
    discord_on_ready(c, on_ready, &state);

    rc = discord_run(c);
    if (rc != 0)
        fprintf(stderr, "artixy: gateway exited fatally (bad token?)\n");

    discord_free(c);
    live_free(live);
    bot_state_free(&state);
    curl_global_cleanup();
    return rc != 0;
}
