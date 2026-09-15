use poise::serenity_prelude as serenity;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{atomic::{AtomicUsize, Ordering}, Mutex, OnceLock};

use crate::{Context, Error};

const POOL_SIZE: usize = 5;
const REGISTRY_CAP: usize = 500;
const REPOST_CAP: u8 = 3;
const REPOST_COOLDOWN_SECS: u64 = 5;
const REGISTRY_TTL_SECS: u64 = 1800;
const MAX_STORED_FILE_BYTES: usize = 1_000_000;
const PERSONA: &str = "artixy";

pub(crate) struct PostedEntry {
    channel: serenity::ChannelId,
    content: String,
    files: Vec<(String, Vec<u8>)>,
    generation: u8,
    posted_at: std::time::Instant,
}

struct PostedRegistry {
    order: VecDeque<serenity::MessageId>,
    entries: HashMap<serenity::MessageId, PostedEntry>,
}

impl PostedRegistry {
    fn register(&mut self, id: serenity::MessageId, entry: PostedEntry) {
        self.entries.insert(id, entry);
        self.order.push_back(id);
        while self.entries.len() > REGISTRY_CAP {
            match self.order.pop_front() {
                Some(old) => {
                    self.entries.remove(&old);
                }
                None => break,
            }
        }
    }

    fn take(&mut self, id: &serenity::MessageId) -> Option<PostedEntry> {
        let entry = self.entries.remove(id)?;
        self.order.retain(|x| x != id);
        Some(entry)
    }

    fn contains(&self, id: &serenity::MessageId) -> bool {
        self.entries.contains_key(id)
    }
}

#[derive(Clone)]
pub(crate) enum Poster {
    Direct,
    Hook {
        id: serenity::WebhookId,
        token: String,
    },
}

static WEBHOOK_URLS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static RESOLVED: OnceLock<Mutex<HashMap<serenity::ChannelId, Vec<(serenity::WebhookId, String)>>>> =
    OnceLock::new();
static POSTED: OnceLock<Mutex<PostedRegistry>> = OnceLock::new();
static SELF_DELETED: OnceLock<Mutex<HashSet<serenity::MessageId>>> = OnceLock::new();
static LAST_REPOST: OnceLock<Mutex<Option<std::time::Instant>>> = OnceLock::new();
static NEXT_HOOK: AtomicUsize = AtomicUsize::new(0);

fn resolved_map(
) -> &'static Mutex<HashMap<serenity::ChannelId, Vec<(serenity::WebhookId, String)>>> {
    RESOLVED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn posted_registry() -> &'static Mutex<PostedRegistry> {
    POSTED.get_or_init(|| {
        Mutex::new(PostedRegistry {
            order: VecDeque::new(),
            entries: HashMap::new(),
        })
    })
}

fn self_deleted_set() -> &'static Mutex<HashSet<serenity::MessageId>> {
    SELF_DELETED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn last_repost() -> &'static Mutex<Option<std::time::Instant>> {
    LAST_REPOST.get_or_init(|| Mutex::new(None))
}

fn webhook_urls_map() -> &'static Mutex<Vec<String>> {
    WEBHOOK_URLS.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn init_webhook_urls(urls: Vec<String>) {
    *webhook_urls_map().lock().unwrap_or_else(|e| e.into_inner()) = urls;
}

pub(crate) fn set_webhook_urls(urls: Vec<String>) {
    *webhook_urls_map().lock().unwrap_or_else(|e| e.into_inner()) = urls;
    resolved_map().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

pub(crate) fn current_webhook_urls() -> Vec<String> {
    webhook_urls_map().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn parse_webhook_url(s: &str) -> Option<(serenity::WebhookId, String)> {
    let t = s.trim().trim_end_matches('/');
    for host in [
        "https://discord.com/api/webhooks/",
        "https://discordapp.com/api/webhooks/",
    ] {
        if let Some(rest) = t.strip_prefix(host) {
            let mut parts = rest.splitn(2, '/');
            let id = parts.next()?.parse::<u64>().ok()?;
            let token = parts.next()?;
            if id == 0 || token.is_empty() || token.contains('/') {
                return None;
            }
            return Some((serenity::WebhookId::new(id), token.to_string()));
        }
    }
    None
}

fn should_recognize(author_is_me: bool, registered: bool) -> bool {
    author_is_me || registered
}

pub(crate) fn is_posted_message(id: &serenity::MessageId) -> bool {
    posted_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(id)
}

pub(crate) fn is_own_message(
    author: serenity::UserId,
    id: serenity::MessageId,
    me: serenity::UserId,
) -> bool {
    should_recognize(author == me, is_posted_message(&id))
}

fn repost_budget(generation: u8, age_secs: u64, cooldown_ok: bool) -> bool {
    generation < REPOST_CAP && age_secs < REGISTRY_TTL_SECS && cooldown_ok
}

pub(crate) fn mark_self_deleted(id: serenity::MessageId) {
    let mut set = self_deleted_set()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if set.len() > 2000 {
        set.clear();
    }
    set.insert(id);
}

fn register_post(
    id: serenity::MessageId,
    channel: serenity::ChannelId,
    content: String,
    files: Vec<(String, Vec<u8>)>,
    generation: u8,
) {
    if content.chars().count() > 8000 {
        return;
    }
    let kept: Vec<(String, Vec<u8>)> = files
        .into_iter()
        .filter(|(_, b)| b.len() <= MAX_STORED_FILE_BYTES)
        .collect();
    posted_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .register(
            id,
            PostedEntry {
                channel,
                content,
                files: kept,
                generation,
                posted_at: std::time::Instant::now(),
            },
        );
}

fn hook_url(id: serenity::WebhookId, token: &str) -> String {
    format!("https://discord.com/api/webhooks/{}/{}", id.get(), token)
}

async fn resolve_pool(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
) -> Vec<(serenity::WebhookId, String)> {
    if let Some(hit) = resolved_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&channel)
        .cloned()
    {
        if !hit.is_empty() {
            return hit;
        }
    }
    let mut pool = Vec::new();
    let urls = current_webhook_urls();
    for u in &urls {
        if pool.len() >= POOL_SIZE {
            break;
        }
        let Some((id, token)) = parse_webhook_url(u) else {
            continue;
        };
        match serenity::model::webhook::Webhook::from_url(http, &hook_url(id, &token)).await {
            Ok(wh) if wh.channel_id == Some(channel) => pool.push((id, token)),
            _ => {}
        }
    }
    if !pool.is_empty() {
        resolved_map()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(channel, pool.clone());
    }
    pool
}

fn evict_channel(channel: &serenity::ChannelId) {
    resolved_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(channel);
}

pub(crate) async fn resolve_poster(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
) -> Poster {
    let pool = resolve_pool(http, channel).await;
    if pool.is_empty() {
        return Poster::Direct;
    }
    let n = NEXT_HOOK.fetch_add(1, Ordering::Relaxed);
    let (id, token) = &pool[n % pool.len()];
    Poster::Hook {
        id: *id,
        token: token.clone(),
    }
}

async fn direct_send(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    content: String,
    files: Vec<(String, Vec<u8>)>,
) -> Result<serenity::Message, Error> {
    let mut builder = serenity::CreateMessage::new();
    if !content.is_empty() {
        builder = builder.content(&content);
    }
    for (name, bytes) in files {
        builder = builder.add_file(serenity::CreateAttachment::bytes(bytes, name));
    }
    Ok(channel.send_message(http, builder).await?)
}

pub(crate) async fn post_message(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    content: String,
    files: Vec<(String, Vec<u8>)>,
) -> Option<serenity::Message> {
    let pool = resolve_pool(http, channel).await;
    if pool.is_empty() {
        return direct_send(http, channel, content, files).await.ok();
    }
    let n = NEXT_HOOK.fetch_add(1, Ordering::Relaxed);
    let (id, token) = &pool[n % pool.len()];
    let url = hook_url(*id, token);
    let wh = match serenity::model::webhook::Webhook::from_url(http, &url).await {
        Ok(w) => w,
        Err(_) => {
            evict_channel(&channel);
            return direct_send(http, channel, content, files).await.ok();
        }
    };
    let mut builder = serenity::ExecuteWebhook::new().username(PERSONA);
    if !content.is_empty() {
        builder = builder.content(&content);
    }
    let mut stored: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, bytes) in files {
        if bytes.len() <= MAX_STORED_FILE_BYTES {
            stored.push((name.clone(), bytes.clone()));
        }
        builder = builder.add_file(serenity::CreateAttachment::bytes(bytes, name));
    }
    match wh.execute(http, true, builder).await {
        Ok(Some(msg)) => {
            register_post(msg.id, channel, content, stored, 0);
            Some(msg)
        }
        _ => {
            evict_channel(&channel);
            direct_send(http, channel, content, vec![]).await.ok()
        }
    }
}

pub(crate) async fn edit_posted(
    poster: &Poster,
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    target: serenity::MessageId,
    content: String,
    files: Vec<(String, Vec<u8>)>,
) -> bool {
    match poster {
        Poster::Direct => {
            let mut builder = serenity::EditMessage::new().content(content);
            for (name, bytes) in files {
                builder = builder.new_attachment(serenity::CreateAttachment::bytes(bytes, name));
            }
            channel.edit_message(http, target, builder).await.is_ok()
        }
        Poster::Hook { id, token } => {
            let url = hook_url(*id, token);
            match serenity::model::webhook::Webhook::from_url(http, &url).await {
                Ok(wh) => {
                    let mut builder = serenity::EditWebhookMessage::new().content(content);
                    for (name, bytes) in files {
                        builder = builder.new_attachment(serenity::CreateAttachment::bytes(bytes, name));
                    }
                    wh.edit_message(http, target, builder).await.is_ok()
                }
                Err(_) => {
                    evict_channel(&channel);
                    false
                }
            }
        }
    }
}

pub(crate) async fn edit_cleared(
    poster: &Poster,
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    target: serenity::MessageId,
    content: String,
) -> bool {
    match poster {
        Poster::Direct => {
            let builder = serenity::EditMessage::new()
                .content(content)
                .remove_all_attachments();
            channel.edit_message(http, target, builder).await.is_ok()
        }
        Poster::Hook { id, token } => {
            let url = hook_url(*id, token);
            match serenity::model::webhook::Webhook::from_url(http, &url).await {
                Ok(wh) => {
                    let builder = serenity::EditWebhookMessage::new()
                        .content(content)
                        .clear_attachments();
                    wh.edit_message(http, target, builder).await.is_ok()
                }
                Err(_) => {
                    evict_channel(&channel);
                    false
                }
            }
        }
    }
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
    if let Poster::Hook { .. } = resolve_poster(&http, channel).await {
        if let Some(msg) = post_message(&http, channel, content.clone(), files.clone()).await {
            if let poise::Context::Application(actx) = ctx {
                let _ = actx.interaction.delete_response(&http).await;
            }
            return Ok(msg);
        }
    }
    match direct_send(&http, channel, content.clone(), files.clone()).await {
        Ok(msg) => {
            if let poise::Context::Application(actx) = ctx {
                let _ = actx.interaction.delete_response(&http).await;
            }
            Ok(msg)
        }
        Err(_) => {
            let mut builder = poise::CreateReply::default();
            if !content.is_empty() {
                builder = builder.content(content);
            }
            for (name, bytes) in files {
                builder = builder.attachment(serenity::CreateAttachment::bytes(bytes, name));
            }
            Ok(ctx.send(builder).await?.into_message().await?)
        }
    }
}

pub(crate) async fn handle_delete(
    http: &std::sync::Arc<serenity::Http>,
    id: serenity::MessageId,
    war: bool,
) {
    let entry = match posted_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take(&id)
    {
        Some(e) => e,
        None => return,
    };
    {
        let mut gone = self_deleted_set()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if gone.remove(&id) {
            return;
        }
    }
    let age = entry.posted_at.elapsed().as_secs();
    if !war {
        return;
    }
    let cooldown_ok = {
        let mut last = last_repost()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match *last {
            Some(t) if t.elapsed().as_secs() < REPOST_COOLDOWN_SECS => false,
            _ => {
                *last = Some(std::time::Instant::now());
                true
            }
        }
    };
    if !repost_budget(entry.generation, age, cooldown_ok) {
        return;
    }
    let next_gen = entry.generation + 1;
    if let Some(msg) =
        post_message(&http, entry.channel, entry.content.clone(), entry.files.clone()).await
    {
        posted_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .register(
                msg.id,
                PostedEntry {
                    channel: entry.channel,
                    content: entry.content,
                    files: entry.files,
                    generation: next_gen,
                    posted_at: std::time::Instant::now(),
                },
            );
        eprintln!(
            "webhook: reposted deleted {} as {} (gen {})",
            id.get(),
            msg.id.get(),
            next_gen
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_url_parses_id_and_token() {
        let (id, token) =
            parse_webhook_url("https://discord.com/api/webhooks/123456/abcDEF_-xyz").unwrap();
        assert_eq!(id.get(), 123456);
        assert_eq!(token, "abcDEF_-xyz");
        let (id, _) =
            parse_webhook_url("https://discordapp.com/api/webhooks/99/tok/").unwrap();
        assert_eq!(id.get(), 99);
        assert!(parse_webhook_url("https://discord.com/api/webhooks/0/tok").is_none());
        assert!(parse_webhook_url("https://discord.com/api/webhooks/12/").is_none());
        assert!(parse_webhook_url("https://discord.com/api/webhooks/abc/tok").is_none());
        assert!(parse_webhook_url("https://evil.com/api/webhooks/1/tok").is_none());
        assert!(parse_webhook_url("not a url").is_none());
        assert!(parse_webhook_url("").is_none());
        assert!(parse_webhook_url("https://discord.com/api/webhooks/1/a/b").is_none());
    }

    #[test]
    fn registry_evicts_oldest_past_cap() {
        let mut reg = PostedRegistry {
            order: VecDeque::new(),
            entries: HashMap::new(),
        };
        assert_eq!(reg.entries.len(), 0);
        for n in 1..=5u64 {
            reg.register(
                serenity::MessageId::new(n),
                PostedEntry {
                    channel: serenity::ChannelId::new(1),
                    content: String::new(),
                    files: Vec::new(),
                    generation: 0,
                    posted_at: std::time::Instant::now(),
                },
            );
        }
        assert_eq!(reg.entries.len(), 5);
        assert!(reg.contains(&serenity::MessageId::new(1)));
        let taken = reg.take(&serenity::MessageId::new(2));
        assert!(taken.is_some());
        assert!(!reg.contains(&serenity::MessageId::new(2)));
        assert_eq!(reg.entries.len(), 4);
    }

    #[test]
    fn recognize_rule_matrix() {
        assert!(should_recognize(true, false));
        assert!(should_recognize(false, true));
        assert!(should_recognize(true, true));
        assert!(!should_recognize(false, false));
    }

    #[test]
    fn repost_budget_matrix() {
        assert!(repost_budget(0, 10, true));
        assert!(repost_budget(2, 100, true));
        assert!(!repost_budget(3, 10, true));
        assert!(!repost_budget(9, 10, true));
        assert!(!repost_budget(0, 2000, true));
        assert!(!repost_budget(0, 10, false));
    }
}
