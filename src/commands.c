/* Command table, prefix parsing, slash registration, dispatch. */
#include "commands.h"
#include "util.h"

#include <ctype.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Scratch for the trimmed argument. Callers must consume *arg_out before
 * the next cmd_parse_prefix call (single-threaded use in handlers). */
static char argbuf[8192];

const char *artixy_help_text =
    "artixy — type commands as a plain message or as slash. One VM, no names needed.\n"
    "\nVM (owner + added users)\n/ps — list VMs\n/status — state + agent\n"
    "/start — power on, wait for agent\n/stop — graceful shutdown\n"
    "/restart — reboot\n/info — details + agent\n"
    "/run <cmd> — run in the VM as your linked user (e.g. /run ls -la)\n"
    "  reply to its live message to type into it (;return ;space ;enter ;esc ;up ;down ;left ;right, add a number to repeat)\n"
    "/send </abs/path> — send a host file from share/ (~20MB max, no secrets)\n"
    "/upload <file> <dir> — attach a file into /tmp/artixy-uploads/ or your /home/<you>/\n"
    "\naccounts (owner/admin)\n/user add @u | remove @u | list — linux account + bot access\n"
    "/admin add @u | remove @u | list — bot admins (add/remove owner only)\n"
    "/shell fish|bash — your shell\n/notify <channel-id> | off — boot message channel\n"
    "/purge_replies <user-id> [limit] — delete their replies to my messages\n"
    "/warmode true|false — arm or stand down protections\n"
    "\nas artix (owner/admin)\n/sayas [message] — post as artix, no args toggles auto mode\n"
    "<text>.ar — post that line as artix\n"
    "\nai\n@artixy <question> — chat (needs /ai true)\n"
    "/ask <question> — ask the AI, answers you directly\n/ai true|false — on/off\n"
    "/ai model:<name> — e.g. model:llama3.1\n"
    "/ai prompt:<text> | prompt:clear — backstory (long text goes in ai_prompt in config)\n"
    "/ai think:true|false — model reasoning, off is fast (default)\n"
    "/ai forget:true — forget this channel\n"
    "\nWarning: managers can power the machine on/off. Keep the token secret: config.jsonc only, never git.";

static const char *known_commands[] = {
    "help", "ps", "status", "start", "stop", "restart", "info", "run", "send",
    "upload", "shell", "botrestart", "user", "admin", "notify",
    "purge_replies", "warmode", "sayas", "ai", "ask", NULL
};

int cmd_parse_prefix(const char *content, char *name_out, size_t name_cap,
                     const char **arg_out) {
    if (!content || (*content != '/' && *content != ';'))
        return -1;
    const char *p = content + 1;
    while (*p == ' ' || *p == '\t')
        p++;
    const char *end = p;
    while (*end && *end != ' ' && *end != '\t' && *end != '\n')
        end++;
    size_t n = (size_t)(end - p);
    if (n == 0 || n + 1 > name_cap)
        return -1;
    for (size_t i = 0; i < n; i++)
        name_out[i] = (char)tolower((unsigned char)p[i]);
    name_out[n] = '\0';
    if (strcmp(name_out, "purgereplies") == 0) {
        if (strlen("purge_replies") + 1 > name_cap)
            return -1;
        strcpy(name_out, "purge_replies");
    }
    bool known = false;
    for (size_t i = 0; known_commands[i]; i++) {
        if (strcmp(name_out, known_commands[i]) == 0) {
            known = true;
            break;
        }
    }
    if (!known)
        return -1;
    while (*end == ' ' || *end == '\t')
        end++;
    /* trim trailing whitespace without mutating content */
    const char *tail = end + strlen(end);
    while (tail > end && (tail[-1] == ' ' || tail[-1] == '\t' ||
                          tail[-1] == '\n' || tail[-1] == '\r'))
        tail--;
    size_t alen = (size_t)(tail - end);
    if (alen >= sizeof argbuf)
        alen = sizeof(argbuf) - 1;
    memcpy(argbuf, end, alen);
    argbuf[alen] = '\0';
    *arg_out = argbuf;
    return 0;
}

/* Slash command definitions for bulk overwrite. */
typedef struct {
    const char *name;
    const char *desc;
    const char *options_json; /* nullable, raw JSON array fragment */
} slash_def_t;

static const slash_def_t slash_defs[] = {
    { "help", "Show help", NULL },
    { "ps", "List VMs", NULL },
    { "status", "VM state + agent", NULL },
    { "start", "Power on, wait for agent", NULL },
    { "stop", "Graceful shutdown", NULL },
    { "restart", "Reboot the VM", NULL },
    { "info", "VM details + agent", NULL },
    { "run", "Run a command in the VM",
      "[{\"type\":3,\"name\":\"cmd\",\"description\":\"Command to run\",\"required\":true}]" },
    { "send", "Send a host file from share/",
      "[{\"type\":3,\"name\":\"path\",\"description\":\"Absolute path under share/\",\"required\":true}]" },
    { "upload", "Upload a file into the VM",
      "[{\"type\":11,\"name\":\"file\",\"description\":\"File to upload\",\"required\":true},"
      "{\"type\":3,\"name\":\"dir\",\"description\":\"Destination dir\",\"required\":true}]" },
    { "shell", "Show or set your shell",
      "[{\"type\":3,\"name\":\"name\",\"description\":\"fish or bash\",\"required\":false}]" },
    { "botrestart", "Restart the bot", NULL },
    { "user", "Manage users",
      "[{\"type\":3,\"name\":\"action\",\"description\":\"add|remove|list\",\"required\":true},"
      "{\"type\":6,\"name\":\"user\",\"description\":\"Target user\",\"required\":false}]" },
    { "admin", "Manage admins",
      "[{\"type\":3,\"name\":\"action\",\"description\":\"add|remove|list\",\"required\":true},"
      "{\"type\":6,\"name\":\"user\",\"description\":\"Target user\",\"required\":false}]" },
    { "notify", "Boot message channel",
      "[{\"type\":3,\"name\":\"what\",\"description\":\"channel ID or off\",\"required\":false}]" },
    { "purge_replies", "Delete replies to my messages",
      "[{\"type\":3,\"name\":\"target\",\"description\":\"User/bot ID\",\"required\":true},"
      "{\"type\":4,\"name\":\"limit\",\"description\":\"How many to scan (max 100)\",\"required\":false}]" },
    { "warmode", "Arm or stand down protections",
      "[{\"type\":5,\"name\":\"enabled\",\"description\":\"true/false\",\"required\":true}]" },
    { "sayas", "Post as artix",
      "[{\"type\":3,\"name\":\"message\",\"description\":\"Text to send\",\"required\":false},"
      "{\"type\":3,\"name\":\"reply_to\",\"description\":\"Message ID or link\",\"required\":false},"
      "{\"type\":11,\"name\":\"file\",\"description\":\"File to send\",\"required\":false}]" },
    { "ai", "Configure AI chat",
      "[{\"type\":5,\"name\":\"enabled\",\"description\":\"on/off\",\"required\":false},"
      "{\"type\":3,\"name\":\"model\",\"description\":\"Ollama model\",\"required\":false},"
      "{\"type\":3,\"name\":\"prompt\",\"description\":\"Backstory prompt\",\"required\":false},"
      "{\"type\":5,\"name\":\"forget\",\"description\":\"Forget channel\",\"required\":false},"
      "{\"type\":5,\"name\":\"think\",\"description\":\"Think mode\",\"required\":false}]" },
    { "ask", "Ask the AI",
      "[{\"type\":3,\"name\":\"question\",\"description\":\"Question\",\"required\":true}]" },
};

char *commands_json(void) {
    /* build [{"name":..,"description":..,"options":..}, ...] */
    size_t cap = 8192, len = 0;
    char *buf = xmalloc(cap);
    if (!buf)
        return NULL;
    buf[0] = '[';
    len = 1;
#define APPEND_FMT(...)                                                       \
    do {                                                                      \
        int need = snprintf(NULL, 0, __VA_ARGS__);                            \
        if (need < 0) {                                                       \
            free(buf);                                                        \
            return NULL;                                                      \
        }                                                                     \
        while (len + (size_t)need + 2 > cap) {                                \
            cap *= 2;                                                         \
            char *nb = xrealloc(buf, cap);                                    \
            if (!nb) {                                                        \
                free(buf);                                                    \
                return NULL;                                                  \
            }                                                                 \
            buf = nb;                                                         \
        }                                                                     \
        snprintf(buf + len, cap - len, __VA_ARGS__);                          \
        len += (size_t)need;                                                  \
    } while (0)
    size_t ndefs = sizeof(slash_defs) / sizeof(slash_defs[0]);
    for (size_t i = 0; i < ndefs; i++) {
        if (i)
            APPEND_FMT(",");
        if (slash_defs[i].options_json)
            APPEND_FMT("{\"name\":\"%s\",\"description\":\"%s\",\"options\":%s}",
                       slash_defs[i].name, slash_defs[i].desc,
                       slash_defs[i].options_json);
        else
            APPEND_FMT("{\"name\":\"%s\",\"description\":\"%s\"}",
                       slash_defs[i].name, slash_defs[i].desc);
    }
    APPEND_FMT("]");
#undef APPEND_FMT
    return buf;
}

static void ctx_from_msg(cmd_ctx_t *ctx, discord_client_t *c, bot_state_t *st,
                         const disc_message_t *m) {
    memset(ctx, 0, sizeof *ctx);
    ctx->dc = c;
    ctx->st = st;
    ctx->is_slash = false;
    ctx->msg = m;
    ctx->channel_id = m->channel_id;
    ctx->guild_id = m->guild_id;
    ctx->author_id = m->author_id;
    const char *nick = m->member_nick;
    if (!nick)
        nick = m->global_name;
    if (!nick)
        nick = m->author_name;
    snprintf(ctx->author_name, sizeof ctx->author_name, "%s",
             nick ? nick : "someone");
}

/* prefix "/user add @x" args -> action + target (mentions first) */
static void user_target_from_prefix(const disc_message_t *m, const char *arg,
                                    uint64_t *id_out, char *name_out,
                                    size_t name_cap) {
    *id_out = 0;
    if (name_out)
        name_out[0] = '\0';
    if (m->n_mentions > 0) {
        *id_out = m->mentions[0];
        /* username unknown from mention alone; leave name empty */
        return;
    }
    /* second whitespace token */
    const char *p = arg;
    while (*p && *p != ' ' && *p != '\t')
        p++;
    while (*p == ' ' || *p == '\t')
        p++;
    if (!*p)
        return;
    char tok[64];
    size_t i = 0;
    while (p[i] && p[i] != ' ' && p[i] != '\t' && i + 1 < sizeof tok) {
        tok[i] = p[i];
        i++;
    }
    tok[i] = '\0';
    *id_out = parse_target_id(tok);
    if (name_out)
        snprintf(name_out, name_cap, "%s", tok);
}

void handle_prefix(discord_client_t *c, bot_state_t *st,
                   const disc_message_t *m) {
    char name[64];
    const char *arg = NULL;
    if (cmd_parse_prefix(m->content, name, sizeof name, &arg) != 0)
        return;
    cmd_ctx_t ctx;
    ctx_from_msg(&ctx, c, st, m);
    if (strcmp(name, "help") == 0)
        cmd_help(&ctx, arg);
    else if (strcmp(name, "ps") == 0)
        cmd_ps(&ctx, arg);
    else if (strcmp(name, "status") == 0)
        cmd_status(&ctx, arg);
    else if (strcmp(name, "start") == 0)
        cmd_start(&ctx, arg);
    else if (strcmp(name, "stop") == 0)
        cmd_stop(&ctx, arg);
    else if (strcmp(name, "restart") == 0)
        cmd_restart_vm(&ctx, arg);
    else if (strcmp(name, "info") == 0)
        cmd_info(&ctx, arg);
    else if (strcmp(name, "run") == 0)
        cmd_run(&ctx, arg);
    else if (strcmp(name, "send") == 0)
        cmd_send(&ctx, arg);
    else if (strcmp(name, "upload") == 0) {
        if (!m->n_attachments) {
            ctx_reply(&ctx, "Attach a file: `/upload <file> <dir>`.");
            return;
        }
        cmd_upload(&ctx, arg, m->attachments[0].url,
                   m->attachments[0].filename, m->attachments[0].size);
    } else if (strcmp(name, "shell") == 0)
        cmd_shell(&ctx, arg);
    else if (strcmp(name, "botrestart") == 0)
        cmd_botrestart(&ctx, arg);
    else if (strcmp(name, "user") == 0) {
        uint64_t tid = 0;
        char tname[64] = "";
        user_target_from_prefix(m, arg, &tid, tname, sizeof tname);
        /* action is the first token of arg */
        char action[16] = "list";
        sscanf(arg, "%15s", action);
        cmd_user(&ctx, action, tid, tname);
    } else if (strcmp(name, "admin") == 0) {
        uint64_t tid = 0;
        user_target_from_prefix(m, arg, &tid, NULL, 0);
        char action[16] = "list";
        sscanf(arg, "%15s", action);
        cmd_admin(&ctx, action, tid);
    } else if (strcmp(name, "notify") == 0)
        cmd_notify(&ctx, arg);
    else if (strcmp(name, "purge_replies") == 0) {
        char target[64] = "";
        int limit = 50;
        sscanf(arg, "%63s %i", target, &limit);
        cmd_purge_replies(&ctx, target, limit);
    } else if (strcmp(name, "warmode") == 0)
        cmd_warmode(&ctx, arg);
    else if (strcmp(name, "sayas") == 0)
        cmd_sayas(&ctx, arg, "", m->attachments, m->n_attachments);
    else if (strcmp(name, "ai") == 0)
        cmd_ai(&ctx, arg);
    else if (strcmp(name, "ask") == 0)
        cmd_ask(&ctx, arg);
}

void handle_interaction(discord_client_t *c, bot_state_t *st,
                        const disc_interaction_t *in) {
    /* Defer-first (Discord 3s rule), then follow up. */
    if (discord_interaction_defer(c, in, false) != 0)
        return;
    if (!in->command)
        return;
    cmd_ctx_t ctx;
    memset(&ctx, 0, sizeof ctx);
    ctx.dc = c;
    ctx.st = st;
    ctx.is_slash = true;
    ctx.inter = in;
    ctx.channel_id = in->channel_id;
    ctx.guild_id = in->guild_id;
    ctx.author_id = in->author_id;
    const char *nick = in->member_nick;
    if (!nick)
        nick = in->author_name;
    snprintf(ctx.author_name, sizeof ctx.author_name, "%s",
             nick ? nick : "someone");
    const char *cmd = in->command;
    if (strcmp(cmd, "help") == 0)
        cmd_help(&ctx, "");
    else if (strcmp(cmd, "ps") == 0)
        cmd_ps(&ctx, "");
    else if (strcmp(cmd, "status") == 0)
        cmd_status(&ctx, "");
    else if (strcmp(cmd, "start") == 0)
        cmd_start(&ctx, "");
    else if (strcmp(cmd, "stop") == 0)
        cmd_stop(&ctx, "");
    else if (strcmp(cmd, "restart") == 0)
        cmd_restart_vm(&ctx, "");
    else if (strcmp(cmd, "info") == 0)
        cmd_info(&ctx, "");
    else if (strcmp(cmd, "run") == 0)
        cmd_run(&ctx, cmd_opt_str(in, "cmd"));
    else if (strcmp(cmd, "send") == 0)
        cmd_send(&ctx, cmd_opt_str(in, "path"));
    else if (strcmp(cmd, "upload") == 0) {
        const disc_attachment_t *a = cmd_opt_attachment(in, "file");
        if (!a) {
            ctx_reply(&ctx, "Attach a file: `/upload <file> <dir>`.");
            return;
        }
        cmd_upload(&ctx, cmd_opt_str(in, "dir"), a->url, a->filename, a->size);
    } else if (strcmp(cmd, "shell") == 0)
        cmd_shell(&ctx, cmd_opt_str(in, "name"));
    else if (strcmp(cmd, "botrestart") == 0)
        cmd_botrestart(&ctx, "");
    else if (strcmp(cmd, "user") == 0) {
        const char *uname = "";
        uint64_t tid = cmd_opt_user(in, "user", &uname);
        cmd_user(&ctx, cmd_opt_str(in, "action"), tid, uname);
    } else if (strcmp(cmd, "admin") == 0) {
        uint64_t tid = cmd_opt_user(in, "user", NULL);
        cmd_admin(&ctx, cmd_opt_str(in, "action"), tid);
    } else if (strcmp(cmd, "notify") == 0)
        cmd_notify(&ctx, cmd_opt_str(in, "what"));
    else if (strcmp(cmd, "purge_replies") == 0) {
        bool present = false;
        long long lim = cmd_opt_int(in, "limit", &present);
        cmd_purge_replies(&ctx, cmd_opt_str(in, "target"),
                          present ? (int)lim : 50);
    } else if (strcmp(cmd, "warmode") == 0) {
        bool present = false;
        bool on = cmd_opt_bool(in, "enabled", &present);
        cmd_warmode(&ctx, on ? "true" : "false");
    } else if (strcmp(cmd, "sayas") == 0) {
        const disc_attachment_t *a = cmd_opt_attachment(in, "file");
        cmd_sayas(&ctx, cmd_opt_str(in, "message"), cmd_opt_str(in, "reply_to"),
                  a, a ? 1 : 0);
    } else if (strcmp(cmd, "ai") == 0) {
        bool he = false, hf = false, ht = false;
        bool e = cmd_opt_bool(in, "enabled", &he);
        bool f = cmd_opt_bool(in, "forget", &hf);
        bool t = cmd_opt_bool(in, "think", &ht);
        const char *model = cmd_opt_str(in, "model");
        const char *prompt = cmd_opt_str(in, "prompt");
        cmd_ai_opts(&ctx, he, e, model[0] ? model : NULL,
                    prompt[0] ? prompt : NULL, hf, f, ht, t);
    } else if (strcmp(cmd, "ask") == 0)
        cmd_ask(&ctx, cmd_opt_str(in, "question"));
}
