/* artixy — Artix VM Discord bot (C port of the Rust original). */
#include "bot.h"
#include "commands.h"
#include "config.h"
#include "discord.h"
#include "events.h"
#include "live.h"
#include "util.h"
#include "vm.h"

#include <curl/curl.h>
#include <pthread.h>
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

/* Mirror stderr to artixy.log in the working directory (bot mode only).
 * Systems without a journal (plain Pi OS) would otherwise lose all logs. */
static void setup_file_log(void) {
    FILE *f = fopen("artixy.log", "a");
    if (!f) {
        fprintf(stderr, "artixy: warning: cannot open artixy.log\n");
        return;
    }
    setvbuf(f, NULL, _IOLBF, 0);
    dup2(fileno(f), STDERR_FILENO);
    /* keep f open for process lifetime */
    (void)f;
}

static void on_config_change(const file_config_t *cfg) {
    vm_set_connect_uri(cfg->libvirt_uri);
}

/*
 * Event handlers run on detached worker threads, NOT the gateway thread.
 * Rationale: slow commands (AI answers, /start's 90s agent wait, uploads)
 * must never stall heartbeats or delay other interactions past Discord's
 * 3s acknowledgement window. All shared state is mutex-guarded and all
 * per-call scratch is thread-local; events are deep-copied because the
 * gateway reuses/frees its buffers on return.
 */
typedef struct {
    discord_client_t *c;
    bot_state_t *st;
    bool is_interaction;
    disc_message_t msg;
    disc_interaction_t inter;
} event_job_t;

static void *event_worker(void *arg) {
    event_job_t *job = arg;
    if (job->is_interaction) {
        handle_interaction(job->c, job->st, &job->inter);
        disc_interaction_free(&job->inter);
    } else {
        events_handle_message(job->c, job->st, &job->msg);
        disc_message_free(&job->msg);
    }
    free(job);
    return NULL;
}

static void spawn_event_worker(event_job_t *job) {
    pthread_t th;
    pthread_attr_t at;
    pthread_attr_init(&at);
    pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&th, &at, event_worker, job) != 0) {
        /* fallback: run inline (gateway stalls, but nothing is lost) */
        event_worker(job);
    }
    pthread_attr_destroy(&at);
}

static void on_message(discord_client_t *c, const disc_message_t *m, void *ud) {
    event_job_t *job = xmalloc(sizeof *job);
    if (!job)
        return;
    job->c = c;
    job->st = ud;
    job->is_interaction = false;
    if (disc_message_clone(m, &job->msg) != 0) {
        free(job);
        return;
    }
    spawn_event_worker(job);
}

static void on_interaction(discord_client_t *c, const disc_interaction_t *in,
                           void *ud) {
    if (in->type != 2)
        return;
    event_job_t *job = xmalloc(sizeof *job);
    if (!job)
        return;
    job->c = c;
    job->st = ud;
    job->is_interaction = true;
    if (disc_interaction_clone(in, &job->inter) != 0) {
        free(job);
        return;
    }
    spawn_event_worker(job);
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
    static bool first_ready = false;
    bool first = false;
    if (!first_ready) {
        first_ready = true;
        first = true;
    }
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
    if (has && first) {
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

    if (!check_rest)
        setup_file_log();

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
    vm_set_connect_uri(state.libvirt_uri);
    if (bot_state_watch(&state, on_config_change) != 0)
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
