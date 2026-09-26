/* Account + moderation + sayas + transfer commands. */
#include "ai.h"
#include "b64.h"
#include "bot.h"
#include "commands.h"
#include "live.h"
#include "util.h"
#include "vm.h"

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

/* ---------------- option + mention helpers ---------------- */

const char *cmd_opt_str(const disc_interaction_t *in, const char *name) {
    for (size_t i = 0; i < in->n_options; i++) {
        if (strcmp(in->options[i].name, name) == 0)
            return in->options[i].str_val ? in->options[i].str_val : "";
    }
    return "";
}

bool cmd_opt_bool(const disc_interaction_t *in, const char *name,
                  bool *present) {
    for (size_t i = 0; i < in->n_options; i++) {
        if (strcmp(in->options[i].name, name) == 0 &&
            in->options[i].type == 5) {
            *present = true;
            return in->options[i].bool_val;
        }
    }
    *present = false;
    return false;
}

long long cmd_opt_int(const disc_interaction_t *in, const char *name,
                      bool *present) {
    for (size_t i = 0; i < in->n_options; i++) {
        if (strcmp(in->options[i].name, name) == 0 &&
            in->options[i].type == 4) {
            *present = true;
            return in->options[i].int_val;
        }
    }
    *present = false;
    return 0;
}

uint64_t cmd_opt_user(const disc_interaction_t *in, const char *name,
                      const char **username_out) {
    for (size_t i = 0; i < in->n_options; i++) {
        if (strcmp(in->options[i].name, name) == 0 &&
            in->options[i].type == 6) {
            uint64_t id = in->options[i].user_id;
            if (username_out) {
                *username_out = "";
                for (size_t k = 0; k < in->n_resolved_users; k++) {
                    if (in->resolved_users[k].id == id)
                        *username_out = in->resolved_users[k].username
                                            ? in->resolved_users[k].username
                                            : "";
                }
            }
            return id;
        }
    }
    return 0;
}

/* Attachment option (type 11): value is the attachment id; resolve it. */
const disc_attachment_t *cmd_opt_attachment(const disc_interaction_t *in,
                                            const char *name) {
    for (size_t i = 0; i < in->n_options; i++) {
        if (strcmp(in->options[i].name, name) == 0 &&
            in->options[i].type == 11 && in->options[i].str_val) {
            uint64_t id = parse_u64(in->options[i].str_val);
            for (size_t k = 0; k < in->n_resolved_attachments; k++) {
                if (in->resolved_attachments[k].id == id)
                    return &in->resolved_attachments[k];
            }
        }
    }
    return NULL;
}

static void uname_of(discord_client_t *dc, uint64_t id, char *out, size_t n) {
    char *name = NULL;
    if (discord_get_user(dc, id, &name, NULL) == 0 && name) {
        snprintf(out, n, "%s", name);
        free(name);
    } else {
        snprintf(out, n, "%llu", (unsigned long long)id);
        free(name);
    }
}

/* ---------------- user ---------------- */

static void do_users(cmd_ctx_t *ctx) {
    if (!ctx_need_auth(ctx))
        return;
    /* snapshot ids under lock; usernames resolved after unlock */
    uint64_t owner = 0;
    uint64_t admins[256];
    size_t n_admins = 0;
    uint64_t users[1024];
    size_t n_users = 0;
    char linuxvals[1024][40];
    pthread_rwlock_rdlock(&ctx->st->mu);
    owner = ctx->st->owner;
    n_admins = ctx->st->n_admins < 256 ? ctx->st->n_admins : 256;
    for (size_t i = 0; i < n_admins; i++)
        admins[i] = ctx->st->admins[i];
    n_users = ctx->st->n_users < 1024 ? ctx->st->n_users : 1024;
    for (size_t i = 0; i < n_users; i++) {
        users[i] = ctx->st->users[i];
        char key[32];
        snprintf(key, sizeof key, "%llu", (unsigned long long)users[i]);
        const char *lx = strmap_get(&ctx->st->linux, key);
        snprintf(linuxvals[i], sizeof linuxvals[i], "%s", lx ? lx : "");
    }
    pthread_rwlock_unlock(&ctx->st->mu);
    char msg[8192];
    size_t w = 0;
    char uname[128];
    uname_of(ctx->dc, owner, uname, sizeof uname);
    w += snprintf(msg + w, sizeof(msg) - w, "Owner: `%s`\nAdmins:", uname);
    if (!n_admins) {
        w += snprintf(msg + w, sizeof(msg) - w,
                      " none yet — set `admin_ids` in config");
    } else {
        for (size_t i = 0; i < n_admins && w < sizeof(msg) - 64; i++) {
            uname_of(ctx->dc, admins[i], uname, sizeof uname);
            w += snprintf(msg + w, sizeof(msg) - w, "\n`%s`", uname);
        }
    }
    w += snprintf(msg + w, sizeof(msg) - w, "\nManagers (discord → linux):");
    if (!n_users) {
        snprintf(msg + w, sizeof(msg) - w,
                 " none yet — owner runs `/user add @user`");
    } else {
        for (size_t i = 0; i < n_users && w < sizeof(msg) - 160; i++) {
            uname_of(ctx->dc, users[i], uname, sizeof uname);
            if (linuxvals[i][0])
                w += snprintf(msg + w, sizeof(msg) - w, "\n`%s` → `%s`", uname,
                              linuxvals[i]);
            else
                w += snprintf(msg + w, sizeof(msg) - w,
                              "\n`%s` → (no linux account)", uname);
        }
    }
    ctx_reply(ctx, msg);
}

static int ensure_passwordless_sudo(const char *vm, const char *user) {
    char *script = bot_sudoers_script(user);
    if (!script)
        return -1;
    char *args[] = { "-c", script, NULL };
    long long code = 0;
    int rc = vm_guest_exec(vm, "/bin/bash", args, false, 15, &code, NULL, NULL);
    if (rc != 0 && strstr(vm_error(), "No such file"))
        rc = vm_guest_exec(vm, "/bin/sh", args, false, 15, &code, NULL, NULL);
    free(script);
    if (rc != 0 || code != 0)
        return -1;
    return 0;
}

static void do_useradd(cmd_ctx_t *ctx, uint64_t uid, const char *username) {
    if (!bot_is_elevated(ctx->st, ctx->author_id)) {
        ctx_deny(ctx, "Owner or admin only.");
        return;
    }
    char name[64];
    bot_sanitize_discord_name(username, name, sizeof name);
    if (!name[0]) {
        snprintf(name, sizeof name, "u%llu", (unsigned long long)uid);
    }
    const char *v = ctx_require_vm(ctx);
    if (!v[0])
        return;
    char vmname[256];
    snprintf(vmname, sizeof vmname, "%s", v);
    char *sa[] = { "domstate", vmname, NULL };
    char *state = NULL;
    bool running = vm_virsh(sa, &state) == 0 && strcmp(state, "running") == 0;
    free(state);
    char msg[2048];
    if (!running) {
        snprintf(msg, sizeof msg,
                 "Linux account not created: `%s` is off — run `/start` first, "
                 "then rerun `/user add @user`.",
                 vmname);
        ctx_reply(ctx, msg);
        return;
    }
    if (!vm_wait_agent(vmname, 60)) {
        ctx_reply(ctx, "Linux account not created: the guest agent is silent. "
                       "Install `qemu-guest-agent` in Artix, then rerun "
                       "`/user add @user`.");
        return;
    }
    char *ua[] = { "-m", "-s", "/bin/bash", name, NULL };
    long long code = 0;
    int rc = vm_guest_exec(vmname, "/usr/bin/useradd", ua, false, 30, &code,
                           NULL, NULL);
    if (rc != 0 && strstr(vm_error(), "No such file")) {
        rc = vm_guest_exec(vmname, "/usr/sbin/useradd", ua, false, 30, &code,
                           NULL, NULL);
    }
    if (rc != 0) {
        char buf[2048], eb[1024];
        bot_codeblock(vm_error(), eb, sizeof eb);
        snprintf(buf, sizeof buf,
                 "Linux account not created: `useradd` in the VM failed:\n%s", eb);
        ctx_reply(ctx, buf);
        return;
    }
    if (code == 9) {
        snprintf(msg, sizeof msg, "Linux user `%s` already exists, linking it.",
                 name);
        ctx_reply(ctx, msg);
    } else if (code != 0) {
        snprintf(msg, sizeof msg,
                 "Linux account not created: `useradd` in the VM failed (code %lld).",
                 code);
        ctx_reply(ctx, msg);
        return;
    }
    char key[32];
    snprintf(key, sizeof key, "%llu", (unsigned long long)uid);
    pthread_rwlock_wrlock(&ctx->st->mu);
    strmap_set(&ctx->st->linux, key, name);
    /* collect sudo targets */
    char targets[64][40];
    size_t ntargets = 0;
    {
        const char *vals[65];
        size_t nv = 0;
        vals[nv++] = name;
        for (size_t i = 0; i < ctx->st->linux.len && nv < 65; i++)
            vals[nv++] = ctx->st->linux.vals[i];
        for (size_t i = 0; i < nv && ntargets < 64; i++) {
            if (!bot_valid_runas(vals[i]))
                continue;
            bool seen = false;
            for (size_t k = 0; k < ntargets; k++) {
                if (strcmp(targets[k], vals[i]) == 0) {
                    seen = true;
                    break;
                }
            }
            if (!seen) {
                snprintf(targets[ntargets], sizeof targets[ntargets], "%s",
                         vals[i]);
                ntargets++;
            }
        }
    }
    pthread_rwlock_unlock(&ctx->st->mu);
    bot_persist(ctx->st);
    char failed[1024] = "";
    size_t flen = 0;
    for (size_t i = 0; i < ntargets; i++) {
        if (ensure_passwordless_sudo(vmname, targets[i]) != 0) {
            fprintf(stderr, "sudo setup for `%s` failed\n", targets[i]);
            size_t tl = strlen(targets[i]);
            if (flen + tl + 2 < sizeof failed) {
                memcpy(failed + flen, targets[i], tl);
                flen += tl;
                failed[flen++] = ' ';
                failed[flen] = '\0';
            }
        }
    }
    if (!failed[0]) {
        snprintf(msg, sizeof msg,
                 "added user \"%s\" linked to `%llu` — account created, "
                 "passwordless sudo enabled.",
                 name, (unsigned long long)uid);
    } else {
        snprintf(msg, sizeof msg,
                 "added user \"%s\" linked to `%llu` — account created, but "
                 "passwordless sudo failed for: `%s`. Re-run `/user add @user` "
                 "once the guest is healthy.",
                 name, (unsigned long long)uid, failed);
    }
    ctx_reply(ctx, msg);
}

static void do_userdel(cmd_ctx_t *ctx, uint64_t uid) {
    if (!bot_is_elevated(ctx->st, ctx->author_id)) {
        ctx_deny(ctx, "Owner or admin only.");
        return;
    }
    char key[32];
    snprintf(key, sizeof key, "%llu", (unsigned long long)uid);
    bool was_manager = false;
    char linked[64] = "";
    bool has_linked = false;
    pthread_rwlock_wrlock(&ctx->st->mu);
    for (size_t i = 0; i < ctx->st->n_users; i++) {
        if (ctx->st->users[i] == uid) {
            memmove(&ctx->st->users[i], &ctx->st->users[i + 1],
                    (ctx->st->n_users - i - 1) * sizeof(uint64_t));
            ctx->st->n_users--;
            was_manager = true;
            break;
        }
    }
    const char *lx = strmap_get(&ctx->st->linux, key);
    if (lx) {
        snprintf(linked, sizeof linked, "%s", lx);
        has_linked = true;
        /* remove from map */
        strmap_t nm;
        strmap_init(&nm);
        for (size_t i = 0; i < ctx->st->linux.len; i++) {
            if (strcmp(ctx->st->linux.keys[i], key) != 0)
                strmap_set(&nm, ctx->st->linux.keys[i], ctx->st->linux.vals[i]);
        }
        strmap_free(&ctx->st->linux);
        ctx->st->linux = nm;
    }
    pthread_rwlock_unlock(&ctx->st->mu);
    bot_persist(ctx->st);
    const char *revoked = was_manager ? " Bot access revoked." : "";
    char msg[2048];
    if (has_linked && bot_valid_runas(linked)) {
        const char *v = ctx_require_vm(ctx);
        if (!v[0])
            return;
        char vmname[256];
        snprintf(vmname, sizeof vmname, "%s", v);
        char dropin[128];
        snprintf(dropin, sizeof dropin, "/etc/sudoers.d/%s", linked);
        char *rma[] = { "-f", dropin, NULL };
        long long c0 = 0;
        vm_guest_exec(vmname, "/bin/rm", rma, false, 10, &c0, NULL, NULL);
        char *uda[] = { "-r", linked, NULL };
        long long code = 0;
        int rc = vm_guest_exec(vmname, "/usr/sbin/userdel", uda, false, 30,
                               &code, NULL, NULL);
        if (rc != 0 && strstr(vm_error(), "No such file"))
            rc = vm_guest_exec(vmname, "/usr/bin/userdel", uda, false, 30,
                               &code, NULL, NULL);
        if (rc != 0) {
            char eb[1024];
            bot_codeblock(vm_error(), eb, sizeof eb);
            snprintf(msg, sizeof msg, "Deleting linux `%s` failed:\n%s%s",
                     linked, eb, revoked);
        } else if (code == 0) {
            snprintf(msg, sizeof msg, "Deleted linux `%s` for <@%llu>.%s",
                     linked, (unsigned long long)uid, revoked);
        } else {
            snprintf(msg, sizeof msg,
                     "Deleting linux `%s` failed (code %lld). Remove it by hand "
                     "in the VM.%s",
                     linked, code, revoked);
        }
        ctx_reply(ctx, msg);
        return;
    }
    if (has_linked) {
        snprintf(msg, sizeof msg,
                 "Linked name `%s` looked invalid, left alone in the VM.%s",
                 linked, revoked);
    } else if (was_manager) {
        snprintf(msg, sizeof msg,
                 "Removed <@%llu> from the bot (no linux account was linked).",
                 (unsigned long long)uid);
    } else {
        snprintf(msg, sizeof msg, "<@%llu> has no linked linux account.",
                 (unsigned long long)uid);
    }
    ctx_reply(ctx, msg);
}

void cmd_user(cmd_ctx_t *ctx, const char *arg, uint64_t target_id,
              const char *target_name) {
    char action[16] = "list";
    if (arg)
        sscanf(arg, "%15s", action);
    for (char *p = action; *p; p++) {
        if (*p >= 'A' && *p <= 'Z')
            *p = (char)(*p - 'A' + 'a');
    }
    if (strcmp(action, "add") == 0) {
        if (!target_id) {
            ctx_reply(ctx, "Pick a user: `/user action:add user:@user`.");
            return;
        }
        do_useradd(ctx, target_id, target_name ? target_name : "");
    } else if (strcmp(action, "remove") == 0) {
        if (!target_id) {
            ctx_reply(ctx, "Pick a user: `/user action:remove user:@user`.");
            return;
        }
        do_userdel(ctx, target_id);
    } else {
        do_users(ctx);
    }
}

/* ---------------- admin ---------------- */

void cmd_admin(cmd_ctx_t *ctx, const char *arg, uint64_t target_id) {
    char action[16] = "list";
    if (arg)
        sscanf(arg, "%15s", action);
    for (char *p = action; *p; p++) {
        if (*p >= 'A' && *p <= 'Z')
            *p = (char)(*p - 'A' + 'a');
    }
    if (strcmp(action, "add") == 0) {
        if (!target_id) {
            ctx_reply(ctx, "Pick a user: `/admin action:add user:@user`.");
            return;
        }
        if (!bot_is_owner(ctx->st, ctx->author_id)) {
            ctx_deny(ctx, "Owner only.");
            return;
        }
        char msg[256];
        pthread_rwlock_wrlock(&ctx->st->mu);
        if (target_id == ctx->st->owner) {
            pthread_rwlock_unlock(&ctx->st->mu);
            ctx_reply(ctx, "That user is the owner already.");
            return;
        }
        bool dup = false;
        for (size_t i = 0; i < ctx->st->n_admins; i++) {
            if (ctx->st->admins[i] == target_id) {
                dup = true;
                break;
            }
        }
        if (dup) {
            pthread_rwlock_unlock(&ctx->st->mu);
            snprintf(msg, sizeof msg, "<@%llu> is already an admin.",
                     (unsigned long long)target_id);
            ctx_reply(ctx, msg);
            return;
        }
        uint64_t *na = xrealloc(ctx->st->admins,
                                (ctx->st->n_admins + 1) * sizeof(uint64_t));
        if (na) {
            ctx->st->admins = na;
            ctx->st->admins[ctx->st->n_admins++] = target_id;
        }
        pthread_rwlock_unlock(&ctx->st->mu);
        bot_persist(ctx->st);
        snprintf(msg, sizeof msg, "Added <@%llu> as admin.",
                 (unsigned long long)target_id);
        ctx_reply(ctx, msg);
    } else if (strcmp(action, "remove") == 0) {
        if (!target_id) {
            ctx_reply(ctx, "Pick a user: `/admin action:remove user:@user`.");
            return;
        }
        if (!bot_is_owner(ctx->st, ctx->author_id)) {
            ctx_deny(ctx, "Owner only.");
            return;
        }
        char msg[256];
        bool found = false;
        pthread_rwlock_wrlock(&ctx->st->mu);
        for (size_t i = 0; i < ctx->st->n_admins; i++) {
            if (ctx->st->admins[i] == target_id) {
                memmove(&ctx->st->admins[i], &ctx->st->admins[i + 1],
                        (ctx->st->n_admins - i - 1) * sizeof(uint64_t));
                ctx->st->n_admins--;
                found = true;
                break;
            }
        }
        pthread_rwlock_unlock(&ctx->st->mu);
        if (found) {
            bot_persist(ctx->st);
            snprintf(msg, sizeof msg, "Removed <@%llu> from admins.",
                     (unsigned long long)target_id);
        } else {
            snprintf(msg, sizeof msg, "<@%llu> is not an admin.",
                     (unsigned long long)target_id);
        }
        ctx_reply(ctx, msg);
    } else {
        if (!bot_is_elevated(ctx->st, ctx->author_id)) {
            ctx_deny(ctx, "Owner or admin only.");
            return;
        }
        uint64_t snap[256];
        size_t n = 0;
        pthread_rwlock_rdlock(&ctx->st->mu);
        n = ctx->st->n_admins < 256 ? ctx->st->n_admins : 256;
        for (size_t i = 0; i < n; i++)
            snap[i] = ctx->st->admins[i];
        pthread_rwlock_unlock(&ctx->st->mu);
        if (!n) {
            ctx_reply(ctx, "No admins yet — owner runs `/admin add @user`.");
            return;
        }
        char msg[2048] = "Admins:";
        size_t w = strlen(msg);
        for (size_t i = 0; i < n && w < sizeof(msg) - 64; i++) {
            char un[128];
            uname_of(ctx->dc, snap[i], un, sizeof un);
            w += snprintf(msg + w, sizeof(msg) - w, "\n`%s`", un);
        }
        ctx_reply(ctx, msg);
    }
}

/* ---------------- notify / warmode ---------------- */

void cmd_notify(cmd_ctx_t *ctx, const char *arg) {
    if (!bot_is_elevated(ctx->st, ctx->author_id)) {
        ctx_deny(ctx, "Owner or admin only.");
        return;
    }
    char what[64] = "";
    if (arg)
        sscanf(arg, "%63s", what);
    if (!what[0]) {
        pthread_rwlock_rdlock(&ctx->st->mu);
        bool has = ctx->st->has_notify;
        uint64_t ch = ctx->st->notify_channel;
        pthread_rwlock_unlock(&ctx->st->mu);
        char msg[256];
        if (has)
            snprintf(msg, sizeof msg, "Boot messages go to <#%llu>.",
                     (unsigned long long)ch);
        else
            snprintf(msg, sizeof msg, "Boot messages are OFF (no channel set).");
        ctx_reply(ctx, msg);
        return;
    }
    if (strcasecmp(what, "off") == 0) {
        pthread_rwlock_wrlock(&ctx->st->mu);
        ctx->st->has_notify = false;
        pthread_rwlock_unlock(&ctx->st->mu);
        bot_persist(ctx->st);
        ctx_reply(ctx, "Boot messages OFF.");
        return;
    }
    uint64_t id = parse_u64(what);
    if (!id) {
        ctx_reply(ctx, "Usage: `/notify <channel-id>` or `/notify off`.");
        return;
    }
    pthread_rwlock_wrlock(&ctx->st->mu);
    ctx->st->has_notify = true;
    ctx->st->notify_channel = id;
    pthread_rwlock_unlock(&ctx->st->mu);
    bot_persist(ctx->st);
    char msg[512];
    snprintf(msg, sizeof msg,
             "Boot messages will go to `<#%llu>`.\n```\n"
             "          .        :-------:\n"
             "        ^/ \\^      :Im here:\n"
             "        ●   ●     <:-------:\n"
             "       /  ω  \\\n"
             "      /_/   \\_\\\n```",
             (unsigned long long)id);
    ctx_reply(ctx, msg);
}

void cmd_warmode(cmd_ctx_t *ctx, const char *arg) {
    if (!bot_is_elevated(ctx->st, ctx->author_id)) {
        ctx_deny(ctx, "Owner or admin only.");
        return;
    }
    char w[16] = "";
    if (arg)
        sscanf(arg, "%15s", w);
    bool on = strcasecmp(w, "true") == 0 || strcmp(w, "1") == 0 ||
              strcasecmp(w, "on") == 0;
    pthread_rwlock_wrlock(&ctx->st->mu);
    ctx->st->war_mode = on;
    pthread_rwlock_unlock(&ctx->st->mu);
    bot_persist(ctx->st);
    if (on) {
        ctx_reply(ctx, "War mode on! >:3");
        return;
    }
    /* lapeace.jpg next to the working directory, like the Rust build */
    unsigned char *img = NULL;
    size_t imglen = 0;
    if (read_file("imgs/lapeace.jpg", (char **)&img, &imglen) != 0) {
        img = NULL;
    }
    if (img) {
        disc_file_t f = { "lapeace.jpg", img, imglen };
        ctx_reply_files(ctx, "war mode disabled, peace?", &f, 1);
        free(img);
    } else {
        ctx_reply(ctx, "war mode disabled, peace?");
    }
}

/* ---------------- purge_replies ---------------- */

static bool replied_to_bot(discord_client_t *dc, const disc_message_t *m,
                           uint64_t bot_id) {
    if (m->has_ref_msg)
        return m->ref_msg_author_id == bot_id;
    if (m->has_reference) {
        disc_message_t orig;
        memset(&orig, 0, sizeof orig);
        if (discord_get_message(dc, m->ref_channel_id, m->ref_message_id,
                                &orig) == 0) {
            bool ok = orig.author_id == bot_id;
            disc_message_free(&orig);
            return ok;
        }
    }
    return false;
}

void cmd_purge_replies(cmd_ctx_t *ctx, const char *target, int limit) {
    if (!bot_is_elevated(ctx->st, ctx->author_id)) {
        ctx_deny(ctx, "Owner or admin only.");
        return;
    }
    uint64_t tid = parse_target_id(target);
    if (!tid) {
        ctx_reply(ctx, "Usage: `/purge_replies <user-id> [limit]`.");
        return;
    }
    if (limit < 1)
        limit = 50;
    if (limit > 100)
        limit = 100;
    uint64_t bot_id = discord_bot_id(ctx->dc);
    disc_message_t *msgs = NULL;
    size_t n = 0;
    if (discord_channel_messages(ctx->dc, ctx->channel_id, limit, &msgs, &n) != 0) {
        char buf[512];
        bot_codeblock("could not read channel history", buf, sizeof buf);
        ctx_reply(ctx, buf);
        return;
    }
    /* plus up to 5 active threads parented here (matches Rust/Go) */
    uint64_t thread_ids[8];
    size_t nthreads = 0;
    if (ctx->guild_id)
        discord_guild_active_threads(ctx->dc, ctx->guild_id, ctx->channel_id,
                                     thread_ids, 8, &nthreads);
    disc_message_t *extra = NULL;
    size_t ne = 0, excap = 0;
    for (size_t t = 0; t < nthreads; t++) {
        disc_message_t *tm = NULL;
        size_t tn = 0;
        if (discord_channel_messages(ctx->dc, thread_ids[t], limit, &tm, &tn) != 0)
            continue;
        if (ne + tn > excap) {
            size_t nc = (ne + tn) * 2;
            disc_message_t *na = xrealloc(extra, nc * sizeof *na);
            if (!na) {
                disc_message_array_free(tm, tn);
                continue;
            }
            extra = na;
            excap = nc;
        }
        memcpy(extra + ne, tm, tn * sizeof *tm);
        free(tm);
        ne += tn;
    }
    unsigned scanned = (unsigned)(n + ne), authored = 0, deleted = 0, failed = 0;
    for (size_t pass = 0; pass < 2; pass++) {
        disc_message_t *arr = pass ? extra : msgs;
        size_t cnt = pass ? ne : n;
        for (size_t i = 0; i < cnt; i++) {
            if (arr[i].author_id != tid)
                continue;
            authored++;
            if (!replied_to_bot(ctx->dc, &arr[i], bot_id))
                continue;
            if (discord_delete_message(ctx->dc, ctx->channel_id, arr[i].id) == 0)
                deleted++;
            else {
                failed++;
                fprintf(stderr, "purge_replies: failed to delete %llu\n",
                        (unsigned long long)arr[i].id);
            }
        }
    }
    disc_message_array_free(msgs, n);
    disc_message_array_free(extra, ne);
    char msg[512];
    snprintf(msg, sizeof msg,
             "Scanned %u recent messages (%zu threads), `<@%llu>` authored %u, "
             "deleted %u replies to my messages, %u deletes failed.",
             scanned, nthreads, (unsigned long long)tid, authored, deleted,
             failed);
    ctx_reply(ctx, msg);
}

/* ---------------- sayas ---------------- */

void cmd_sayas(cmd_ctx_t *ctx, const char *message, const char *reply_to,
               const disc_attachment_t *atts, size_t n_atts) {
    if (!bot_is_elevated(ctx->st, ctx->author_id)) {
        if (ctx->is_slash)
            ctx_reply_ephemeral(ctx, "Owner or admin only.");
        else
            ctx_deny(ctx, "Owner or admin only.");
        return;
    }
    /* trim trailing whitespace of message */
    char text[2100];
    snprintf(text, sizeof text, "%s", message ? message : "");
    size_t tl = strlen(text);
    while (tl > 0 && (text[tl - 1] == ' ' || text[tl - 1] == '\t' ||
                      text[tl - 1] == '\n' || text[tl - 1] == '\r'))
        text[--tl] = '\0';
    disc_file_t *files = NULL;
    size_t n_files = 0;
    char **bufs = NULL;
    if (n_atts) {
        if (bot_download_atts(atts, n_atts, &files, &n_files, &bufs) != 0) {
            files = NULL;
            n_files = 0;
            bufs = NULL;
        }
    }
    if (!text[0] && !n_files) {
        /* toggle */
        pthread_rwlock_wrlock(&ctx->st->mu);
        ctx->st->sayas_enabled = !ctx->st->sayas_enabled;
        bool on = ctx->st->sayas_enabled;
        pthread_rwlock_unlock(&ctx->st->mu);
        bot_persist(ctx->st);
        const char *m = on ? "Say-as-artix: **enabled** — your messages will now be sent as artix (toggle again to disable)."
                           : "Say-as-artix: **disabled**.";
        if (ctx->is_slash)
            ctx_reply_ephemeral(ctx, m);
        else
            ctx_reply(ctx, m);
        bot_free_dl_files(files, n_files, bufs);
        return;
    }
    char body[2100];
    snprintf(body, sizeof body, "%s", text);
    /* >2000 chars -> file (matches Go: attach + empty body) */
    if (strlen(body) > 2000) {
        /* attach name from first word's alnum chars, like AttachName */
        char stem[64] = "";
        size_t sw = 0;
        for (size_t i = 0; body[i] && body[i] != ' ' && body[i] != '\t' &&
                           sw + 1 < sizeof stem;
             i++) {
            char ch = body[i];
            if ((ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
                (ch >= '0' && ch <= '9') || ch == '-' || ch == '_')
                stem[sw++] = ch;
        }
        stem[sw] = '\0';
        if (!sw)
            snprintf(stem, sizeof stem, "output");
        char aname[72];
        snprintf(aname, sizeof aname, "%s.txt", stem);
        /* cap body: last 400000 chars with a note (cap_file_body) */
        char *clean = bot_strip_sgr(body);
        size_t cl = clean ? strlen(clean) : 0;
        const size_t fmax = 400000;
        size_t foff = 0;
        char capnote[64] = "";
        if (cl > fmax) {
            foff = cl - fmax;
            while (clean[foff] && (clean[foff] & 0xc0) == 0x80)
                foff++;
            snprintf(capnote, sizeof capnote, "…[showing last %zu chars]\n",
                     fmax);
        }
        size_t need = strlen(capnote) + (clean ? cl - foff : 0) + 1;
        char *fdata = xmalloc(need);
        disc_file_t *nf = NULL;
        char **nb = NULL;
        if (fdata) {
            snprintf(fdata, need, "%s%s", capnote, clean ? clean + foff : "");
            nf = xrealloc(files, (n_files + 1) * sizeof *nf);
            nb = xrealloc(bufs, (n_files + 1) * sizeof *nb);
        }
        free(clean);
        if (fdata && nf && nb) {
            files = nf;
            bufs = nb;
            files[n_files].name = xstrdup(aname);
            files[n_files].data = fdata;
            files[n_files].len = strlen(fdata);
            bufs[n_files] = fdata;
            if (files[n_files].name)
                n_files++;
            else {
                free(fdata);
                /* roll back the grow */
                files = xrealloc(files, (n_files ? n_files : 1) * sizeof *nf);
                bufs = xrealloc(bufs, (n_files ? n_files : 1) * sizeof *nb);
            }
        } else {
            free(fdata);
        }
        body[0] = '\0';
    }
    if (!ctx->is_slash && ctx->msg) {
        discord_delete_message(ctx->dc, ctx->channel_id, ctx->msg->id);
    }
    if (!body[0] && !n_files) {
        ctx_reply(ctx, "Nothing to send — attach a file or type a message.");
        bot_free_dl_files(files, n_files, bufs);
        return;
    }
    if (reply_to && *reply_to) {
        uint64_t ch = 0, mid = 0;
        if (!parse_message_ref(reply_to, ctx->channel_id, &ch, &mid)) {
            ctx_reply(ctx, "Couldn't read that reply target — give a message ID or a full message link.");
            bot_free_dl_files(files, n_files, bufs);
            return;
        }
        disc_message_t target;
        memset(&target, 0, sizeof target);
        if (discord_get_message(ctx->dc, ch, mid, &target) != 0) {
            ctx_reply(ctx, "Couldn't fetch that message (wrong channel, or I can't see it).");
            bot_free_dl_files(files, n_files, bufs);
            return;
        }
        disc_message_free(&target);
        discord_send_reply(ctx->dc, ch, mid, body, files, n_files, NULL);
    } else {
        if (body[0] && !n_files)
            discord_send_message(ctx->dc, ctx->channel_id, body, NULL, 0, NULL);
        else
            discord_send_message(ctx->dc, ctx->channel_id, body, files, n_files,
                                 NULL);
    }
    if (body[0])
        ai_record_artixy(ctx->channel_id, body);
    bot_free_dl_files(files, n_files, bufs);
}

/* ---------------- send ---------------- */

void cmd_send(cmd_ctx_t *ctx, const char *path) {
    if (!ctx_need_auth(ctx))
        return;
    char p[1024];
    snprintf(p, sizeof p, "%s", path ? path : "");
    /* trim */
    while (*p == ' ' || *p == '\t')
        memmove(p, p + 1, strlen(p));
    size_t pl = strlen(p);
    while (pl > 0 && (p[pl - 1] == ' ' || p[pl - 1] == '\t'))
        p[--pl] = '\0';
    if (!p[0] || p[0] != '/') {
        ctx_reply(ctx, "Absolute path only.");
        return;
    }
    char cwd[4096];
    if (!getcwd(cwd, sizeof cwd)) {
        ctx_reply(ctx, "```\ncan't resolve project dir\n```");
        return;
    }
    char share_req[8192], target_req[8192];
    snprintf(share_req, sizeof share_req, "%s/share", cwd);
    snprintf(target_req, sizeof target_req, "%s", p);
    char share[8192], target[8192];
    if (!realpath(share_req, share)) {
        ctx_reply(ctx, "Nothing is sendable yet — create a `share/` dir in the bot's project dir and put files there.");
        return;
    }
    if (!realpath(target_req, target)) {
        ctx_reply(ctx, "No readable file there (must exist, absolute path, under the bot's `share/` dir, ~20MB max).");
        return;
    }
    size_t sl = strlen(share);
    if (!(strcmp(target, share) == 0 ||
          (strncmp(target, share, sl) == 0 && target[sl] == '/'))) {
        ctx_reply(ctx, "That path is outside the bot's `share/` dir — not sending it.");
        return;
    }
    const char *rel = target + sl + (target[sl] ? 1 : 0);
    if (bot_share_has_dot(rel)) {
        ctx_reply(ctx, "Dotfiles and dot-dirs are never sent.");
        return;
    }
    const char *base = strrchr(target, '/');
    base = base ? base + 1 : target;
    if (bot_sensitive_send_name(base)) {
        fprintf(stderr, "send refused (sensitive name): %s\n", base);
        ctx_reply(ctx, "Refusing to send secrets, keys, tokens or config files.");
        return;
    }
    char cfgreal[8192];
    if (realpath(config_path(), cfgreal) && strcmp(target, cfgreal) == 0) {
        ctx_reply(ctx, "Refusing to send secrets, keys, tokens or config files.");
        return;
    }
    struct stat st;
    if (stat(target, &st) != 0 || !S_ISREG(st.st_mode) ||
        (uint64_t)st.st_size >= 20u * 1024u * 1024u) {
        ctx_reply(ctx, "No readable file there (absolute path under the bot's `share/` dir, ~20MB max).");
        return;
    }
    char *data = NULL;
    size_t len = 0;
    if (read_file(target, &data, &len) != 0) {
        char buf[512];
        bot_codeblock("attach failed", buf, sizeof buf);
        ctx_reply(ctx, buf);
        return;
    }
    disc_file_t f = { base, data, len };
    ctx_reply_files(ctx, "", &f, 1);
    free(data);
}

/* ---------------- upload ---------------- */

static int guest_mkdir(const char *vm, const char *dir) {
    char *args[] = { "-p", "--", (char *)dir, NULL };
    long long code = 0;
    int rc = vm_guest_exec(vm, "/bin/mkdir", args, false, 15, &code, NULL, NULL);
    if (rc != 0 && strstr(vm_error(), "No such file"))
        rc = vm_guest_exec(vm, "/usr/bin/mkdir", args, false, 15, &code, NULL,
                           NULL);
    if (rc != 0 || code != 0)
        return -1;
    return 0;
}

void cmd_upload(cmd_ctx_t *ctx, const char *dir, const char *file_url,
                const char *file_name, uint64_t file_size) {
    if (!ctx_need_auth(ctx))
        return;
    if (!file_url || !*file_url) {
        ctx_reply(ctx, "Attach a file: `/upload <file> <dir>`.");
        return;
    }
    if (file_size > 20u * 1024u * 1024u) {
        ctx_reply(ctx, "That file is over ~20MB — too big to upload.");
        return;
    }
    const char *base = file_name ? strrchr(file_name, '/') : NULL;
    base = base ? base + 1 : (file_name ? file_name : "");
    if (!*base || strcmp(base, ".") == 0 || strcmp(base, "..") == 0) {
        ctx_reply(ctx, "Bad file name.");
        return;
    }
    char norm[600];
    if (!bot_normalize_guest_dir(dir ? dir : "", norm, sizeof norm)) {
        ctx_reply(ctx, "Bad destination dir — use `/tmp/artixy-uploads/...` or your own `/home/<you>/...`.");
        return;
    }
    char linked[64] = "";
    {
        const char *lx = bot_linked_user(ctx->st, ctx->author_id);
        if (bot_valid_runas(lx))
            snprintf(linked, sizeof linked, "%s", lx);
    }
    if (!bot_upload_allowed(norm, linked[0] ? linked : NULL)) {
        fprintf(stderr, "upload refused: %llu -> %s\n",
                (unsigned long long)ctx->author_id, norm);
        char msg[512];
        if (linked[0])
            snprintf(msg, sizeof msg,
                     "That dir is off-limits — use `/tmp/artixy-uploads/` or "
                     "your own `/home/%s/`.",
                     linked);
        else
            snprintf(msg, sizeof msg,
                     "That dir is off-limits — use `/tmp/artixy-uploads/` "
                     "(link a linux account with `/user add` for home uploads).");
        ctx_reply(ctx, msg);
        return;
    }
    const char *v = ctx_require_vm(ctx);
    if (!v[0])
        return;
    char vmname[256];
    snprintf(vmname, sizeof vmname, "%s", v);
    unsigned char *data = NULL;
    size_t len = 0;
    if (bot_download(file_url, 20u * 1024u * 1024u + 1, &data, &len) != 0 ||
        len > 20u * 1024u * 1024u) {
        char buf[512];
        bot_codeblock("download failed", buf, sizeof buf);
        ctx_reply(ctx, buf);
        free(data);
        return;
    }
    if (guest_mkdir(vmname, norm) != 0) {
        char buf[1024], eb[512];
        bot_codeblock(vm_error(), eb, sizeof eb);
        snprintf(buf, sizeof buf, "mkdir failed: %s", eb);
        ctx_reply(ctx, buf);
        free(data);
        return;
    }
    char dest[768];
    {
        size_t dl = strlen(norm);
        while (dl > 1 && norm[dl - 1] == '/')
            norm[dl - 1] = '\0', dl--;
        snprintf(dest, sizeof dest, "%s/%s", norm, base);
    }
    char rand[32];
    {
        unsigned int r = (unsigned int)time(NULL) ^ (unsigned int)getpid();
        snprintf(rand, sizeof rand, "%08x", r);
    }
    char tmp[96];
    snprintf(tmp, sizeof tmp, "/tmp/artixy-up-%s.b64", rand);
    char *b64 = b64_encode(data, len);
    if (!b64) {
        free(data);
        return;
    }
    size_t blen = strlen(b64);
    bool first = true;
    for (size_t i = 0; i < blen;) {
        size_t chunk = blen - i > 512 * 1024 ? 512 * 1024 : blen - i;
        char piece[512 * 1024 + 1];
        memcpy(piece, b64 + i, chunk);
        piece[chunk] = '\0';
        i += chunk;
        char tmesc[128];
        bot_sh_escape(tmp, tmesc, sizeof tmesc);
        /* piece is base64 alphabet: no quotes by construction */
        char script[512 * 1024 + 256];
        snprintf(script, sizeof script, "printf '%%s' '%s' %s %s", piece,
                 first ? ">" : ">>", tmesc);
        first = false;
        char *args[] = { "-c", script, NULL };
        long long code = 0;
        int rc = vm_guest_exec(vmname, "/bin/bash", args, false, 30, &code,
                               NULL, NULL);
        if (rc != 0 && strstr(vm_error(), "No such file"))
            rc = vm_guest_exec(vmname, "/bin/sh", args, false, 30, &code, NULL,
                               NULL);
        if (rc != 0 || code != 0) {
            char *rma[] = { "-f", tmp, NULL };
            long long c0 = 0;
            vm_guest_exec(vmname, "/bin/rm", rma, false, 10, &c0, NULL, NULL);
            char buf[256];
            snprintf(buf, sizeof buf, "```\nupload failed (code %lld)\n```", code);
            ctx_reply(ctx, buf);
            free(b64);
            free(data);
            return;
        }
    }
    free(b64);
    char tesc[128], desc[768];
    bot_sh_escape(tmp, tesc, sizeof tesc);
    bot_sh_escape(dest, desc, sizeof desc);
    char script[2048];
    snprintf(script, sizeof script,
             "base64 -d %s > %s && rm -f %s && wc -c < %s", tesc, desc, tesc,
             desc);
    char *sha[] = { "-c", script, NULL };
    long long code = 0;
    char *so = NULL, *se = NULL;
    int rc = vm_guest_exec(vmname, "/bin/bash", sha, true, 60, &code, &so, &se);
    free(se);
    if (rc != 0 && strstr(vm_error(), "No such file")) {
        free(so);
        rc = vm_guest_exec(vmname, "/bin/sh", sha, true, 60, &code, &so, &se);
        free(se);
    }
    if (rc != 0 || code != 0) {
        char *rma[] = { "-f", tmp, NULL };
        long long c0 = 0;
        vm_guest_exec(vmname, "/bin/rm", rma, false, 10, &c0, NULL, NULL);
        char buf[256];
        snprintf(buf, sizeof buf, "```\ndecode failed (code %lld)\n```", code);
        ctx_reply(ctx, buf);
        free(so);
        free(data);
        return;
    }
    unsigned long long landed = 0;
    sscanf(so ? so : "", "%llu", &landed);
    free(so);
    char msg[1024];
    if (landed != len) {
        snprintf(msg, sizeof msg,
                 "```\nsize mismatch: sent %zu but landed %llu\n```", len,
                 landed);
        ctx_reply(ctx, msg);
        free(data);
        return;
    }
    const char *suffix = "";
    if (linked[0]) {
        char chuser[72];
        snprintf(chuser, sizeof chuser, "%s:", linked);
        char *cha[] = { chuser, dest, NULL };
        long long cc = 0;
        int cr = vm_guest_exec(vmname, "/usr/bin/chown", cha, false, 15, &cc,
                               NULL, NULL);
        if (cr != 0 || cc != 0) {
            cr = vm_guest_exec(vmname, "/bin/chown", cha, false, 15, &cc, NULL,
                               NULL);
            if (cr != 0 || cc != 0)
                suffix = " (root-owned, use sudo)";
        }
    }
    snprintf(msg, sizeof msg, "uploaded `%s` (%zu bytes) to `%s`%s.", base, len,
             dest, suffix);
    ctx_reply(ctx, msg);
    free(data);
}

/* ---------------- run stub (Phase 6) + AI commands ---------------- */

void cmd_run(cmd_ctx_t *ctx, const char *arg) {
    if (!ctx_need_auth(ctx))
        return;
    char cmd[4096];
    snprintf(cmd, sizeof cmd, "%s", arg ? arg : "");
    /* trim */
    while (*cmd == ' ' || *cmd == '\t')
        memmove(cmd, cmd + 1, strlen(cmd));
    size_t cl = strlen(cmd);
    while (cl > 0 && (cmd[cl - 1] == ' ' || cmd[cl - 1] == '\t'))
        cmd[--cl] = '\0';
    if (!cl) {
        ctx_reply(ctx, "Usage: `/run <command>`.");
        return;
    }
    const char *v = ctx_require_vm(ctx);
    if (!v[0])
        return;
    char vmname[256];
    snprintf(vmname, sizeof vmname, "%s", v);
    if (!vm_agent_ping(vmname)) {
        ctx_reply(ctx, "Artix is off.");
        return;
    }
    char ack_text[4200];
    snprintf(ack_text, sizeof ack_text, "`run: %s` starting…", cmd);
    uint64_t ack_id = 0;
    if (ctx->is_slash) {
        /* deferred already; post the live message as a followup */
        discord_interaction_followup(ctx->dc, ctx->inter, ack_text, false,
                                     NULL, 0, &ack_id);
        if (!ack_id)
            return;
    } else {
        if (discord_send_message(ctx->dc, ctx->channel_id, ack_text, NULL, 0,
                                 &ack_id) != 0 ||
            !ack_id)
            return;
    }
    char linked[64] = "";
    {
        const char *lx = bot_linked_user(ctx->st, ctx->author_id);
        if (bot_valid_runas(lx))
            snprintf(linked, sizeof linked, "%s", lx);
    }
    live_begin_run(ctx->dc, ctx->st->live, ctx->channel_id, ack_id,
                   ctx->author_id, ctx->author_name, vmname, cmd, linked,
                   !bot_is_owner(ctx->st, ctx->author_id));
}

/* Parse prefix "/ai ..." args into fields. */
typedef struct {
    bool has_enabled;
    bool enabled;
    bool has_model;
    char model[160];
    bool has_prompt;
    char prompt[1024];
    bool has_forget;
    bool forget;
    bool has_think;
    bool think;
} ai_args_t;

static void parse_ai_args(const char *arg, ai_args_t *o) {
    memset(o, 0, sizeof *o);
    if (!arg)
        return;
    /* forget:true shortcut anywhere */
    {
        char low[4096];
        size_t i = 0;
        for (; arg[i] && i + 1 < sizeof low; i++)
            low[i] = (char)tolower((unsigned char)arg[i]);
        low[i] = '\0';
        if (strstr(low, "forget:true") || strstr(low, "forget: true")) {
            o->has_forget = true;
            o->forget = true;
            return;
        }
    }
    /* prompt: captures the remainder (may contain spaces) */
    {
        const char *p = arg;
        char low[4096];
        size_t i = 0;
        for (; p[i] && i + 1 < sizeof low; i++)
            low[i] = (char)tolower((unsigned char)p[i]);
        low[i] = '\0';
        char *at = strstr(low, "prompt:");
        if (at) {
            size_t off = (size_t)(at - low) + strlen("prompt:");
            const char *v = arg + off;
            while (*v == ' ' || *v == '\t')
                v++;
            /* cut at known trailing flags */
            size_t vl = strlen(v);
            char tmp[1024];
            snprintf(tmp, sizeof tmp, "%s", v);
            /* cut at known trailing flags (think:/model:/forget:) */
            char tlow[1024];
            size_t k = 0;
            for (; tmp[k] && k + 1 < sizeof tlow; k++)
                tlow[k] = (char)tolower((unsigned char)tmp[k]);
            tlow[k] = '\0';
            size_t cut = vl;
            const char *fl[] = { " think:", " model:", " forget:", NULL };
            for (size_t f = 0; fl[f]; f++) {
                char *hit = strstr(tlow, fl[f]);
                if (hit && (size_t)(hit - tlow) < cut)
                    cut = (size_t)(hit - tlow);
            }
            tmp[cut] = '\0';
            /* rtrim */
            size_t tl = strlen(tmp);
            while (tl > 0 && (tmp[tl - 1] == ' ' || tmp[tl - 1] == '\t'))
                tmp[--tl] = '\0';
            if (tmp[0]) {
                o->has_prompt = true;
                snprintf(o->prompt, sizeof o->prompt, "%s", tmp);
            }
        }
    }
    /* whitespace tokens */
    {
        char toks[4096];
        snprintf(toks, sizeof toks, "%s", arg);
        for (char *tok = strtok(toks, " \t\n"); tok;
             tok = strtok(NULL, " \t\n")) {
            char *colon = strchr(tok, ':');
            if (colon) {
                size_t kl = (size_t)(colon - tok);
                char key[32];
                if (kl >= sizeof key)
                    continue;
                for (size_t i = 0; i < kl; i++)
                    key[i] = (char)tolower((unsigned char)tok[i]);
                key[kl] = '\0';
                const char *val = colon + 1;
                if (strcmp(key, "model") == 0 && *val) {
                    o->has_model = true;
                    snprintf(o->model, sizeof o->model, "%s", val);
                } else if (strcmp(key, "think") == 0 && *val) {
                    o->has_think = true;
                    o->think = strcasecmp(val, "true") == 0 ||
                               strcmp(val, "1") == 0 ||
                               strcasecmp(val, "on") == 0;
                } else if (strcmp(key, "forget") == 0 && *val) {
                    o->has_forget = true;
                    o->forget = strcasecmp(val, "true") == 0 ||
                                strcmp(val, "1") == 0;
                } else if (strcmp(key, "enabled") == 0 && *val) {
                    o->has_enabled = true;
                    o->enabled = strcasecmp(val, "true") == 0 ||
                                 strcmp(val, "1") == 0 ||
                                 strcasecmp(val, "on") == 0;
                }
            } else if (strcasecmp(tok, "true") == 0 ||
                       strcasecmp(tok, "false") == 0 ||
                       strcasecmp(tok, "on") == 0 || strcasecmp(tok, "off") == 0) {
                o->has_enabled = true;
                o->enabled = strcasecmp(tok, "true") == 0 ||
                             strcasecmp(tok, "on") == 0;
            } else if (strcasecmp(tok, "clear") == 0) {
                o->has_prompt = true;
                snprintf(o->prompt, sizeof o->prompt, "clear");
            }
        }
    }
}

void cmd_ai(cmd_ctx_t *ctx, const char *arg); /* forward */
void cmd_ai_opts(cmd_ctx_t *ctx, bool has_enabled, bool enabled,
                 const char *model, const char *prompt, bool has_forget,
                 bool forget, bool has_think, bool think) {
    if (has_forget && forget) {
        if (!ctx_need_public(ctx))
            return;
        ai_history_clear(ctx->channel_id);
        ctx_reply(ctx, "Forgot the conversation here.");
        return;
    }
    bool changing = has_enabled || (model && *model) || (prompt && *prompt) ||
                    has_think;
    if (changing && !bot_is_elevated(ctx->st, ctx->author_id)) {
        ctx_deny(ctx, "Owner or admin only.");
        return;
    }
    if (!ctx_need_public(ctx))
        return;
    if (!changing) {
        pthread_rwlock_rdlock(&ctx->st->mu);
        char modelb[160], promptb[4096];
        snprintf(modelb, sizeof modelb, "%s", ctx->st->ai_model);
        snprintf(promptb, sizeof promptb, "%s",
                 ctx->st->ai_prompt ? ctx->st->ai_prompt : "");
        pthread_rwlock_unlock(&ctx->st->mu);
        char msg[4600];
        if (!promptb[0])
            snprintf(msg, sizeof msg, "**model:** `%s`\n**backstory:** none",
                     modelb);
        else
            snprintf(msg, sizeof msg, "**model:** `%s`\n**backstory:** %s",
                     modelb, promptb);
        if (ctx->is_slash)
            ctx_reply_ephemeral(ctx, msg);
        else {
            /* DM when possible (post_private) */
            uint64_t dm = 0;
            if (discord_create_dm(ctx->dc, ctx->author_id, &dm) == 0 && dm)
                discord_send_message(ctx->dc, dm, msg, NULL, 0, NULL);
            else
                ctx_reply(ctx, msg);
        }
        return;
    }
    bool model_touched = model && *model;
    char notice[1024];
    char host[512], mname[160];
    bool enabled_now = false;
    {
        pthread_rwlock_wrlock(&ctx->st->mu);
        if (has_enabled)
            ctx->st->ai_enabled = enabled;
        if (has_think)
            ctx->st->ai_think = think;
        if (model_touched) {
            if (!ai_valid_model_name(model)) {
                pthread_rwlock_unlock(&ctx->st->mu);
                ctx_reply(ctx, "Bad model name — use letters, numbers and `._-:/` only (e.g. `llama3.1`, `qwen2.5-coder:7b`), max 128 chars.");
                return;
            }
            snprintf(ctx->st->ai_model, sizeof ctx->st->ai_model, "%s", model);
        }
        if (prompt && *prompt) {
            if (strcasecmp(prompt, "clear") == 0) {
                free(ctx->st->ai_prompt);
                ctx->st->ai_prompt = xstrdup("");
            } else {
                free(ctx->st->ai_prompt);
                ctx->st->ai_prompt = xstrdup(prompt);
            }
        }
        const char *think_s = ctx->st->ai_think ? "on (slow)" : "off (fast)";
        const char *on_s = ctx->st->ai_enabled ? "enabled" : "disabled";
        ai_resolve_host(ctx->st->ollama_host, host, sizeof host);
        snprintf(mname, sizeof mname, "%s", ctx->st->ai_model);
        enabled_now = ctx->st->ai_enabled;
        size_t pl = ctx->st->ai_prompt ? strlen(ctx->st->ai_prompt) : 0;
        if (pl)
            snprintf(notice, sizeof notice,
                     "AI chat is **%s** — model `%s` on `%s` (think %s).\n"
                     "Backstory set (%zu chars).",
                     on_s, mname, host, think_s, pl);
        else
            snprintf(notice, sizeof notice,
                     "AI chat is **%s** — model `%s` on `%s` (think %s).\n"
                     "Backstory: none.",
                     on_s, mname, host, think_s);
        pthread_rwlock_unlock(&ctx->st->mu);
    }
    bot_persist(ctx->st);
    if ((enabled_now && has_enabled) || model_touched) {
        int present = ai_model_present(host, mname);
        if (present == 0) {
            size_t nl = strlen(notice);
            snprintf(notice + nl, sizeof(notice) - nl,
                     "\nWarning: `%s` isn't in `ollama list` on %s — run "
                     "`ollama pull %s` there.",
                     mname, host, mname);
        }
    }
    /* ephemeral for slash to avoid channel noise; prefix uses post_private */
    if (ctx->is_slash)
        ctx_reply_ephemeral(ctx, notice);
    else {
        uint64_t dm = 0;
        if (discord_create_dm(ctx->dc, ctx->author_id, &dm) == 0 && dm)
            discord_send_message(ctx->dc, dm, notice, NULL, 0, NULL);
        else
            ctx_reply(ctx, notice);
    }
}

void cmd_ai(cmd_ctx_t *ctx, const char *arg) {
    ai_args_t a;
    parse_ai_args(arg, &a);
    cmd_ai_opts(ctx, a.has_enabled, a.enabled, a.has_model ? a.model : "",
                a.has_prompt ? a.prompt : "", a.has_forget, a.forget,
                a.has_think, a.think);
}

void cmd_ask(cmd_ctx_t *ctx, const char *arg) {
    if (!ctx_need_public(ctx))
        return;
    char model[160], host[512], prompt[8192];
    bool ai_on = false;
    double temp = 0.8;
    bool think = false;
    {
        pthread_rwlock_rdlock(&ctx->st->mu);
        ai_on = ctx->st->ai_enabled;
        snprintf(model, sizeof model, "%s", ctx->st->ai_model);
        ai_resolve_host(ctx->st->ollama_host, host, sizeof host);
        snprintf(prompt, sizeof prompt, "%s",
                 ctx->st->ai_prompt ? ctx->st->ai_prompt : "");
        temp = ctx->st->ai_temperature;
        think = ctx->st->ai_think;
        pthread_rwlock_unlock(&ctx->st->mu);
    }
    if (!ai_on) {
        const char *m = "AI is off — the owner runs `/ai true model:<name>` to enable me.";
        if (ctx->is_slash)
            ctx_reply_ephemeral(ctx, m);
        else
            ctx_reply(ctx, m);
        return;
    }
    char q[4001];
    snprintf(q, sizeof q, "%s", arg ? arg : "");
    /* trim */
    while (*q == ' ' || *q == '\t' || *q == '\n')
        memmove(q, q + 1, strlen(q));
    size_t ql = strlen(q);
    while (ql > 0 && (q[ql - 1] == ' ' || q[ql - 1] == '\t' || q[ql - 1] == '\n'))
        q[--ql] = '\0';
    if (!ql) {
        const char *m = "Ask me something — `/ask <question>`.";
        if (ctx->is_slash)
            ctx_reply_ephemeral(ctx, m);
        else
            ctx_reply(ctx, m);
        return;
    }
    if (!ctx->is_slash)
        discord_trigger_typing(ctx->dc, ctx->channel_id);
    char *answer = NULL;
    if (ai_chat(host, model, ctx->channel_id, ctx->author_name, q, prompt,
                temp, think, &answer) != 0 || !answer) {
        free(answer);
        if (ctx->is_slash)
            ctx_reply_ephemeral(ctx, "sorry, glitched out — try again in a sec");
        else
            ctx_reply(ctx, "sorry, glitched out — try again in a sec");
        return;
    }
    size_t n = 0;
    char **chunks = ai_chunk_reply(answer, &n);
    free(answer);
    if (!chunks) {
        ctx_reply(ctx, "sorry, glitched out — try again in a sec");
        return;
    }
    bool first = true;
    for (size_t i = 0; i < n; i++) {
        if (!ctx->is_slash && first)
            discord_send_reply(ctx->dc, ctx->channel_id, ctx->msg->id,
                               chunks[i], NULL, 0, NULL);
        else if (!ctx->is_slash)
            discord_send_message(ctx->dc, ctx->channel_id, chunks[i], NULL, 0,
                                 NULL);
        else
            ctx_reply(ctx, chunks[i]);
        first = false;
    }
    ai_chunks_free(chunks, n);
}

/* end of file */

/* ---------------- ambient @artixy mentions ---------------- */

/* Bounded append that never overflows (silently truncates). */
static void strappend(char *dst, size_t n, const char *src) {
    size_t d = strlen(dst);
    size_t sl = strlen(src);
    if (d + 1 >= n)
        return;
    size_t room = n - d - 1;
    if (sl > room)
        sl = room;
    memcpy(dst + d, src, sl);
    dst[d + sl] = '\0';
}

static bool mentions_bot(const disc_message_t *m, uint64_t bot_id) {
    for (size_t i = 0; i < m->n_mentions; i++) {
        if (m->mentions[i] == bot_id)
            return true;
    }
    char pat1[64], pat2[64];
    snprintf(pat1, sizeof pat1, "<@%llu>", (unsigned long long)bot_id);
    snprintf(pat2, sizeof pat2, "<@!%llu>", (unsigned long long)bot_id);
    if ((m->content && strstr(m->content, pat1)) ||
        (m->content && strstr(m->content, pat2)))
        return true;
    return ai_mentions_name(m->content);
}

bool ambient_mention(discord_client_t *c, bot_state_t *st,
                     const disc_message_t *m) {
    uint64_t bot_id = discord_bot_id(c);
    if (!bot_id || !mentions_bot(m, bot_id))
        return false;
    char *no_mention = ai_strip_mention(m->content, bot_id);
    char *prompt0 = ai_strip_name(no_mention);
    free(no_mention);
    if (!prompt0)
        return false;
    bool is_command = prompt0[0] == '/' || prompt0[0] == ';';
    if (is_command) {
        free(prompt0);
        return false;
    }
    if (bot_is_blocked(st, m->author_id)) {
        free(prompt0);
        return true;
    }
    char model[160], host[512], sysprompt[8192];
    bool ai_on = false;
    double temp = 0.8;
    bool think = false;
    {
        pthread_rwlock_rdlock(&st->mu);
        ai_on = st->ai_enabled;
        snprintf(model, sizeof model, "%s", st->ai_model);
        ai_resolve_host(st->ollama_host, host, sizeof host);
        snprintf(sysprompt, sizeof sysprompt, "%s",
                 st->ai_prompt ? st->ai_prompt : "");
        temp = st->ai_temperature;
        think = st->ai_think;
        pthread_rwlock_unlock(&st->mu);
    }
    if (!ai_on) {
        discord_send_reply(c, m->channel_id, m->id,
                           "AI is off — the owner runs `/ai true model:<name>` to enable me.",
                           NULL, 0, NULL);
        free(prompt0);
        return true;
    }
    /* speaker display */
    char speaker[192];
    {
        const char *nick = m->member_nick;
        if (!nick)
            nick = m->global_name;
        if (!nick)
            nick = m->author_name;
        if (nick && m->author_name && strcmp(nick, m->author_name) != 0)
            snprintf(speaker, sizeof speaker, "%s (@%s)", nick, m->author_name);
        else
            snprintf(speaker, sizeof speaker, "%s", nick ? nick : "someone");
    }
    /* replied-message context */
    char prompt[8192];
    snprintf(prompt, sizeof prompt, "%s", prompt0);
    free(prompt0);
    const char *qtext = NULL;
    char qbuf[1600];
    char qwho[128] = "";
    if (m->has_ref_msg) {
        if (m->ref_msg_content && *m->ref_msg_content) {
            snprintf(qbuf, sizeof qbuf, "%s", m->ref_msg_content);
            qtext = qbuf;
        } else if (m->ref_msg_author_name) {
            snprintf(qbuf, sizeof qbuf, "[attachment(s)]");
            qtext = qbuf;
        }
        if (qtext)
            snprintf(qwho, sizeof qwho, "%s",
                     m->ref_msg_author_name ? m->ref_msg_author_name : "?");
    } else if (m->has_reference) {
        disc_message_t orig;
        memset(&orig, 0, sizeof orig);
        if (discord_get_message(c, m->ref_channel_id, m->ref_message_id,
                                &orig) == 0) {
            if ((orig.content && *orig.content) || orig.n_attachments) {
                if (orig.content && *orig.content)
                    snprintf(qbuf, sizeof qbuf, "%s", orig.content);
                else
                    snprintf(qbuf, sizeof qbuf, "[attachment(s)]");
                qtext = qbuf;
                const char *qn = orig.global_name;
                if (!qn)
                    qn = orig.author_name;
                snprintf(qwho, sizeof qwho, "%s", qn ? qn : "?");
            }
            disc_message_free(&orig);
        }
    }
    if (qtext) {
        char tag[96];
        char *st2 = ai_speaker_tag(qwho);
        {
            const char *src = st2 ? st2 : qwho;
            size_t sl = strlen(src);
            if (sl > sizeof(tag) - 1)
                sl = sizeof(tag) - 1;
            memcpy(tag, src, sl);
            tag[sl] = '\0';
        }
        free(st2);
        char combined[8192];
        combined[0] = '\0';
        if (!prompt[0]) {
            strappend(combined, sizeof combined, "(quoting ");
            strappend(combined, sizeof combined, tag);
            strappend(combined, sizeof combined, "): ");
            strappend(combined, sizeof combined, qtext);
        } else {
            strappend(combined, sizeof combined, prompt);
            strappend(combined, sizeof combined, "\n(replying to ");
            strappend(combined, sizeof combined, tag);
            strappend(combined, sizeof combined, "): ");
            strappend(combined, sizeof combined, qtext);
        }
        snprintf(prompt, sizeof prompt, "%s", combined);
    }
    /* recent channel context */
    {
        disc_message_t *recent = NULL;
        size_t nr = 0;
        if (discord_channel_messages(c, m->channel_id, 15, &recent, &nr) == 0) {
            char lines[2800] = "";
            size_t total = 0;
            /* recent is newest-first; iterate oldest-first, skip self */
            for (size_t i = nr; i-- > 0;) {
                if (recent[i].id == m->id)
                    continue;
                const char *who = recent[i].global_name;
                if (!who)
                    who = recent[i].author_name;
                char body[384];
                if (recent[i].content && *recent[i].content)
                    snprintf(body, sizeof body, "%s", recent[i].content);
                else if (recent[i].n_attachments)
                    snprintf(body, sizeof body, "[attachment(s)]");
                else
                    continue;
                /* cap 300 chars */
                size_t bl = strlen(body);
                if (bl > 300) {
                    size_t cut = 300;
                    while (cut > 0 && (body[cut] & 0xc0) == 0x80)
                        cut--;
                    body[cut] = '\0';
                }
                char *wtag = ai_speaker_tag(who ? who : "?");
                char line[512];
                snprintf(line, sizeof line, "- %s: %s", wtag ? wtag : "?",
                         body);
                free(wtag);
                if (total + strlen(line) + 1 > 2500)
                    break;
                strappend(lines, sizeof lines, line);
                strappend(lines, sizeof lines, "\n");
                total += strlen(line) + 1;
            }
            disc_message_array_free(recent, nr);
            if (lines[0]) {
                char combined[12288];
                combined[0] = '\0';
                strappend(combined, sizeof combined, prompt);
                strappend(combined, sizeof combined,
                          "\n\n[recent messages in channel]:\n");
                strappend(combined, sizeof combined, lines);
                {
                    size_t cl = strlen(combined);
                    if (cl > sizeof(prompt) - 1)
                        cl = sizeof(prompt) - 1;
                    memcpy(prompt, combined, cl);
                    prompt[cl] = '\0';
                }
            }
        }
    }
    if (!prompt[0]) {
        char hint[256];
        snprintf(hint, sizeof hint,
                 "Ping me with a question — `@artixy <question>` or "
                 "`artixy <question>` (model `%s`).",
                 model);
        discord_send_reply(c, m->channel_id, m->id, hint, NULL, 0, NULL);
        return true;
    }
    /* cap 7000 chars */
    {
        size_t bl = strlen(prompt), chars = 0, cut = 0;
        while (prompt[cut] && chars < 7000) {
            unsigned char b = (unsigned char)prompt[cut];
            cut += ((b & 0x80) == 0) ? 1 : ((b & 0xe0) == 0xc0) ? 2 :
                   ((b & 0xf0) == 0xe0)                         ? 3 :
                                                                  4;
            chars++;
        }
        prompt[cut < bl ? cut : bl] = '\0';
    }
    discord_trigger_typing(c, m->channel_id);
    char *answer = NULL;
    if (ai_chat(host, model, m->channel_id, speaker, prompt, sysprompt, temp,
                think, &answer) != 0 ||
        !answer) {
        free(answer);
        answer = ai_glitch_text(host, model, sysprompt, temp, think);
        if (!answer)
            answer = xstrdup("sorry, glitched out — try again in a sec");
    }
    size_t n = 0;
    char **chunks = ai_chunk_reply(answer ? answer : "", &n);
    free(answer);
    if (!chunks) {
        discord_send_reply(c, m->channel_id, m->id,
                           "sorry, glitched out — try again in a sec", NULL, 0,
                           NULL);
        return true;
    }
    for (size_t i = 0; i < n; i++) {
        if (i == 0)
            discord_send_reply(c, m->channel_id, m->id, chunks[i], NULL, 0,
                               NULL);
        else if (discord_send_message(c, m->channel_id, chunks[i], NULL, 0,
                                      NULL) != 0)
            break;
    }
    ai_chunks_free(chunks, n);
    return true;
}
