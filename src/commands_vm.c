/* VM + shell + restart commands: ps status start stop restart info shell botrestart. */
#include "bot.h"
#include "commands.h"
#include "util.h"
#include "vm.h"

#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

void cmd_help(cmd_ctx_t *ctx, const char *arg) {
    (void)arg;
    ctx_reply(ctx, artixy_help_text);
}

void cmd_ps(cmd_ctx_t *ctx, const char *arg) {
    (void)arg;
    if (!ctx_need_auth(ctx))
        return;
    char *args[] = { "list", "--all", NULL };
    char *out = NULL;
    if (vm_virsh(args, &out) != 0) {
        char buf[2048];
        bot_codeblock(vm_error(), buf, sizeof buf);
        ctx_reply(ctx, buf);
        return;
    }
    char buf[2048];
    bot_codeblock(out, buf, sizeof buf);
    free(out);
    ctx_reply(ctx, buf);
}

void cmd_status(cmd_ctx_t *ctx, const char *arg) {
    (void)arg;
    if (!ctx_need_auth(ctx))
        return;
    const char *v = ctx_require_vm(ctx);
    if (!v[0])
        return;
    char vmname[256];
    snprintf(vmname, sizeof vmname, "%s", v);
    char *args[] = { "domstate", vmname, NULL };
    char *state = NULL;
    char statebuf[512];
    if (vm_virsh(args, &state) != 0) {
        snprintf(statebuf, sizeof statebuf, "%s", vm_error());
    } else {
        snprintf(statebuf, sizeof statebuf, "%s", state);
        free(state);
    }
    const char *agent = "agent: n/a (off)";
    if (strcmp(statebuf, "running") == 0)
        agent = vm_agent_ping(vmname) ? "agent: up" : "agent: DOWN";
    char msg[1024];
    snprintf(msg, sizeof msg, "`%s`: %s | %s", vmname, statebuf, agent);
    ctx_reply(ctx, msg);
}

void cmd_start(cmd_ctx_t *ctx, const char *arg) {
    (void)arg;
    if (!ctx_need_auth(ctx))
        return;
    const char *v = ctx_require_vm(ctx);
    if (!v[0])
        return;
    char vmname[256];
    snprintf(vmname, sizeof vmname, "%s", v);
    char *sa[] = { "domstate", vmname, NULL };
    char *state = NULL;
    bool running = vm_virsh(sa, &state) == 0 && strcmp(state, "running") == 0;
    free(state);
    bool started_here = false;
    time_t t0 = 0;
    char msg[1024];
    if (running) {
        snprintf(msg, sizeof msg,
                 "`%s` is already on. Waiting for the guest agent…", vmname);
        ctx_reply(ctx, msg);
    } else {
        char *sta[] = { "start", vmname, NULL };
        char *o = NULL;
        if (vm_virsh(sta, &o) != 0) {
            char buf[2048];
            bot_codeblock(vm_error(), buf, sizeof buf);
            ctx_reply(ctx, buf);
            free(o);
            return;
        }
        free(o);
        started_here = true;
        t0 = time(NULL);
        snprintf(msg, sizeof msg,
                 "`%s` starting. Waiting for the guest agent…", vmname);
        ctx_reply(ctx, msg);
    }
    if (vm_wait_agent(vmname, 90)) {
        if (started_here) {
            long secs = (long)(time(NULL) - t0);
            if (secs >= 60)
                snprintf(msg, sizeof msg, "%s booted in %ldm %lds.", vmname,
                         secs / 60, secs % 60);
            else
                snprintf(msg, sizeof msg, "%s booted in %lds.", vmname, secs);
        } else {
            snprintf(msg, sizeof msg,
                     "`%s` is on and the guest agent answers.", vmname);
        }
        ctx_reply(ctx, msg);
    } else {
        ctx_reply(ctx, "Artix bot is on /start to boot artix");
    }
}

void cmd_stop(cmd_ctx_t *ctx, const char *arg) {
    (void)arg;
    if (!ctx_need_auth(ctx))
        return;
    const char *v = ctx_require_vm(ctx);
    if (!v[0])
        return;
    char vmname[256];
    snprintf(vmname, sizeof vmname, "%s", v);
    char msg[1024];
    snprintf(msg, sizeof msg, "stopping %s…", vmname);
    ctx_reply(ctx, msg);
    char *sa[] = { "shutdown", vmname, NULL };
    char *o = NULL;
    if (vm_virsh(sa, &o) != 0) {
        char buf[2048];
        bot_codeblock(vm_error(), buf, sizeof buf);
        ctx_reply(ctx, buf);
        free(o);
        return;
    }
    free(o);
    for (int i = 0; i < 60; i++) {
        struct timespec ts = { 1, 0 };
        nanosleep(&ts, NULL);
        char *da[] = { "domstate", vmname, NULL };
        char *st = NULL;
        if (vm_virsh(da, &st) == 0 && strcmp(st, "shut off") == 0) {
            free(st);
            snprintf(msg, sizeof msg, "%s has stopped.", vmname);
            ctx_reply(ctx, msg);
            return;
        }
        free(st);
    }
    snprintf(msg, sizeof msg, "%s is still stopping — check `;status`.", vmname);
    ctx_reply(ctx, msg);
}

void cmd_restart_vm(cmd_ctx_t *ctx, const char *arg) {
    (void)arg;
    if (!ctx_need_auth(ctx))
        return;
    const char *v = ctx_require_vm(ctx);
    if (!v[0])
        return;
    char vmname[256];
    snprintf(vmname, sizeof vmname, "%s", v);
    char *args[] = { "reboot", vmname, NULL };
    char *o = NULL;
    char msg[1024];
    if (vm_virsh(args, &o) != 0) {
        char buf[2048];
        bot_codeblock(vm_error(), buf, sizeof buf);
        ctx_reply(ctx, buf);
        free(o);
        return;
    }
    free(o);
    snprintf(msg, sizeof msg, "`%s` rebooting.", vmname);
    ctx_reply(ctx, msg);
}

void cmd_info(cmd_ctx_t *ctx, const char *arg) {
    (void)arg;
    if (!ctx_need_auth(ctx))
        return;
    const char *v = ctx_require_vm(ctx);
    if (!v[0])
        return;
    char vmname[256];
    snprintf(vmname, sizeof vmname, "%s", v);
    char *args[] = { "dominfo", vmname, NULL };
    char *o = NULL;
    if (vm_virsh(args, &o) != 0) {
        char buf[2048];
        bot_codeblock(vm_error(), buf, sizeof buf);
        ctx_reply(ctx, buf);
        free(o);
        return;
    }
    const char *agent = vm_agent_ping(vmname)
                            ? "guest agent: up"
                            : "guest agent: DOWN (install qemu-guest-agent in Artix)";
    char combined[4096];
    snprintf(combined, sizeof combined, "%s\n%s", o, agent);
    free(o);
    char buf[2048];
    bot_codeblock(combined, buf, sizeof buf);
    ctx_reply(ctx, buf);
}

void cmd_shell(cmd_ctx_t *ctx, const char *arg) {
    if (!ctx_need_auth(ctx))
        return;
    char key[32];
    snprintf(key, sizeof key, "%llu", (unsigned long long)ctx->author_id);
    /* trim + lowercase arg */
    char name[16];
    size_t w = 0;
    if (arg) {
        for (size_t i = 0; arg[i] && w + 1 < sizeof name; i++) {
            char ch = arg[i];
            if (ch >= 'A' && ch <= 'Z')
                ch = (char)(ch - 'A' + 'a');
            if (ch == ' ' || ch == '\t')
                continue;
            name[w++] = ch;
        }
    }
    name[w] = '\0';
    if (!name[0]) {
        pthread_rwlock_rdlock(&ctx->st->mu);
        const char *cur = strmap_get(&ctx->st->shells, key);
        char curbuf[16];
        snprintf(curbuf, sizeof curbuf, "%s", cur ? cur : "bash");
        pthread_rwlock_unlock(&ctx->st->mu);
        char msg[256];
        snprintf(msg, sizeof msg,
                 "Your shell: `%s`. Change with `/shell fish` or `/shell bash`.",
                 curbuf);
        ctx_reply(ctx, msg);
        return;
    }
    if (strcmp(name, "fish") != 0 && strcmp(name, "bash") != 0) {
        ctx_reply(ctx, "Only `fish` or `bash`.");
        return;
    }
    pthread_rwlock_wrlock(&ctx->st->mu);
    strmap_set(&ctx->st->shells, key, name);
    pthread_rwlock_unlock(&ctx->st->mu);
    bot_persist(ctx->st);
    char msg[128];
    snprintf(msg, sizeof msg, "Your shell is now `%s`.", name);
    ctx_reply(ctx, msg);
}

static bool deployed_via_nix(void) {
    char exe[4096];
    ssize_t n = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
    if (n <= 0)
        return false;
    exe[n] = '\0';
    return strncmp(exe, "/nix/store/", 11) == 0;
}

void cmd_botrestart(cmd_ctx_t *ctx, const char *arg) {
    (void)arg;
    if (!ctx_need_auth(ctx))
        return;
    if (deployed_via_nix()) {
        ctx_reply(ctx,
                  "Deployed from Nix — I can't re-exec myself out of a read-only "
                  "`/nix/store`. Restart with `systemctl --user restart artixy`.");
        return;
    }
    ctx_reply(ctx, "Restarting…");
    char exe[4096];
    ssize_t n = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
    if (n <= 0)
        return;
    exe[n] = '\0';
    /* give the reply a moment to flush through the gateway/REST path */
    struct timespec ts = { 1, 0 };
    nanosleep(&ts, NULL);
    pid_t pid = fork();
    if (pid < 0)
        return;
    if (pid == 0) {
        setsid();
        int fd = open("/dev/null", O_RDWR);
        if (fd >= 0) {
            dup2(fd, STDIN_FILENO);
            dup2(fd, STDOUT_FILENO);
            dup2(fd, STDERR_FILENO);
            if (fd > 2)
                close(fd);
        }
        char *argv[] = { exe, NULL };
        execv(exe, argv);
        _exit(127);
    }
    _exit(0);
}
