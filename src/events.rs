use poise::serenity_prelude as serenity;

use crate::{
    commands::download_sayas_files,
    config::{access_allowed, Data},
    util::{attach_name, cap_file_body, strip_sgr},
    vm::linked_user,
    webhook::{handle_delete, is_own_message, is_posted_message, post_message},
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
    if let serenity::FullEvent::TypingStart { event } = event {
        let name = event
            .member
            .as_ref()
            .and_then(|m| m.nick.clone().or_else(|| m.user.global_name.clone()).or_else(|| Some(m.user.name.clone())))
            .or_else(|| {
                ctx.cache
                    .user(event.user_id)
                    .map(|u| u.global_name.clone().unwrap_or_else(|| u.name.clone()))
            })
            .unwrap_or_else(|| event.user_id.get().to_string());
        let channel_name = ctx
            .cache
            .channel(event.channel_id)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let _ = crate::feed::log_typing(&name, event.channel_id.get(), &channel_name).await;
        return Ok(());
    }
    let serenity::FullEvent::Message { new_message } = event else {
        return Ok(());
    };
    let channel_name = ctx
        .cache
        .channel(new_message.channel_id)
        .map(|c| c.name.clone())
        .unwrap_or_default();
    let _ = crate::feed::log_message(
        &new_message.author.name,
        new_message.author.bot,
        new_message.channel_id.get(),
        &channel_name,
        &new_message.content,
    )
    .await;
    let id = new_message.author.id.get();
    let (owner, blocked, war) = {
        let a = data.allowed.read().await;
        let s = data.settings.read().await;
        (
            id == a.owner || a.admins.contains(&id),
            a.blocked.contains(&id),
            s.war_mode,
        )
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
    let me_id: u64 = ctx.cache.current_user().id.get();
    if is_own_message(new_message.author.id, new_message.id, serenity::UserId::new(me_id)) {
        return Ok(());
    }
    let mentioned = new_message.mentions.iter().any(|u| u.id.get() == me_id)
        || new_message.content.contains(&format!("<@{me_id}>"))
        || new_message.content.contains(&format!("<@!{me_id}>"))
        || crate::ai::mentions_name(&new_message.content);
    if mentioned {
        let prompt0 = crate::ai::strip_name(&crate::ai::strip_mention(&new_message.content, me_id));
        let is_command = prompt0.starts_with('/') || prompt0.starts_with(';');
        if !is_command {
            let blocked = {
                let a = data.allowed.read().await;
                a.blocked.contains(&id)
            };
            if blocked {
                return Ok(());
            }
            let (ai_on, ai_model, ai_host, ai_key) = {
                let s = data.settings.read().await;
                (
                    s.ai_enabled,
                    s.ai_model.clone(),
                    crate::ai::resolve_host(&s.ollama_host),
                    crate::ai::resolve_ollama_key(&s.ollama_api_key),
                )
            };
            if !ai_on {
                let _ = new_message
                    .reply(&ctx.http, "AI is off — the owner runs `/ai true model:<name>` to enable me.")
                    .await;
                return Ok(());
            }
            let mut prompt = prompt0.clone();
            let display = new_message
                .member
                .as_ref()
                .and_then(|m| m.nick.clone())
                .or_else(|| new_message.author.global_name.clone())
                .unwrap_or_else(|| new_message.author.name.clone());
            let speaker = if display == new_message.author.name {
                display
            } else {
                format!("{display} (@{})", new_message.author.name)
            };
            if prompt.trim().is_empty() {
                if let Some(r) = new_message.referenced_message.as_ref() {
                    let qdisplay = r
                        .author
                        .global_name
                        .clone()
                        .unwrap_or_else(|| r.author.name.clone());
                    let qtext = r.content.trim().to_string();
                    if !qtext.is_empty() {
                        prompt = format!(
                            "(quoting {}): {qtext}",
                            crate::ai::speaker_tag(&qdisplay)
                        );
                    }
                }
            }
            if prompt.trim().is_empty() {
                let _ = new_message
                    .reply(&ctx.http, format!("Ping me with a question — `@artixy <question>` or `artixy <question>` (model `{ai_model}`)."))
                    .await;
                return Ok(());
            }
            if prompt.chars().count() > 4000 {
                prompt = prompt.chars().take(4000).collect();
            }
            let _ = new_message.channel_id.broadcast_typing(&ctx.http).await;
            match crate::ai::ollama_chat(&ai_host, &ai_model, &ai_key, new_message.channel_id.get(), &speaker, &prompt).await {
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
                    let text = if crate::ai::is_api_full_err(&e.to_string()) {
                        crate::ai::api_full_message()
                    } else {
                        crate::ai::glitch_text(&ai_host, &ai_model).await
                    };
                    let _ = new_message.reply(&ctx.http, &text).await;
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
                let vm = data.vm.read().await.clone();
                let ok =
                    crate::live::forward_terminal_input(&vm, &fifo, runas.as_deref(), &payload).await;
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
            crate::ai::record_artixy(new_message.channel_id.get(), &body);
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
    let (mut ar_files, _) = download_sayas_files(&new_message.attachments).await;
    let mut ar_body = text.clone();
    if ar_body.chars().count() > 2000 {        ar_files.insert(
            0,
            (
                attach_name(&ar_body),
                cap_file_body(&strip_sgr(&ar_body)).into_bytes(),
            ),
        );
        ar_body = String::new();
    }
    crate::ai::record_artixy(new_message.channel_id.get(), &ar_body);
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

