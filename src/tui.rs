use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{prelude::*, widgets::*};

use poise::serenity_prelude as serenity;

use crate::{
    ai::{
        api_full_message, chunk_reply, clear_history, is_api_full_err, is_rate_limit_err,
        mentions_name, ollama_chat, resolve_ollama_key, run_websearch, strip_name, valid_model_name,
        web_status, RATE_LIMIT_USER_MSG,
    },
    feed::FeedLine,
    Error,
};

pub(crate) struct TuiSettings {
    pub(crate) ai_model: String,
    pub(crate) ollama_host: String,
    pub(crate) ollama_key: String,
    pub(crate) token: Option<String>,
}

struct Msg {
    author: String,
    color: Color,
    text: String,
    time: String,
}

struct Channel {
    name: String,
    id: u64,
    messages: Vec<Msg>,
    scroll: usize,
    follow: bool,
    unread: usize,
    discord: bool,
}

enum Reply {
    Text(Vec<String>),
}

struct Job {
    channel: u64,
    reply: Reply,
}

pub(crate) fn stamp() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60)
}

fn chan_label(e: &FeedLine) -> String {
    let name = e.channel_name.trim();
    if name.is_empty() {
        format!("#{}", e.channel)
    } else {
        format!("#{} ({})", name, e.channel)
    }
}

fn short_chan(id: u64, id_to_name: &HashMap<u64, String>) -> String {
    if id == u64::MAX {
        return "#spy".to_string();
    }
    if let Some(n) = id_to_name.get(&id) {
        if !n.trim().is_empty() {
            return format!("#{}", n.trim());
        }
    }
    format!("#{}", id)
}

fn char_width(c: char) -> usize {
    match c {
        '\u{1100}'..='\u{115F}'
        | '\u{2E80}'..='\u{303E}'
        | '\u{3041}'..='\u{33FF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{AC00}'..='\u{D7AF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FE30}'..='\u{FE4F}'
        | '\u{FF00}'..='\u{FF60}'
        | '\u{FFE0}'..='\u{FFE6}' => 2,
        _ => 1,
    }
}

fn wrap_line(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0;
        for word in raw.split_whitespace() {
            let w: usize = word.chars().map(char_width).sum();
            if cur_w > 0 && cur_w + 1 + w > width {
                out.push(cur);
                cur = String::new();
                cur_w = 0;
            }
            if !cur.is_empty() {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += w;
        }
        out.push(cur);
    }
    out
}

const TYPING_TTL: Duration = Duration::from_secs(9);
const BOT_LIVE_SECS: u64 = 60;
const MAX_DISCORD_CHANNELS: usize = 40;

struct App {
    channels: Vec<Channel>,
    active: usize,
    input: String,
    cursor: usize,
    settings: TuiSettings,
    pending: HashMap<u64, usize>,
    tx: tokio::sync::mpsc::UnboundedSender<Job>,
    rx: tokio::sync::mpsc::UnboundedReceiver<Job>,
    quit: bool,
    feed_off: u64,
    spy_hint: bool,
    chan_names: HashMap<String, u64>,
    id_to_name: HashMap<u64, String>,
    typing: HashMap<(u64, String), Instant>,
    last_feed: Option<Instant>,
    feed_total: u64,
    filter_mentions: bool,
}

fn mk_channel(name: &str, id: u64, discord: bool) -> Channel {
    Channel {
        name: name.to_string(),
        id,
        messages: Vec::new(),
        scroll: 0,
        follow: true,
        unread: 0,
        discord,
    }
}

impl App {
    fn new(settings: TuiSettings) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = Self {
            channels: vec![
                mk_channel("general", 1, false),
                mk_channel("bot-commands", 2, false),
                mk_channel("spy", u64::MAX, false),
            ],
            active: 0,
            input: String::new(),
            cursor: 0,
            settings,
            pending: HashMap::new(),
            tx,
            rx,
            quit: false,
            feed_off: 0,
            spy_hint: false,
            chan_names: HashMap::new(),
            id_to_name: HashMap::new(),
            typing: HashMap::new(),
            last_feed: None,
            feed_total: 0,
            filter_mentions: false,
        };
        app.say(
            0,
            "artixy",
            Color::Magenta,
            "welcome to the bot control center :3\n- chat locally: `artixy <question>`\n- live discord mirror: Tab to #spy (all traffic) or per-channel #discord-* views\n- send as bot: `/say #<id|name> <text>` (see `/channels`)\n- show typing: top bar + `/status` (who is typing where)\n- control bot: `/ai on|off|model <m>`, `/war on|off`, `/sayas on|off`, `/status`\n- filter: `/filter mentions|all` limits #spy to messages to artixy\n- `/websearch <q>`, `/forget`, `/clear`, `/quit` — `/help` for all",
        );
        app
    }

    fn is_spy(&self) -> bool {
        self.channels[self.active].id == u64::MAX
    }

    fn spy_idx(&self) -> Option<usize> {
        self.channels.iter().position(|c| c.id == u64::MAX)
    }

    fn bot_live(&self) -> bool {
        self.last_feed
            .map(|t| t.elapsed().as_secs() < BOT_LIVE_SECS)
            .unwrap_or(false)
    }

    fn prune_typing(&mut self) {
        let now = Instant::now();
        self.typing
            .retain(|_, t| now.duration_since(*t) < TYPING_TTL);
    }

    fn typing_in(&self, channel: u64) -> Vec<String> {
        let now = Instant::now();
        let mut out: Vec<String> = self
            .typing
            .iter()
            .filter(|((ch, _), t)| *ch == channel && now.duration_since(**t) < TYPING_TTL)
            .map(|((_, who), _)| who.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn typing_summary(&self) -> Vec<String> {
        let now = Instant::now();
        let mut pairs: Vec<(u64, String)> = self
            .typing
            .iter()
            .filter(|(_, t)| now.duration_since(**t) < TYPING_TTL)
            .map(|((ch, who), _)| (*ch, who.clone()))
            .collect();
        pairs.sort();
        pairs.dedup();
        pairs
            .into_iter()
            .map(|(ch, who)| format!("{} in {}", who, short_chan(ch, &self.id_to_name)))
            .collect()
    }

    fn ensure_discord_channel(&mut self, id: u64) {
        if id == u64::MAX || id == 1 || id == 2 {
            return;
        }
        if self.channels.iter().any(|c| c.id == id) {
            // refresh name if we learned it
            if let Some(pretty) = self.id_to_name.get(&id).cloned() {
                let pretty = pretty.trim().to_string();
                if !pretty.is_empty() {
                    if let Some(ch) = self.channels.iter_mut().find(|c| c.id == id) {
                        let want = format!("discord-{}", pretty);
                        if ch.name != want {
                            ch.name = want;
                        }
                    }
                }
            }
            return;
        }
        let discord_count = self.channels.iter().filter(|c| c.discord).count();
        if discord_count >= MAX_DISCORD_CHANNELS {
            return;
        }
        let name = match self.id_to_name.get(&id) {
            Some(n) if !n.trim().is_empty() => format!("discord-{}", n.trim()),
            _ => format!("discord-{}", id),
        };
        self.channels.push(mk_channel(&name, id, true));
    }

    fn poll_feed(&mut self) {
        let lines = crate::feed::read_new(&mut self.feed_off);
        if lines.is_empty() {
            if !self.spy_hint {
                self.spy_hint = true;
                if let Some(idx) = self.spy_idx() {
                    self.say(
                        idx,
                        "system",
                        Color::DarkGray,
                        "no discord traffic yet — run `artixy` (bot mode, with a token) and every message + typing the bot sees appears here live.",
                    );
                }
            }
            return;
        }
        let spy_idx = match self.spy_idx() {
            Some(i) => i,
            None => return,
        };
        let now = Instant::now();
        for e in lines {
            self.last_feed = Some(now);
            self.feed_total += 1;
            let cname = e.channel_name.trim().to_string();
            if !cname.is_empty() {
                self.chan_names.insert(cname.to_lowercase(), e.channel);
                self.id_to_name.insert(e.channel, cname.clone());
            }
            self.ensure_discord_channel(e.channel);
            if e.kind == "typing" {
                let who = e.author.trim().to_string();
                if !who.is_empty() {
                    self.typing.insert((e.channel, who), now);
                }
                continue;
            }
            // msg
            if e.text.trim().is_empty() {
                continue;
            }
            let to_artixy = mentions_name(&e.text);
            if self.filter_mentions && !to_artixy {
                // still route to per-channel view, but skip noisy #spy
                if let Some(pi) = self.channels.iter().position(|c| c.id == e.channel) {
                    if pi != spy_idx {
                        let (author, color) = if e.bot {
                            (e.author.clone(), Color::Yellow)
                        } else {
                            (e.author.clone(), Color::Cyan)
                        };
                        self.say(pi, &author, color, &e.text);
                        if pi != self.active {
                            self.channels[pi].unread = self.channels[pi].unread.saturating_add(1);
                        }
                    }
                }
                continue;
            }
            let (author, color) = if to_artixy {
                (format!("{} →@artixy", e.author), Color::Magenta)
            } else if e.bot {
                (e.author.clone(), Color::Yellow)
            } else {
                (e.author.clone(), Color::Cyan)
            };
            let body = format!("[{}] {}", chan_label(&e), e.text);
            self.say(spy_idx, &author, color, &body);
            if spy_idx != self.active {
                self.channels[spy_idx].unread = self.channels[spy_idx].unread.saturating_add(1);
            }
            // also mirror into per-channel view without the [chan] prefix
            if let Some(pi) = self.channels.iter().position(|c| c.id == e.channel) {
                if pi != spy_idx {
                    self.say(pi, &author, color, &e.text);
                    if pi != self.active {
                        self.channels[pi].unread = self.channels[pi].unread.saturating_add(1);
                    }
                }
            }
        }
    }

    fn resolve_target(&self, target: &str) -> Option<u64> {
        let t = target.trim().trim_start_matches('#').trim();
        if t.is_empty() {
            return None;
        }
        if let Ok(n) = t.parse::<u64>() {
            if n != 0 {
                return Some(n);
            }
        }
        let low = t.to_lowercase();
        if let Some(id) = self.chan_names.get(&low).copied() {
            return Some(id);
        }
        // allow "discord-name" style
        let stripped = low.strip_prefix("discord-").unwrap_or(&low);
        if let Some(id) = self.chan_names.get(stripped).copied() {
            return Some(id);
        }
        // reverse lookup by pretty name
        for (id, name) in &self.id_to_name {
            if name.to_lowercase() == low || name.to_lowercase() == stripped {
                return Some(*id);
            }
        }
        None
    }

    fn send_as_bot(&mut self, target: String, text: String) {
        let Some(id) = self.resolve_target(&target) else {
            self.say_active(
                "system",
                Color::DarkGray,
                "unknown channel — run `/channels` to see live ids/names from #spy, then `/say #<id|name> <text>`.",
            );
            return;
        };
        let Some(token) = self.settings.token.clone() else {
            self.say_active(
                "system",
                Color::DarkGray,
                "no discord token configured — set discord_token in config or DISCORD_TOKEN env, then restart tui.",
            );
            return;
        };
        if text.trim().is_empty() {
            self.say_active("system", Color::DarkGray, "usage: /say #<id|name> <text>");
            return;
        }
        let tx = self.tx.clone();
        let active = self.active;
        let active_id = self.channels[active].id;
        // report back to the channel you typed in, plus mirror status to #spy
        *self.pending.entry(active_id).or_insert(0) += 1;
        let chan_name = short_chan(id, &self.id_to_name);
        tokio::spawn(async move {
            let http = serenity::Http::new(&token);
            let msg = match serenity::ChannelId::new(id).say(&http, &text).await {
                Ok(m) => format!("sent as artixy to {} (<#{id}>, msg {})", chan_name, m.id.get()),
                Err(e) => format!("send to <#{id}> failed: {e}"),
            };
            let _ = tx.send(Job {
                channel: active_id,
                reply: Reply::Text(vec![msg]),
            });
        });
    }

    fn typing_as_bot(&mut self, target: String) {
        let Some(id) = self.resolve_target(&target) else {
            self.say_active(
                "system",
                Color::DarkGray,
                "unknown channel — run `/channels` first.",
            );
            return;
        };
        let Some(token) = self.settings.token.clone() else {
            self.say_active(
                "system",
                Color::DarkGray,
                "no discord token configured, cannot broadcast typing.",
            );
            return;
        };
        let tx = self.tx.clone();
        let active_id = self.channels[self.active].id;
        *self.pending.entry(active_id).or_insert(0) += 1;
        tokio::spawn(async move {
            let http = serenity::Http::new(&token);
            let msg = match serenity::ChannelId::new(id).broadcast_typing(&http).await {
                Ok(_) => format!("broadcast typing as artixy in <#{id}>"),
                Err(e) => format!("typing failed in <#{id}>: {e}"),
            };
            let _ = tx.send(Job {
                channel: active_id,
                reply: Reply::Text(vec![msg]),
            });
        });
    }

    fn chan(&self) -> u64 {
        self.channels[self.active].id
    }

    fn say(&mut self, idx: usize, author: &str, color: Color, text: &str) {
        let ch = &mut self.channels[idx];
        ch.messages.push(Msg {
            author: author.to_string(),
            color,
            text: text.chars().take(4000).collect(),
            time: stamp(),
        });
        if ch.messages.len() > 300 {
            let drop = ch.messages.len() - 300;
            ch.messages.drain(..drop);
        }
        ch.follow = true;
    }

    fn say_active(&mut self, author: &str, color: Color, text: &str) {
        let idx = self.active;
        self.say(idx, author, color, text);
    }

    fn spawn_ai(&mut self, prompt: String) {
        let chan = self.chan();
        let idx = self.active;
        self.say(idx, "you", Color::Green, &prompt);
        *self.pending.entry(chan).or_insert(0) += 1;
        let tx = self.tx.clone();
        let host = self.settings.ollama_host.clone();
        let model = self.settings.ai_model.clone();
        let key = self.settings.ollama_key.clone();
        tokio::spawn(async move {
            let reply = match ollama_chat(&host, &model, &key, chan, "you", &prompt).await {
                Ok(text) => Reply::Text(chunk_reply(&text)),
                Err(e) if is_api_full_err(&e.to_string()) => {
                    Reply::Text(vec![api_full_message()])
                }
                Err(_) => Reply::Text(vec![crate::ai::glitch_text(&host, &model).await]),
            };
            let _ = tx.send(Job {
                channel: chan,
                reply,
            });
        });
    }

    fn spawn_search(&mut self, query: String) {
        let chan = self.chan();
        let idx = self.active;
        self.say(idx, "you", Color::Green, &format!("/websearch {query}"));
        *self.pending.entry(chan).or_insert(0) += 1;
        let tx = self.tx.clone();
        let key = self.settings.ollama_key.clone();
        tokio::spawn(async move {
            let answer = run_websearch(&key, &query).await;
            let text = if answer.trim().is_empty() {
                format!("no web results for {query}")
            } else if is_rate_limit_err(&answer) || answer == RATE_LIMIT_USER_MSG {
                format!("{answer} — or ask `artixy {query}` and I'll answer from what I know.")
            } else {
                answer.chars().take(1500).collect()
            };
            let _ = tx.send(Job {
                channel: chan,
                reply: Reply::Text(vec![text]),
            });
        });
    }

    fn spawn_status(&mut self) {
        let chan = self.chan();
        *self.pending.entry(chan).or_insert(0) += 1;
        let tx = self.tx.clone();
        let key = self.settings.ollama_key.clone();
        let model = self.settings.ai_model.clone();
        let host = self.settings.ollama_host.clone();
        tokio::spawn(async move {
            let web = web_status(&key).await;
            let web = if is_rate_limit_err(&web) {
                format!("{web} (offline fallback: chat answers from knowledge)")
            } else {
                web
            };
            let text = format!("model `{model}` on `{host}`\n{web}");
            let _ = tx.send(Job {
                channel: chan,
                reply: Reply::Text(vec![text]),
            });
        });
    }

    fn drain(&mut self) {
        while let Ok(job) = self.rx.try_recv() {
            if let Some(n) = self.pending.get_mut(&job.channel) {
                *n = n.saturating_sub(1);
            }
            if let Some(idx) = self.channels.iter().position(|c| c.id == job.channel) {
                let Reply::Text(chunks) = job.reply;
                let n = chunks.len();
                for c in chunks {
                    self.say(idx, "artixy", Color::Magenta, &c);
                }
                if idx != self.active {
                    self.channels[idx].unread =
                        self.channels[idx].unread.saturating_add(n);
                }
            }
        }
    }

    fn goto(&mut self, idx: usize) {
        if idx < self.channels.len() {
            self.active = idx;
            self.channels[idx].unread = 0;
            self.channels[idx].follow = true;
        }
    }

    fn submit(&mut self) {
        let line = std::mem::take(&mut self.input);
        self.cursor = 0;
        let text = line.trim().to_string();
        if text.is_empty() {
            return;
        }
        if text == "/quit" || text == "/exit" {
            self.quit = true;
            return;
        }
        if text == "/clear" {
            let ch = &mut self.channels[self.active];
            ch.messages.clear();
            ch.scroll = 0;
            return;
        }
        // ---- control-center commands work from ANY channel, incl #spy ----
        if text == "/help" {
            self.say_active(
                "system",
                Color::DarkGray,
                "chat: `artixy <q>` (local AI) | live: Tab to #spy / #discord-* | `/say #<id|name> <text>` send as bot | `/typing #<id|name>` bot typing | `/channels` list discord | `/status` bot+AI | `/ai on|off|model <m>|status` | `/war on|off` | `/sayas on|off` | `/notify <id>|off` | `/filter mentions|all` | `/websearch <q>` | `/forget` | `/clear` | `/quit` | Tab/Shift-Tab switch, PgUp/PgDn scroll, Alt+1-9 jump",
            );
            return;
        }
        if text == "/channels" {
            self.list_channels();
            return;
        }
        if text == "/status" {
            self.local_status();
            self.spawn_status();
            return;
        }
        if text == "/forget" {
            if self.is_spy() {
                self.say_active("system", Color::DarkGray, "nothing to forget in #spy (read-only mirror). Tab to #general.");
                return;
            }
            clear_history(self.chan());
            self.say_active("system", Color::DarkGray, "forgot the conversation here.");
            return;
        }
        if text == "/ai" || text == "/ai status" {
            self.spawn_status();
            return;
        }
        if let Some(rest) = text.strip_prefix("/ai").filter(|_| text.starts_with("/ai ")) {
            self.handle_ai_cmd(rest.trim());
            return;
        }
        if let Some(rest) = text.strip_prefix("/war").filter(|_| text.starts_with("/war")) {
            self.handle_bool_config("war_mode", rest.trim(), "war mode");
            return;
        }
        if let Some(rest) = text.strip_prefix("/sayas").filter(|_| text.starts_with("/sayas")) {
            self.handle_bool_config("sayas_enabled", rest.trim(), "say-as-artix");
            return;
        }
        if text.starts_with("/filter") {
            let arg = text.strip_prefix("/filter").unwrap_or("").trim().to_lowercase();
            match arg.as_str() {
                "" => {
                    self.say_active(
                        "system",
                        Color::DarkGray,
                        &format!(
                            "#spy filter is `{}` — `/filter mentions` (only messages to artixy) or `/filter all`.",
                            if self.filter_mentions { "mentions" } else { "all" }
                        ),
                    );
                }
                "mentions" | "mention" | "artixy" | "on" => {
                    self.filter_mentions = true;
                    self.say_active("system", Color::DarkGray, "#spy now shows only messages to artixy. `/filter all` to undo.");
                }
                "all" | "off" => {
                    self.filter_mentions = false;
                    self.say_active("system", Color::DarkGray, "#spy now shows all discord traffic.");
                }
                _ => {
                    self.say_active("system", Color::DarkGray, "usage: /filter mentions|all");
                }
            }
            return;
        }
        if let Some(rest) = text.strip_prefix("/say").filter(|_| text.starts_with("/say ")) {
            let rest = rest.trim();
            match rest.split_once(char::is_whitespace) {
                Some((t, msg)) if !t.trim().is_empty() && !msg.trim().is_empty() => {
                    self.send_as_bot(t.trim().to_string(), msg.trim().to_string());
                }
                _ => self.say_active("system", Color::DarkGray, "usage: /say #<id|name> <text> (see /channels)"),
            }
            return;
        }
        if let Some(rest) = text.strip_prefix("/typing").filter(|_| text.starts_with("/typing")) {
            let target = rest.trim().to_string();
            if target.is_empty() {
                self.say_active("system", Color::DarkGray, "usage: /typing #<id|name>");
            } else {
                self.typing_as_bot(target);
            }
            return;
        }
        if text.starts_with("/notify") {
            self.handle_notify(text.strip_prefix("/notify").unwrap_or("").trim());
            return;
        }
        if let Some(q) = text.strip_prefix("/websearch") {
            let q = q.trim().to_string();
            if q.is_empty() {
                self.say_active("system", Color::DarkGray, "usage: /websearch <query>");
            } else if self.is_spy() {
                // allow search from spy too, answer goes back to spy view
                self.spawn_search(q);
            } else {
                self.spawn_search(q);
            }
            return;
        }
        if text.starts_with('/') {
            self.say_active("system", Color::DarkGray, "unknown command, try /help");
            return;
        }
        // plain chat: #spy is read-only
        if self.is_spy() {
            self.say_active(
                "system",
                Color::DarkGray,
                "spy is read-only — Tab to #general to chat locally, or `/say #<id> <text>` to send as the bot.",
            );
            return;
        }
        if self.channels[self.active].discord {
            self.say_active(
                "system",
                Color::DarkGray,
                "this is a live discord mirror (read-only) — use `/say #<id|name> <text>` to send as the bot, or Tab to #general to chat locally.",
            );
            return;
        }
        if mentions_name(&text) {
            let prompt = strip_name(&text);
            if prompt.trim().is_empty() {
                self.say_active(
                    "artixy",
                    Color::Magenta,
                    "ping me with a question — `artixy <question>`",
                );
            } else {
                self.spawn_ai(prompt);
            }
        } else {
            let idx = self.active;
            self.say(idx, "you", Color::Green, &text);
        }
    }

    fn list_channels(&mut self) {
        if self.id_to_name.is_empty() {
            self.say_active(
                "system",
                Color::DarkGray,
                "no discord channels seen yet — run the bot (`artixy` with token) and chat in discord, then they appear here.",
            );
            return;
        }
        let mut pairs: Vec<(u64, String)> = self.id_to_name.iter().map(|(k, v)| (*k, v.clone())).collect();
        pairs.sort_by_key(|(id, _)| *id);
        let mut lines = vec![format!("{} live discord channels (use with /say, /typing):", pairs.len())];
        for (id, name) in pairs.iter().take(30) {
            let typing = self.typing_in(*id);
            let t = if typing.is_empty() {
                String::new()
            } else {
                format!(" — typing: {}", typing.join(", "))
            };
            lines.push(format!("- #{name} (<#{id}>){t}"));
        }
        if pairs.len() > 30 {
            lines.push(format!("…and {} more", pairs.len() - 30));
        }
        self.say_active("system", Color::DarkGray, &lines.join("\n"));
    }

    fn local_status(&mut self) {
        let live = if self.bot_live() { "LIVE" } else { "OFFLINE" };
        let typing = self.typing_summary();
        let typing_txt = if typing.is_empty() {
            "nobody typing".to_string()
        } else if typing.len() <= 3 {
            format!("typing: {}", typing.join(" · "))
        } else {
            format!("typing: {} +{} more", typing[..3].join(" · "), typing.len() - 3)
        };
        let discord_n = self.id_to_name.len();
        let token_txt = if self.settings.token.is_some() { "set" } else { "MISSING" };
        self.say_active(
            "system",
            Color::DarkGray,
            &format!(
                "bot: {live} (feed msgs: {}, discord channels seen: {}) | {typing_txt} | token: {token_txt} | #spy filter: {} | model `{}` on `{}`",
                self.feed_total,
                discord_n,
                if self.filter_mentions { "mentions" } else { "all" },
                self.settings.ai_model,
                self.settings.ollama_host,
            ),
        );
    }

    fn handle_ai_cmd(&mut self, arg: &str) {
        let a = arg.trim();
        let low = a.to_lowercase();
        if low.is_empty() || low == "status" {
            self.spawn_status();
            return;
        }
        // Support combos like "on model:llama3.1", "true model foo", "off", "model foo".
        let mut want_on: Option<bool> = None;
        let mut model = String::new();
        let mut tokens: Vec<String> = Vec::new();
        // split but keep "model:xxx" together
        for w in a.split_whitespace() {
            tokens.push(w.to_string());
        }
        let mut i = 0;
        while i < tokens.len() {
            let t = tokens[i].to_lowercase();
            if t == "on" || t == "true" || t == "enable" || t == "enabled" {
                want_on = Some(true);
            } else if t == "off" || t == "false" || t == "disable" || t == "disabled" {
                want_on = Some(false);
            } else if t == "model" {
                if i + 1 < tokens.len() {
                    model = tokens[i + 1].trim().to_string();
                    i += 1;
                }
            } else if let Some(rest) = tokens[i].strip_prefix("model:").or_else(|| tokens[i].strip_prefix("model=")) {
                let _ = rest;
                // "model:xxx" — strip prefix case-insensitively by fixed len
                let raw = &tokens[i];
                if let Some(pos) = raw.find(':').or_else(|| raw.find('=')) {
                    model = raw[pos + 1..].trim().to_string();
                }
            } else if t.starts_with("model:") || t.starts_with("model=") {
                // fallback (already handled above)
            } else if tokens.len() == 1 && valid_model_name(&tokens[i]) {
                model = tokens[i].trim().to_string();
            }
            i += 1;
        }
        // bare single word model name without "model" keyword (e.g. "/ai qwen3:4b")
        if model.is_empty() && want_on.is_none() {
            if tokens.len() == 1 && valid_model_name(&tokens[0]) {
                let wlow = tokens[0].to_lowercase();
                if wlow != "on" && wlow != "off" && wlow != "status" {
                    model = tokens[0].clone();
                }
            }
        }
        if model.is_empty() && want_on.is_none() {
            self.say_active(
                "system",
                Color::DarkGray,
                "usage: /ai on|off|model <name>|status (e.g. `/ai on`, `/ai model llama3.1`)",
            );
            return;
        }
        if !model.is_empty() && !valid_model_name(&model) {
            self.say_active("system", Color::DarkGray, "bad model name — letters/numbers `._-:/` only, max 128 chars (e.g. llama3.1).");
            return;
        }
        let m_opt = if model.is_empty() { None } else { Some(model.clone()) };
        match update_file_config(|c| {
            if let Some(on) = want_on {
                c.ai_enabled = on;
            } else if m_opt.is_some() {
                // setting a model implies enabling, like the discord /ai command
                c.ai_enabled = true;
            }
            if let Some(m) = m_opt.clone() {
                c.ai_model = m;
            }
        }) {
            Ok(_) => {
                if !model.is_empty() {
                    self.settings.ai_model = model.clone();
                }
                let cfg = crate::config::load_file_config();
                self.say_active(
                    "system",
                    Color::DarkGray,
                    &format!(
                        "ai saved: enabled={} model=`{}` (bot hot-applies).",
                        cfg.ai_enabled, cfg.ai_model
                    ),
                );
            }
            Err(e) => self.say_active("system", Color::DarkGray, &format!("config save failed: {e}")),
        }
    }

    fn handle_notify(&mut self, arg: &str) {
        let a = arg.trim();
        if a.is_empty() || a.eq_ignore_ascii_case("status") {
            let cfg = crate::config::load_file_config();
            match cfg.notify_channel {
                Some(id) => self.say_active("system", Color::DarkGray, &format!("boot messages go to <#{id}>. `/notify off` or `/notify <channel-id>`.")),
                None => self.say_active("system", Color::DarkGray, "boot messages are OFF. `/notify <channel-id>` to set."),
            }
            return;
        }
        if a.eq_ignore_ascii_case("off") {
            match update_file_config(|c| c.notify_channel = None) {
                Ok(_) => self.say_active("system", Color::DarkGray, "boot messages OFF (saved)."),
                Err(e) => self.say_active("system", Color::DarkGray, &format!("config save failed: {e}")),
            }
            return;
        }
        match a.parse::<u64>() {
            Ok(id) if id != 0 => match update_file_config(|c| c.notify_channel = Some(id)) {
                Ok(_) => self.say_active("system", Color::DarkGray, &format!("boot messages will go to <#{id}> (saved).")),
                Err(e) => self.say_active("system", Color::DarkGray, &format!("config save failed: {e}")),
            },
            _ => self.say_active("system", Color::DarkGray, "usage: /notify <channel-id>|off|status"),
        }
    }

    fn handle_bool_config(&mut self, key: &str, arg: &str, label: &str) {
        let low = arg.trim().to_lowercase();
        let cmd = key_key_cmd(key);
        if low.is_empty() || low == "status" {
            let cfg = crate::config::load_file_config();
            let on = match key {
                "war_mode" => cfg.war_mode,
                "sayas_enabled" => cfg.sayas_enabled,
                _ => false,
            };
            self.say_active("system", Color::DarkGray, &format!("{label} is {on} (config). `/{cmd} on|off` to change."));
            return;
        }
        if low != "on" && low != "off" && low != "true" && low != "false" {
            self.say_active("system", Color::DarkGray, &format!("usage: /{} on|off", key_key_cmd(key)));
            return;
        }
        let on = low == "on" || low == "true";
        let res = update_file_config(|c| match key {
            "war_mode" => c.war_mode = on,
            "sayas_enabled" => c.sayas_enabled = on,
            _ => {}
        });
        match res {
            Ok(_) => self.say_active("system", Color::DarkGray, &format!("{label}={on} saved (bot hot-applies).")),
            Err(e) => self.say_active("system", Color::DarkGray, &format!("config save failed: {e}")),
        }
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if mods.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Char('c') | KeyCode::Char('d') | KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('u') => {
                    self.input.clear();
                    self.cursor = 0;
                }
                KeyCode::Char('l') => {
                    let ch = &mut self.channels[self.active];
                    ch.messages.clear();
                    ch.scroll = 0;
                }
                _ => {}
            }
            return;
        }
        if mods.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(c) = code {
                if let Some(d) = c.to_digit(10) {
                    let idx = (d as usize).saturating_sub(1);
                    if idx < self.channels.len() {
                        self.goto(idx);
                        return;
                    }
                }
            }
        }
        match code {
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Backspace => {
                if self.cursor > 0 && !self.input.is_empty() {
                    let mut chars: Vec<char> = self.input.chars().collect();
                    let at = self.cursor.min(chars.len());
                    chars.remove(at - 1);
                    self.input = chars.into_iter().collect();
                    self.cursor -= 1;
                }
            }
            KeyCode::Delete => {
                let mut chars: Vec<char> = self.input.chars().collect();
                if self.cursor < chars.len() {
                    chars.remove(self.cursor);
                    self.input = chars.into_iter().collect();
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Tab => {
                let n = self.channels.len().max(1);
                self.goto((self.active + 1) % n);
            }
            KeyCode::BackTab => {
                let n = self.channels.len().max(1);
                self.goto((self.active + n - 1) % n);
            }
            KeyCode::PageUp => {
                let ch = &mut self.channels[self.active];
                ch.follow = false;
                ch.scroll = ch.scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                let ch = &mut self.channels[self.active];
                ch.scroll = ch.scroll.saturating_add(10);
                ch.follow = false;
            }
            KeyCode::Up => {
                let ch = &mut self.channels[self.active];
                ch.follow = false;
                ch.scroll = ch.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                let ch = &mut self.channels[self.active];
                ch.scroll = ch.scroll.saturating_add(1);
            }
            KeyCode::Char(c) => {
                let mut chars: Vec<char> = self.input.chars().collect();
                let at = self.cursor.min(chars.len());
                chars.insert(at, c);
                self.input = chars.into_iter().collect();
                self.cursor += 1;
            }
            _ => {}
        }
    }
}

fn key_key_cmd(key: &str) -> &str {
    match key {
        "war_mode" => "war",
        "sayas_enabled" => "sayas",
        _ => key,
    }
}

fn update_file_config(f: impl FnOnce(&mut crate::config::FileConfig)) -> Result<(), String> {
    let mut cfg = crate::config::load_file_config();
    f(&mut cfg);
    let text = toml::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    let path = crate::config::config_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perm = meta.permissions();
            if perm.mode() & 0o077 != 0 {
                perm.set_mode(0o600);
                let _ = std::fs::set_permissions(&path, perm);
            }
        }
    }
    Ok(())
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.size();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(1)])
        .split(rows[1]);

    let live = if app.bot_live() { "LIVE" } else { "OFFLINE" };
    let live_color = if app.bot_live() { Color::Green } else { Color::DarkGray };
    let typing = app.typing_summary();
    let typing_txt = if typing.is_empty() {
        String::new()
    } else if typing.len() <= 2 {
        format!(" │ typing: {}", typing.join(", "))
    } else {
        format!(" │ typing: {} +{}more", typing[..2].join(", "), typing.len() - 2)
    };
    let pending: usize = app.pending.values().sum();
    let title = Line::from(vec![
        Span::styled(
            " artixy ",
            Style::default().fg(Color::Black).bg(Color::Magenta),
        ),
        Span::raw(" control center "),
        Span::styled(format!(" {live} "), Style::default().fg(Color::Black).bg(live_color)),
        Span::raw(format!(" feed:{} disc:{} pending:{}{} │ tab:ch alt+1-9:jump /help", app.feed_total, app.id_to_name.len(), pending, typing_txt)),
    ]);
    frame.render_widget(Paragraph::new(title), rows[0]);

    let names: Vec<ListItem> = app
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let typing_dot = if app.typing_in(c.id).is_empty() { " " } else { "●" };
            let unread = if c.unread > 0 { format!(" ({})", c.unread) } else { String::new() };
            let label = if i == app.active {
                format!(">#{}{} {}", c.name, unread, typing_dot)
            } else {
                format!(" #{} {} {}", c.name, unread, typing_dot)
            };
            let style = if i == app.active {
                Style::default().fg(Color::White)
            } else if c.unread > 0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();
    let side = List::new(names).block(Block::bordered().title("channels"));
    frame.render_widget(side, cols[0]);

    let ch = &app.channels[app.active];
    let inner_w = cols[1].width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for m in &ch.messages {
        let head = format!("{}  {}", m.author, m.time);
        lines.push(Line::from(Span::styled(head, Style::default().fg(m.color))));
        for w in wrap_line(&m.text, inner_w) {
            lines.push(Line::from(Span::raw(format!("  {w}"))));
        }
        lines.push(Line::from(""));
    }
    // inline typing indicator for current channel
    let cur_typing = app.typing_in(app.channels[app.active].id);
    if !cur_typing.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  … {} is typing", cur_typing.join(", ")),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        )));
    }
    let view_h = cols[1].height.saturating_sub(2) as usize;
    let max_off = lines.len().saturating_sub(view_h);
    let ch = &mut app.channels[app.active];
    if ch.follow {
        ch.scroll = max_off;
    } else {
        ch.scroll = ch.scroll.min(max_off);
    }
    let scroll = ch.scroll;
    let mut title = format!("#{} ", app.channels[app.active].name);
    if app.channels[app.active].id == u64::MAX && app.filter_mentions {
        title.push_str("(mentions only) ");
    }
    if app.channels[app.active].discord {
        title.push_str("(live mirror, read-only — /say to send) ");
    }
    let body = Paragraph::new(lines)
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));
    frame.render_widget(body, cols[1]);

    let chars: Vec<char> = app.input.chars().collect();
    let at = app.cursor.min(chars.len());
    let before: String = chars[..at].iter().collect();
    let under = chars.get(at).copied().unwrap_or(' ');
    let after: String = chars[at + (chars.get(at).is_some() as usize)..]
        .iter()
        .collect();
    let input_line = Line::from(vec![
        Span::raw(before),
        Span::styled(
            under.to_string(),
            Style::default().fg(Color::Black).bg(Color::White),
        ),
        Span::raw(after),
    ]);
    let cur_typing2 = app.typing_in(app.channels[app.active].id);
    let status = if !cur_typing2.is_empty() {
        format!("{} typing…", cur_typing2.join(", "))
    } else if pending > 0 {
        "artixy is typing…".to_string()
    } else {
        "message (/help)".to_string()
    };
    let input = Paragraph::new(input_line).block(Block::bordered().title(status));
    frame.render_widget(input, rows[2]);
}

pub(crate) async fn run_tui(settings: TuiSettings) -> Result<(), Error> {
    crossterm::terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    crossterm::execute!(out, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new(settings);

    let mut frame: u64 = 0;
    loop {
        app.drain();
        app.prune_typing();
        frame += 1;
        if frame % 8 == 0 {
            app.poll_feed();
        }
        terminal.draw(|f| render(f, &mut app))?;
        if app.quit {
            break;
        }
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(k) = event::read()? {
                app.on_key(k.code, k.modifiers);
            }
        }
    }

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

pub(crate) fn local_settings(
    ai_enabled: bool,
    ai_model: String,
    ollama_host: String,
    ollama_key_cfg: &str,
    token: Option<String>,
) -> TuiSettings {
    let _ = ai_enabled;
    TuiSettings {
        ai_model,
        ollama_host,
        ollama_key: resolve_ollama_key(ollama_key_cfg),
        token,
    }
}
