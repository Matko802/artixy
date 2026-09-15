use poise::serenity_prelude as serenity;

use crate::{
    commands::download_sayas_files,
    config::{access_allowed, Data},
    util::{attach_name, cap_file_body, strip_sgr},
    vm::linked_user,
    webhook::{handle_delete, is_posted_message, post_message},
    Error,
};

fn is_boo_message(s: &str) -> bool {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "boo")
}

const TAUNT: &str = "purged your message haha";

fn blocked_reply_action(
    ref_author: Option<u64>,
    ref_id: Option<serenity::MessageId>,
    ref_content: &str,
    me: u64,
) -> (bool, bool) {
    let mine = matches!(ref_author, Some(a) if a == me)
        || ref_id.map(|id| is_posted_message(&id)).unwrap_or(false);
    match mine {
        true => (true, ref_content != TAUNT),
        false => (false, false),
    }
}
fn artixy_text(s: &str) -> Option<String> {
    let text = s.trim_end().strip_suffix(".ar")?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
}

pub(crate) async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    if let serenity::FullEvent::MessageDelete {
        deleted_message_id,
        ..
    } = event
    {
        let war = data.settings.read().await.war_mode;
        handle_delete(&ctx.http, *deleted_message_id, war).await;
        return Ok(());
    }
    let serenity::FullEvent::Message { new_message } = event else {
        return Ok(());
    };
    let id = new_message.author.id.get();
    let (owner, blocked, war) = {
        let a = data.allowed.read().await;
        let s = data.settings.read().await;
        (id == a.owner, a.blocked.contains(&id), s.war_mode)
    };
    if blocked && war {
        let me = match ctx.http.get_current_user().await {
            Ok(u) => u.id.get(),
            Err(_) => return Ok(()),
        };
        let (delete, taunt) = match &new_message.referenced_message {
            Some(r) => blocked_reply_action(Some(r.author.id.get()), Some(r.id), &r.content, me),
            None => match &new_message.message_reference {
                Some(r) => match r.message_id {
                    Some(mid) => match new_message.channel_id.message(&ctx.http, mid).await {
                        Ok(orig) => blocked_reply_action(
                            Some(orig.author.id.get()),
                            Some(orig.id),
                            &orig.content,
                            me,
                        ),
                        Err(_) => (false, false),
                    },
                    None => (false, false),
                },
                None => (false, false),
            },
        };
        if delete {
            match new_message.delete(&ctx.http).await {
                Ok(_) => {
                    if taunt {
                        let _ = post_message(&ctx.http, new_message.channel_id, TAUNT.into(), Vec::new()).await;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "guard: failed to delete blocked reply {} in {}: {}",
                        new_message.id.get(),
                        new_message.channel_id.get(),
                        e
                    );
                }
            }
        }
        return Ok(());
    }
    if new_message.author.bot {
        return Ok(());
    }
    // --- Ollama AI chat when the bot is pinged (@artixy <question>) ---
    let me_id: u64 = ctx.cache.current_user().id.get();
    let mentioned = new_message.mentions.iter().any(|u| u.id.get() == me_id)
        || new_message.content.contains(&format!("<@{me_id}>"))
        || new_message.content.contains(&format!("<@!{me_id}>"));
    if mentioned {
        let prompt0 = crate::ai::strip_mention(&new_message.content, me_id);
        // Let real commands through: `@artixy /run x` / `@artixy ;shell` etc.
        let is_command = prompt0.starts_with('/') || prompt0.starts_with(';');
        if !is_command {
            let authed = {
                let a = data.allowed.read().await;
                access_allowed(a.owner, &a.users, &a.blocked, id)
            };
            if !authed {
                return Ok(());
            }
            let (ai_on, ai_model, ai_host) = {
                let s = data.settings.read().await;
                (s.ai_enabled, s.ai_model.clone(), crate::ai::resolve_host(&s.ollama_host))
            };
            if !ai_on {
                let _ = new_message
                    .reply(&ctx.http, "AI is off — the owner runs `/ai true model:<name>` to enable me.")
                    .await;
                return Ok(());
            }
            let mut prompt = prompt0.clone();
            if prompt.trim().is_empty() {
                if let Some(r) = new_message.referenced_message.as_ref() {
                    prompt = r.content.trim().to_string();
                }
            }
            if prompt.trim().is_empty() {
                let _ = new_message
                    .reply(&ctx.http, format!("Ping me with a question — `@artixy <question>` (model `{ai_model}`)."))
                    .await;
                return Ok(());
            }
            if prompt.chars().count() > 4000 {
                prompt = prompt.chars().take(4000).collect();
            }
            let _ = new_message.channel_id.broadcast_typing(&ctx.http).await;
            match crate::ai::ollama_chat(&ai_host, &ai_model, &prompt).await {
                Ok(text) => {
                    let chunks = crate::ai::chunk_reply(&text);
                    let mut first = true;
                    for c in chunks {
                        if first {
                            let _ = new_message.reply(&ctx.http, &c).await;
                            first = false;
                        } else if post_message(&ctx.http, new_message.channel_id, c, Vec::new()).await.is_none() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("ai chat failed (model {ai_model} on {ai_host}): {e}");
                    let _ = new_message
                        .reply(&ctx.http, format!("Ollama chat failed (`{ai_model}` on `{ai_host}`): {e}"))
                        .await;
                }
            }
            return Ok(());
        }
    }
    if new_message.attachments.is_empty() && !new_message.content.trim().is_empty() {
        let refd_id = new_message
            .referenced_message
            .as_ref()
            .map(|r| r.id)
            .or(new_message.message_reference.as_ref().and_then(|r| r.message_id));
        if let Some(target_id) = refd_id {
            let live_hit = {
                let m = data.live.lock().await;
                m.iter()
                    .find(|(_, e)| e.msg_id == target_id)
                    .map(|(_, e)| (e.author_id, e.in_f.clone(), e.channel))
            };
            if let Some((owner_id, fifo_opt, live_channel)) = live_hit {
                if owner_id.get() != id {
                    let _ = new_message.delete(&ctx.http).await;
                    let dm_text = "That live session belongs to someone else — typing into it is blocked.";
                    if let Ok(dm) = new_message.author.create_dm_channel(&ctx.http).await {
                        let _ = dm
                            .send_message(
                                &ctx.http,
                                serenity::CreateMessage::new().content(dm_text),
                            )
                            .await;
                    }
                    return Ok(());
                }
                let Some(fifo) = fifo_opt else {
                    let _ = new_message.delete(&ctx.http).await;
                    if let Ok(dm) = new_message.author.create_dm_channel(&ctx.http).await {
                        let _ = dm
                            .send_message(
                                &ctx.http,
                                serenity::CreateMessage::new()
                                    .content("That live session isn't interactive (fifo missing)."),
                            )
                            .await;
                    }
                    return Ok(());
                };
                let authed = {
                    let a = data.allowed.read().await;
                    access_allowed(a.owner, &a.users, &a.blocked, id)
                };
                if !authed {
                    return Ok(());
                }
                let trimmed = new_message.content.trim_end();
                if trimmed.is_empty() {
                    return Ok(());
                }
                let payload = crate::live::expand_typed_input(trimmed);
                let runas = linked_user(data, id).await;
                let ok =
                    crate::live::forward_terminal_input(&data.vm, &fifo, runas.as_deref(), &payload).await;
                let _ = new_message.delete(&ctx.http).await;
                if !ok {
                    if let Ok(dm) = new_message.author.create_dm_channel(&ctx.http).await {
                        let _ = dm
                            .send_message(
                                &ctx.http,
                                serenity::CreateMessage::new()
                                    .content("Couldn't type into that session (it just ended)."),
                            )
                            .await;
                    }
                }
                let _ = live_channel;
                return Ok(());
            }
        }
    }
    if !owner {
        if war && is_boo_message(&new_message.content) {
            let _ = new_message.reply(&ctx.http, "boo on you! :3").await;
        }
        return Ok(());
    }
    if data.settings.read().await.sayas_enabled {
        let content = new_message.content.trim().to_string();
        let has_files = !new_message.attachments.is_empty();
        let plain_text = !content.is_empty()
            && !content.starts_with('/')
            && !content.starts_with(';')
            && !content.ends_with(".ar");
        if plain_text || (has_files && content.is_empty()) {
            // Download first so files-only messages (empty text) still forward.
            let (mut files, _) = download_sayas_files(&new_message.attachments).await;
            let mut body = content.clone();
            if body.chars().count() > 2000 {
                files.insert(
                    0,
                    (
                        attach_name(&body),
                        cap_file_body(&strip_sgr(&body)).into_bytes(),
                    ),
                );
                body = String::new();
            }
            if body.is_empty() && files.is_empty() {
                return Ok(());
            }
            let _ = new_message.delete(&ctx.http).await;
            if body.chars().count() <= 2000 && !body.is_empty() && files.is_empty() {
                match &new_message.referenced_message {
                    Some(target) => {
                        let _ = target.reply(&ctx.http, &body).await;
                    }
                    None => {
                        let _ = post_message(&ctx.http, new_message.channel_id, body, Vec::new()).await;
                    }
                }
            } else {
                match &new_message.referenced_message {
                    Some(target) => {
                        let mut builder = serenity::CreateMessage::new()
                            .reference_message((new_message.channel_id, target.id));
                        if !body.is_empty() {
                            builder = builder.content(&body);
                        }
                        for (name, bytes) in files {
                            builder = builder.add_file(serenity::CreateAttachment::bytes(bytes, name));
                        }
                        let _ = new_message
                            .channel_id
                            .send_message(&ctx.http, builder)
                            .await;
                    }
                    None => {
                        let _ = post_message(&ctx.http, new_message.channel_id, body, files).await;
                    }
                }
            }
            return Ok(());
        }
    }
    let Some(text) = artixy_text(&new_message.content) else {
        return Ok(());
    };
    let mut deleted = false;
    for _ in 0..3 {
        match new_message.delete(&ctx.http).await {
            Ok(_) => {
                deleted = true;
                break;
            }
            Err(e) => {
                eprintln!(
                    "artixy-say: delete attempt failed for {} in {}: {}",
                    new_message.id.get(),
                    new_message.channel_id.get(),
                    e
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
    if !deleted {
        eprintln!(
            "artixy-say: giving up deleting {} (needs Manage Messages there)",
            new_message.id.get()
        );
    }
    // Files stuck onto the `.ar` message ride along as artix too.
    let (mut ar_files, _) = download_sayas_files(&new_message.attachments).await;
    let mut ar_body = text.clone();
    if ar_body.chars().count() > 2000 {
        ar_files.insert(
            0,
            (
                attach_name(&ar_body),
                cap_file_body(&strip_sgr(&ar_body)).into_bytes(),
            ),
        );
        ar_body = String::new();
    }
    if ar_body.chars().count() <= 2000 && !ar_body.is_empty() && ar_files.is_empty() {
        match &new_message.referenced_message {
            Some(target) => {
                let _ = target.reply(&ctx.http, &ar_body).await;
            }
            None => {
                let _ = post_message(&ctx.http, new_message.channel_id, ar_body, Vec::new()).await;
            }
        }
    } else if ar_body.is_empty() && ar_files.is_empty() {
        // Nothing downloadable — nothing to repost.
    } else {
        match &new_message.referenced_message {
            Some(target) => {
                let mut builder = serenity::CreateMessage::new()
                    .reference_message((new_message.channel_id, target.id));
                if !ar_body.is_empty() {
                    builder = builder.content(&ar_body);
                }
                for (name, bytes) in ar_files {
                    builder = builder.add_file(serenity::CreateAttachment::bytes(bytes, name));
                }
                let _ = new_message
                    .channel_id
                    .send_message(&ctx.http, builder)
                    .await;
            }
            None => {
                let _ = post_message(&ctx.http, new_message.channel_id, ar_body, ar_files).await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boo_matches_whole_word_case_insensitive() {
        assert!(is_boo_message("boo"));
        assert!(is_boo_message("Boo"));
        assert!(is_boo_message("BOO!"));
        assert!(is_boo_message("well, boo."));
        assert!(!is_boo_message("book"));
        assert!(!is_boo_message("bamboo"));
        assert!(!is_boo_message("taboo"));
        assert!(!is_boo_message(""));
        assert!(!is_boo_message("hello there"));
    }

    #[test]
    fn blocked_reply_verdicts() {
        assert_eq!(blocked_reply_action(Some(9), None, "trash talk", 9), (true, true));
        assert_eq!(
            blocked_reply_action(Some(9), None, "purged your message haha", 9),
            (true, false)
        );
        assert_eq!(blocked_reply_action(Some(3), None, "trash talk", 9), (false, false));
        assert_eq!(blocked_reply_action(None, None, "trash talk", 9), (false, false));
        assert_eq!(blocked_reply_action(None, None, "", 9), (false, false));
    }

    #[test]
    fn artixy_suffix_returns_plain_text() {        assert_eq!(artixy_text("hello .ar"), Some("hello".into()));
        assert_eq!(artixy_text("hello .ar   "), Some("hello".into()));
        assert_eq!(artixy_text("a.ar"), Some("a".into()));
        assert_eq!(artixy_text(".ar"), None);
        assert_eq!(artixy_text("   .ar  "), None);
        assert_eq!(artixy_text(".ar hello"), None);
        assert_eq!(artixy_text("hello"), None);
        assert_eq!(artixy_text(""), None);
        assert_eq!(artixy_text(".AR"), None);
        assert_eq!(artixy_text("hello .artixy"), None);
    }
}
