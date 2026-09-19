use poise::serenity_prelude as serenity;

use crate::{Context, Error};

// Direct-send helpers (no Discord webhook impersonation).
// Previously this module managed webhook URL pools, a posted-message
// registry, and war-mode reposts. Now everything posts/edits as the bot.

pub(crate) fn is_own_message(
    author: serenity::UserId,
    _id: serenity::MessageId,
    me: serenity::UserId,
) -> bool {
    author == me
}

pub(crate) async fn post_message(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    content: String,
    files: Vec<(String, Vec<u8>)>,
) -> Option<serenity::Message> {
    let mut builder = serenity::CreateMessage::new();
    if !content.is_empty() {
        builder = builder.content(&content);
    }
    for (name, bytes) in files {
        builder = builder.add_file(serenity::CreateAttachment::bytes(bytes, name));
    }
    channel.send_message(http, builder).await.ok()
}

pub(crate) async fn edit_posted(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    target: serenity::MessageId,
    content: String,
    files: Vec<(String, Vec<u8>)>,
) -> bool {
    let mut builder = serenity::EditMessage::new().content(content);
    for (name, bytes) in files {
        builder = builder.new_attachment(serenity::CreateAttachment::bytes(bytes, name));
    }
    channel.edit_message(http, target, builder).await.is_ok()
}

pub(crate) async fn edit_cleared(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    target: serenity::MessageId,
    content: String,
) -> bool {
    let builder = serenity::EditMessage::new()
        .content(content)
        .remove_all_attachments();
    channel.edit_message(http, target, builder).await.is_ok()
}

pub(crate) async fn post_text(ctx: Context<'_>, content: impl Into<String>) -> Result<serenity::Message, Error> {
    post_response(ctx, content.into(), Vec::new()).await
}

pub(crate) async fn post_denied(ctx: Context<'_>, content: &str) -> Result<serenity::Message, Error> {
    let http = ctx.serenity_context().http.clone();
    let text = format!("<@{}> {}", ctx.author().id.get(), content);
    match ctx {
        poise::Context::Prefix(pctx) => Ok(pctx.msg.reply_ping(&http, text).await?),
        _ => {
            let _ = ctx.defer().await;
            let mentions = serenity::CreateAllowedMentions::new()
                .all_users(true)
                .all_roles(false)
                .everyone(false);
            let builder = serenity::CreateMessage::new()
                .content(text)
                .allowed_mentions(mentions);
            let msg = ctx.channel_id().send_message(&http, builder).await?;
            if let poise::Context::Application(actx) = ctx {
                let _ = actx.interaction.delete_response(&http).await;
            }
            Ok(msg)
        }
    }
}

pub(crate) async fn post_response(
    ctx: Context<'_>,
    content: String,
    files: Vec<(String, Vec<u8>)>,
) -> Result<serenity::Message, Error> {
    let http = ctx.serenity_context().http.clone();
    let channel = ctx.channel_id();
    let _ = ctx.defer().await;
    let mut builder = serenity::CreateMessage::new();
    if !content.is_empty() {
        builder = builder.content(&content);
    }
    for (name, bytes) in files.clone() {
        builder = builder.add_file(serenity::CreateAttachment::bytes(bytes, name));
    }
    match channel.send_message(&http, builder).await {
        Ok(msg) => {
            if let poise::Context::Application(actx) = ctx {
                let _ = actx.interaction.delete_response(&http).await;
            }
            Ok(msg)
        }
        Err(_) => {
            let mut reply = poise::CreateReply::default();
            if !content.is_empty() {
                reply = reply.content(content);
            }
            for (name, bytes) in files {
                reply = reply.attachment(serenity::CreateAttachment::bytes(bytes, name));
            }
            Ok(ctx.send(reply).await?.into_message().await?)
        }
    }
}
