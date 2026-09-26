/* Ambient message pipeline (ported 1:1 from events.rs). */
#include "events.h"
#include "ai.h"
#include "bot.h"
#include "commands.h"
#include "discord.h"
#include "live.h"
#include "util.h"
#include "vm.h"

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static const char *taunt = "purged your message haha";

bool events_is_boo(const char *s) {
    if (!s)
        return false;
    const char *p = s;
    while (*p) {
        while (*p && !(( *p >= 'a' && *p <= 'z') ||
                       (*p >= 'A' && *p <= 'Z') ||
                       (*p >= '0' && *p <= '9')))
            p++;
        if (!*p)
            break;
        const char *w = p;
        while (*p && ((*p >= 'a' && *p <= 'z') ||
                      (*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9')))
            p++;
        if ((size_t)(p - w) == 3 &&
            (w[0] == 'b' || w[0] == 'B') && (w[1] == 'o' || w[1] == 'O') &&
            (w[2] == 'o' || w[2] == 'O'))
            return true;
    }
    return false;
}

char *events_artixy_text(const char *content) {
    if (!content)
        return NULL;
    /* trim trailing whitespace */
    size_t n = strlen(content);
    while (n > 0 && (content[n - 1] == ' ' || content[n - 1] == '\t' ||
                     content[n - 1] == '\n' || content[n - 1] == '\r'))
        n--;
    /* NOTE: strncmp, not strcmp — trailing whitespace beyond n is untouched */
    if (n < 3 || strncmp(content + n - 3, ".ar", 3) != 0)
        return NULL;
    /* trim the suffix + surrounding space */
    size_t m = n - 3;
    while (m > 0 && (content[m - 1] == ' ' || content[m - 1] == '\t'))
        m--;
    if (!m)
        return NULL;
    char *out = xmalloc(m + 1);
    if (!out)
        return NULL;
    memcpy(out, content, m);
    out[m] = '\0';
    return out;
}

/* Post body+files as artix, honouring a reply target (0 = none). */
static void post_as_artix(discord_client_t *dc, uint64_t channel_id,
                          uint64_t reply_to, const char *body,
                          disc_file_t *files, size_t n_files) {
    const char *text = body ? body : "";
    if (reply_to)
        discord_send_reply(dc, channel_id, reply_to, text, files, n_files,
                           NULL);
    else
        discord_send_message(dc, channel_id, text, files, n_files, NULL);
}

void events_handle_message(discord_client_t *c, bot_state_t *st,
                           const disc_message_t *m) {
    if (m->from_webhook)
        return;

    uint64_t me = discord_bot_id(c);
    bool blocked = bot_is_blocked(st, m->author_id);
    bool owner_admin = bot_is_owner(st, m->author_id) ||
                       bot_is_elevated(st, m->author_id);
    bool war = false, sayas_auto = false;
    {
        pthread_rwlock_rdlock(&st->mu);
        war = st->war_mode;
        sayas_auto = st->sayas_enabled;
        pthread_rwlock_unlock(&st->mu);
    }

    /* war-mode guard: blocked users replying to the bot get purged */
    if (blocked && war) {
        /* resolve the replied-to message (inline or fetched) */
        uint64_t ref_author = 0;
        char *ref_content = NULL;
        if (m->has_ref_msg) {
            ref_author = m->ref_msg_author_id;
            ref_content = m->ref_msg_content ? xstrdup(m->ref_msg_content)
                                             : xstrdup("");
        } else if (m->has_reference) {
            disc_message_t orig;
            memset(&orig, 0, sizeof orig);
            if (discord_get_message(c, m->ref_channel_id, m->ref_message_id,
                                    &orig) == 0) {
                ref_author = orig.author_id;
                ref_content = orig.content ? xstrdup(orig.content)
                                           : xstrdup("");
                disc_message_free(&orig);
            }
        }
        if (ref_content) {
            if (ref_author == me) {
                discord_delete_message(c, m->channel_id, m->id);
                if (strcmp(ref_content, taunt) != 0)
                    discord_send_message(c, m->channel_id, taunt, NULL, 0,
                                         NULL);
            }
            free(ref_content);
        }
        return;
    }

    if (m->author_id == me)
        return;

    /* slash/prefix commands */
    {
        char name[64];
        const char *arg = NULL;
        if (cmd_parse_prefix(m->content ? m->content : "", name, sizeof name,
                             &arg) == 0) {
            handle_prefix(c, st, m);
            return;
        }
    }

    /* @artixy AI chat */
    if (ambient_mention(c, st, m))
        return;

    /* live-session typing: plain reply to a live message */
    if (!m->n_attachments && m->content && *m->content) {
        uint64_t target = 0;
        if (m->has_ref_msg && m->ref_msg_id)
            target = m->ref_msg_id;
        else if (m->has_reference)
            target = m->ref_message_id;
        if (target && st->live) {
            uint64_t owner_id = 0;
            char *fifo = NULL;
            if (live_session_for_msg(st->live, m->channel_id, target,
                                     &owner_id, &fifo)) {
                bool handled = true;
                if (owner_id != m->author_id) {
                    discord_delete_message(c, m->channel_id, m->id);
                    uint64_t dm = 0;
                    if (discord_create_dm(c, m->author_id, &dm) == 0 && dm)
                        discord_send_message(
                            c, dm,
                            "That live session belongs to someone else — "
                            "typing into it is blocked.",
                            NULL, 0, NULL);
                } else if (!fifo) {
                    discord_delete_message(c, m->channel_id, m->id);
                    uint64_t dm = 0;
                    if (discord_create_dm(c, m->author_id, &dm) == 0 && dm)
                        discord_send_message(
                            c, dm,
                            "That live session isn't interactive (fifo "
                            "missing).",
                            NULL, 0, NULL);
                } else if (!bot_is_authed(st, m->author_id)) {
                    handled = false;
                } else {
                    char trimmed[2048];
                    snprintf(trimmed, sizeof trimmed, "%s", m->content);
                    size_t tl = strlen(trimmed);
                    while (tl > 0 && (trimmed[tl - 1] == ' ' ||
                                      trimmed[tl - 1] == '\t' ||
                                      trimmed[tl - 1] == '\n'))
                        trimmed[--tl] = '\0';
                    if (!tl) {
                        free(fifo);
                        return;
                    }
                    char *payload = live_expand_typed_input(trimmed);
                    char vmname[256] = "";
                    char runas[64] = "";
                    {
                        pthread_rwlock_rdlock(&st->mu);
                        snprintf(vmname, sizeof vmname, "%s", st->vm);
                        pthread_rwlock_unlock(&st->mu);
                    }
                    const char *lx =
                        bot_linked_user(st, m->author_id);
                    if (bot_valid_runas(lx))
                        snprintf(runas, sizeof runas, "%s", lx);
                    bool ok = payload && live_forward_input(
                                           vmname, fifo,
                                           runas[0] ? runas : NULL, payload);
                    free(payload);
                    discord_delete_message(c, m->channel_id, m->id);
                    if (!ok) {
                        uint64_t dm = 0;
                        if (discord_create_dm(c, m->author_id, &dm) == 0 && dm)
                            discord_send_message(
                                c, dm,
                                "Couldn't type into that session (it just "
                                "ended).",
                                NULL, 0, NULL);
                    }
                }
                free(fifo);
                if (handled)
                    return;
            }
        }
    }

    if (!owner_admin) {
        if (war && events_is_boo(m->content ? m->content : "")) {
            discord_send_reply(c, m->channel_id, m->id, "boo on you! :3", NULL,
                               0, NULL);
        }
        return;
    }

    /* sayas-auto: plain owner/admin messages are reposted as artix */
    if (sayas_auto) {
        const char *content = m->content ? m->content : "";
        bool has_files = m->n_attachments > 0;
        /* trim leading check */
        while (*content == ' ' || *content == '\t')
            content++;
        char *ar_probe = events_artixy_text(m->content);
        bool is_ar = ar_probe != NULL;
        free(ar_probe);
        bool plain_text = *content && content[0] != '/' && content[0] != ';' &&
                          !is_ar;
        if (plain_text || (has_files && !*content)) {
            disc_file_t *files = NULL;
            size_t n_files = 0;
            char **bufs = NULL;
            if (has_files &&
                bot_download_atts(m->attachments, m->n_attachments, &files,
                                  &n_files, &bufs) != 0) {
                files = NULL;
                n_files = 0;
                bufs = NULL;
            }
            char body[2100];
            snprintf(body, sizeof body, "%s", content);
            if (strlen(body) > 2000) {
                /* overflow -> file (same shape as sayas) */
                char *clean = bot_strip_sgr(body);
                size_t need = (clean ? strlen(clean) : 0) + 1;
                char *fdata = xmalloc(need);
                disc_file_t *nf = NULL;
                char **nb = NULL;
                if (fdata) {
                    snprintf(fdata, need, "%s", clean ? clean : "");
                    nf = xrealloc(files, (n_files + 1) * sizeof *nf);
                    nb = xrealloc(bufs, (n_files + 1) * sizeof *nb);
                }
                free(clean);
                if (fdata && nf && nb) {
                    files = nf;
                    bufs = nb;
                    files[n_files].name = xstrdup("output.txt");
                    files[n_files].data = fdata;
                    files[n_files].len = strlen(fdata);
                    bufs[n_files] = fdata;
                    if (files[n_files].name)
                        n_files++;
                    else
                        free(fdata);
                } else {
                    free(fdata);
                }
                body[0] = '\0';
            }
            if (!body[0] && !n_files) {
                bot_free_dl_files(files, n_files, bufs);
                return;
            }
            discord_delete_message(c, m->channel_id, m->id);
            if (body[0])
                ai_record_artixy(m->channel_id, body);
            uint64_t reply_to = 0;
            if (m->has_ref_msg && m->ref_msg_id)
                reply_to = m->ref_msg_id;
            else if (m->has_reference)
                reply_to = m->ref_message_id;
            post_as_artix(c, m->channel_id, reply_to, body, files, n_files);
            bot_free_dl_files(files, n_files, bufs);
            return;
        }
    }

    /* trailing ".ar" posts the line as artix */
    {
        char *text = events_artixy_text(m->content ? m->content : "");
        if (!text)
            return;
        for (int i = 0; i < 3; i++) {
            if (discord_delete_message(c, m->channel_id, m->id) == 0)
                break;
            fprintf(stderr, "artixy-say: delete attempt failed for %llu\n",
                    (unsigned long long)m->id);
            struct timespec ts = { 1, 0 };
            nanosleep(&ts, NULL);
        }
        disc_file_t *files = NULL;
        size_t n_files = 0;
        char **bufs = NULL;
        if (m->n_attachments)
            bot_download_atts(m->attachments, m->n_attachments, &files,
                              &n_files, &bufs);
        char body[2100];
        snprintf(body, sizeof body, "%s", text);
        free(text);
        if (strlen(body) > 2000) {
            char *clean = bot_strip_sgr(body);
            size_t need = (clean ? strlen(clean) : 0) + 1;
            char *fdata = xmalloc(need);
            if (fdata) {
                snprintf(fdata, need, "%s", clean ? clean : "");
                disc_file_t *nf =
                    xrealloc(files, (n_files + 1) * sizeof *nf);
                char **nb = xrealloc(bufs, (n_files + 1) * sizeof *nb);
                if (nf && nb) {
                    files = nf;
                    bufs = nb;
                    files[n_files].name = xstrdup("output.txt");
                    files[n_files].data = fdata;
                    files[n_files].len = strlen(fdata);
                    bufs[n_files] = fdata;
                    if (files[n_files].name)
                        n_files++;
                    else
                        free(fdata);
                } else {
                    free(fdata);
                }
            }
            free(clean);
            body[0] = '\0';
        }
        if (body[0])
            ai_record_artixy(m->channel_id, body);
        uint64_t reply_to = 0;
        if (m->has_ref_msg && m->ref_msg_id)
            reply_to = m->ref_msg_id;
        else if (m->has_reference)
            reply_to = m->ref_message_id;
        if (body[0] || n_files)
            post_as_artix(c, m->channel_id, reply_to, body, files, n_files);
        bot_free_dl_files(files, n_files, bufs);
    }
}
