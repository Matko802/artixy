// artixy TUI — bot control center.
//
// Clean layout (ratatui 0.26, immediate-mode MVC as per ratatui best
// practices: App = model, render() = view, run_tui/on_key = controller):
//
//   header   one line: brand + LIVE/OFFLINE + where + typing
//   main     sidebar (channels) | chat (messages)
//   input    single-line composer for the active channel
//   footer   key hints + transient notices
//
// Chat UX conventions:
// - Up/Down = input history, PgUp/PgDn = scroll chat, Tab = next channel.
// - F1 or /help opens a help popup (chat stays clean).
// - #spy is a watch-only mirror; #<live> channels send as artixy when you
//   type, and /commands there run on discord (/say works from anywhere).
// - When hosted web search is out of credits it is silently off and the
//   bot answers from knowledge without mentioning it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{prelude::*, widgets::*};

use poise::serenity_prelude as serenity;

use crate::{
    ai::{
        api_full_message, chunk_reply, clear_history, is_api_full_err, is_rate_limit_err,
        mentions_name, ollama_chat, resolve_ollama_key, run_websearch, strip_name, valid_model_name,
        web_status, RATE_LIMIT_USER_MSG,
    },
    Error,
};

// ---------------------------------------------------------------------------
// settings + theme
// ---------------------------------------------------------------------------

pub(crate) struct TuiSettings {
    pub(crate) ai_enabled: bool,
    pub(crate) ai_model: String,
    pub(crate) ollama_host: String,
    pub(crate) ollama_key: String,
    pub(crate) token: Option<String>,
}

const ACCENT: Color = Color::Magenta;
const MINE: Color = Color::Green;
const BOT_MSG: Color = Color::Yellow;
const USER_MSG: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

// ---------------------------------------------------------------------------
// model
// ---------------------------------------------------------------------------

struct Msg {
    author: String,
    color: Color,
    /// Channel tag shown in the header line (e.g. "#general").
    /// Empty when the channel is obvious (local chat, per-channel view).
    tag: String,
    text: String,
    time: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelKind {
    Local,
    Spy,
    Live,
}

struct Channel {
    name: String,
    id: u64,
    kind: ChannelKind,
    messages: Vec<Msg>,
    scroll: usize,
    follow: bool,
    unread: usize,
    last_active: Option<Instant>,
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

const TYPING_TTL: Duration = Duration::from_secs(9);
const BOT_LIVE_SECS: u64 = 60;
const MAX_LIVE_CHANNELS: usize = 20;
const MAX_MSGS: usize = 300;
const NOTICE_SECS: u64 = 6;

struct App {
    channels: Vec<Channel>,
    active: usize,
    list_state: ListState,
    input: String,
    cursor: usize, // char index into input
    history: Vec<String>,
    hist_pos: Option<usize>,
    show_help: bool,
    notice: Option<(String, Instant)>,
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

fn mk_channel(name: &str, id: u64, kind: ChannelKind) -> Channel {
    Channel {
        name: name.to_string(),
        id,
        kind,
        messages: Vec::new(),
        scroll: 0,
        follow: true,
        unread: 0,
        last_active: None,
    }
}

fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| match c {
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
        })
        .sum()
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

enum Command {
    Help,
    Channels,
    Status,
    Forget,
    Ai(String),
    War(String),
    Sayas(String),
    Notify(String),
    Filter(String),
    Say { target: String, text: String },
    Typing(String),
    Websearch(String),
    Clear,
    Quit,
    Local(String),
    Unknown(String),
}

impl Command {
    fn parse(line: &str) -> Self {
        let text = line.trim();
        if text == "/help" || text == "help" {
            return Self::Help;
        }
        if text == "/quit" || text == "/exit" {
            return Self::Quit;
        }
        if text == "/clear" {
            return Self::Clear;
        }
        if text == "/channels" {
            return Self::Channels;
        }
        if text == "/status" {
            return Self::Status;
        }
        if text == "/forget" {
            return Self::Forget;
        }
        if text == "/ai" || text == "/ai status" {
            return Self::Ai(String::new());
        }
        for prefix in ["/ai ", "/war ", "/sayas ", "/notify ", "/filter ", "/typing "] {
            if text.starts_with(prefix) {
                let arg = text[prefix.len()..].trim().to_string();
                return match prefix {
                    "/ai " => Self::Ai(arg),
                    "/war " => Self::War(arg),
                    "/sayas " => Self::Sayas(arg),
                    "/notify " => Self::Notify(arg),
                    "/filter " => Self::Filter(arg),
                    _ => Self::Typing(arg),
                };
            }
        }
        for word in ["/war", "/sayas", "/notify", "/filter", "/typing"] {
            if text == word {
                return match word {
                    "/war" => Self::War(String::new()),
                    "/sayas" => Self::Sayas(String::new()),
                    "/notify" => Self::Notify(String::new()),
                    "/filter" => Self::Filter(String::new()),
                    _ => Self::Typing(String::new()),
                };
            }
        }
        if text.starts_with("/say ") {
            let rest = text["/say ".len()..].trim();
            if let Some((t, msg)) = rest.split_once(char::is_whitespace) {
                if !t.trim().is_empty() && !msg.trim().is_empty() {
                    return Self::Say {
                        target: t.trim().to_string(),
                        text: msg.trim().to_string(),
                    };
                }
            }
            return Self::Unknown("usage: /say #<id|name> <text>".to_string());
        }
        if text.starts_with("/websearch") {
            let q = text.strip_prefix("/websearch").unwrap_or("").trim().to_string();
            if q.is_empty() {
                return Self::Unknown("usage: /websearch <query>".to_string());
            }
            return Self::Websearch(q);
        }
        if text.starts_with('/') {
            return Self::Unknown("unknown command — F1 for help".to_string());
        }
        Self::Local(text.to_string())
    }

    /// Commands handled locally by the TUI (take precedence over forwarding).
    fn is_native(name: &str) -> bool {
        matches!(
            name,
            "help"
                | "quit"
                | "exit"
                | "clear"
                | "channels"
                | "status"
                | "forget"
                | "ai"
                | "war"
                | "sayas"
                | "notify"
                | "filter"
                | "say"
                | "typing"
                | "websearch"
        )
    }

    fn help_table() -> Vec<(String, String)> {
        vec![
            ("artixy <q>".into(), "chat with local AI".into()),
            ("typing in #live".into(), "sends as artixy there".into()),
            ("/run, /ps… in #live".into(), "run on discord as artixy".into()),
            ("//cmd in #live".into(), "force discord version".into()),
            ("/say #ch <text>".into(), "send as bot from anywhere".into()),
            ("/typing #ch".into(), "bot typing indicator".into()),
            ("/channels".into(), "list live discord channels".into()),
            ("/status".into(), "bot + AI + typing status".into()),
            ("/ai on|off|model".into(), "control AI (saves)".into()),
            ("/war on|off".into(), "protections (saves)".into()),
            ("/sayas on|off".into(), "say-as-artix (saves)".into()),
            ("/notify <id>|off".into(), "boot channel (saves)".into()),
            ("/filter mentions|all".into(), "filter #spy".into()),
            ("/websearch <q>".into(), "web search".into()),
            ("/forget".into(), "wipe chat memory here".into()),
            ("/clear".into(), "clear this view".into()),
            ("/quit".into(), "leave".into()),
        ]
    }
}

// ---------------------------------------------------------------------------
// App impl
// ---------------------------------------------------------------------------

impl App {
    fn new(settings: TuiSettings) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = ListState::default();
        state.select(Some(0));
        let mut app = Self {
            channels: vec![
                mk_channel("general", 1, ChannelKind::Local),
                mk_channel("spy", u64::MAX, ChannelKind::Spy),
            ],
            active: 0,
            list_state: state,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            hist_pos: None,
            show_help: false,
            notice: None,
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
            ACCENT,
            "",
            "welcome! type artixy <question> to chat, F1 for help.\n#spy mirrors live discord once the bot runs. Tab into a #live channel and just type — it sends as artixy, and /commands run there like on discord.",
        );
        app
    }

    // -- basics ------------------------------------------------------------

    fn active_channel(&self) -> &Channel {
        &self.channels[self.active]
    }

    fn chan(&self) -> u64 {
        self.channels[self.active].id
    }

    fn is_spy(&self) -> bool {
        self.channels[self.active].kind == ChannelKind::Spy
    }

    fn is_readonly(&self) -> bool {
        self.channels[self.active].kind != ChannelKind::Local
    }

    fn spy_idx(&self) -> Option<usize> {
        self.channels.iter().position(|c| c.kind == ChannelKind::Spy)
    }

    fn bot_live(&self) -> bool {
        self.last_feed
            .map(|t| t.elapsed().as_secs() < BOT_LIVE_SECS)
            .unwrap_or(false)
    }

    fn notify(&mut self, text: &str) {
        self.notice = Some((text.to_string(), Instant::now()));
    }

    fn fresh_notice(&self) -> Option<&str> {
        self.notice.as_ref().and_then(|(s, t)| {
            if t.elapsed().as_secs() < NOTICE_SECS {
                Some(s.as_str())
            } else {
                None
            }
        })
    }

    // -- typing ------------------------------------------------------------

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

    fn typing_summary(&self) -> Vec<(u64, String)> {
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
    }

    fn channel_tag(&self, id: u64) -> String {
        if id == u64::MAX {
            return "#spy".to_string();
        }
        if let Some(n) = self.id_to_name.get(&id) {
            if !n.trim().is_empty() {
                return format!("#{}", n.trim());
            }
        }
        format!("#{}", id)
    }

    // -- live feed ----------------------------------------------------------

    fn ensure_live_channel(&mut self, id: u64) {
        if id == u64::MAX || id == 1 {
            return;
        }
        if let Some(ch) = self.channels.iter_mut().find(|c| c.id == id) {
            if let Some(pretty) = self.id_to_name.get(&id).cloned() {
                let pretty = pretty.trim().to_string();
                if !pretty.is_empty() && ch.name != pretty {
                    ch.name = pretty;
                }
            }
            ch.last_active = Some(Instant::now());
            return;
        }
        if self.channels.iter().filter(|c| c.kind == ChannelKind::Live).count() >= MAX_LIVE_CHANNELS {
            // Evict the stalest live channel to stay tidy.
            let mut oldest: Option<(usize, Instant)> = None;
            for (i, c) in self.channels.iter().enumerate() {
                if c.kind != ChannelKind::Live {
                    continue;
                }
                let t = c.last_active.unwrap_or_else(|| Instant::now());
                if oldest.map(|(_, o)| t < o).unwrap_or(true) {
                    oldest = Some((i, t));
                }
            }
            if let Some((i, _)) = oldest {
                let removing_active = i == self.active;
                self.channels.remove(i);
                if removing_active {
                    self.goto(0);
                } else if i < self.active {
                    self.active -= 1;
                }
            } else {
                return;
            }
        }
        let name = match self.id_to_name.get(&id) {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => id.to_string(),
        };
        let mut ch = mk_channel(&name, id, ChannelKind::Live);
        ch.last_active = Some(Instant::now());
        self.channels.push(ch);
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
                        DIM,
                        "",
                        "no discord traffic yet — run the bot (`artixy` with a token) and live messages + typing appear here.",
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
                self.id_to_name.insert(e.channel, cname);
            }
            self.ensure_live_channel(e.channel);
            if e.kind == "typing" {
                if !e.author.trim().is_empty() {
                    self.typing.insert((e.channel, e.author.trim().to_string()), now);
                }
                continue;
            }
            if e.text.trim().is_empty() {
                continue;
            }
            let to_artixy = mentions_name(&e.text);
            let tag = self.channel_tag(e.channel);
            let (author, color) = if to_artixy {
                (format!("{} →artixy", e.author), ACCENT)
            } else if e.bot {
                (e.author.clone(), BOT_MSG)
            } else {
                (e.author.clone(), USER_MSG)
            };
            // Per-channel mirror (no tag needed — context is obvious).
            if let Some(pi) = self.channels.iter().position(|c| c.id == e.channel) {
                if pi != spy_idx {
                    self.say(pi, &author, color, "", &e.text);
                    self.bump(pi);
                }
            }
            // #spy mirror (tag shows where it came from).
            if self.filter_mentions && !to_artixy {
                continue;
            }
            self.say(spy_idx, &author, color, &tag, &e.text);
            self.bump(spy_idx);
        }
    }

    fn bump(&mut self, idx: usize) {
        if idx != self.active {
            self.channels[idx].unread = self.channels[idx].unread.saturating_add(1);
        }
    }

    // -- sending -------------------------------------------------------------

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
        for (id, name) in &self.id_to_name {
            if name.to_lowercase() == low {
                return Some(*id);
            }
        }
        None
    }

    fn send_as_bot(&mut self, target: String, text: String) {
        let Some(id) = self.resolve_target(&target) else {
            self.say_active("system", DIM, "", "unknown channel — /channels lists live names.");
            self.notify("unknown channel — see /channels");
            return;
        };
        let Some(token) = self.settings.token.clone() else {
            self.say_active("system", DIM, "", "no discord token — set discord_token, then restart tui.");
            return;
        };
        if text.trim().is_empty() {
            self.say_active("system", DIM, "", "usage: /say #<id|name> <text>");
            return;
        }
        let tx = self.tx.clone();
        let reply_to = self.chan();
        *self.pending.entry(reply_to).or_insert(0) += 1;
        let tag = self.channel_tag(id);
        tokio::spawn(async move {
            let http = serenity::Http::new(&token);
            let msg = match serenity::ChannelId::new(id).say(&http, &text).await {
                Ok(m) => format!("sent to {} (msg {})", tag, m.id.get()),
                Err(e) => format!("send to {} failed: {e}", tag),
            };
            let _ = tx.send(Job {
                channel: reply_to,
                reply: Reply::Text(vec![msg]),
            });
        });
        self.notify("sending as artixy…");
    }

    fn typing_as_bot(&mut self, target: String) {
        let Some(id) = self.resolve_target(&target) else {
            self.say_active("system", DIM, "", "unknown channel — /channels lists live names.");
            return;
        };
        let Some(token) = self.settings.token.clone() else {
            self.say_active("system", DIM, "", "no discord token configured.");
            return;
        };
        if target.trim().is_empty() {
            self.say_active("system", DIM, "", "usage: /typing #<id|name>");
            return;
        }
        let tx = self.tx.clone();
        let reply_to = self.chan();
        *self.pending.entry(reply_to).or_insert(0) += 1;
        tokio::spawn(async move {
            let http = serenity::Http::new(&token);
            let msg = match serenity::ChannelId::new(id).broadcast_typing(&http).await {
                Ok(_) => format!("typing as artixy in <#{id}>"),
                Err(e) => format!("typing failed in <#{id}>: {e}"),
            };
            let _ = tx.send(Job {
                channel: reply_to,
                reply: Reply::Text(vec![msg]),
            });
        });
    }

    // -- chat -----------------------------------------------------------------

    fn say(&mut self, idx: usize, author: &str, color: Color, tag: &str, text: &str) {
        let ch = &mut self.channels[idx];
        ch.messages.push(Msg {
            author: author.to_string(),
            color,
            tag: tag.to_string(),
            text: text.chars().take(4000).collect(),
            time: stamp(),
        });
        if ch.messages.len() > MAX_MSGS {
            let drop = ch.messages.len() - MAX_MSGS;
            ch.messages.drain(..drop);
        }
        ch.follow = true;
        ch.last_active = Some(Instant::now());
    }

    fn say_active(&mut self, author: &str, color: Color, tag: &str, text: &str) {
        let idx = self.active;
        self.say(idx, author, color, tag, text);
    }

    fn spawn_ai(&mut self, prompt: String) {
        let chan = self.chan();
        let idx = self.active;
        self.say(idx, "you", MINE, "", &prompt);
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
        self.say(idx, "you", MINE, "", &format!("/websearch {query}"));
        *self.pending.entry(chan).or_insert(0) += 1;
        let tx = self.tx.clone();
        let key = self.settings.ollama_key.clone();
        tokio::spawn(async move {
            if crate::ai::web_search_disabled() {
                let _ = tx.send(Job {
                    channel: chan,
                    reply: Reply::Text(vec![format!(
                        "web search is off — ask `artixy {query}` and I'll answer from what I know."
                    )]),
                });
                return;
            }
            let answer = run_websearch(&key, &query).await;
            let text = if crate::ai::web_search_disabled()
                || crate::ai::is_out_of_credits_err(&answer)
            {
                format!("web search is off — ask `artixy {query}` and I'll answer from what I know.")
            } else if answer.trim().is_empty() {
                format!("no web results for {query} — ask `artixy {query}` and I'll answer from what I know.")
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
        let enabled = self.settings.ai_enabled;
        tokio::spawn(async move {
            let web = web_status(&key).await;
            let state = if enabled { "enabled" } else { "disabled" };
            let text = format!("AI {state} · model `{model}` on `{host}`\n{web}");
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
                    self.say(idx, "artixy", ACCENT, "", &c);
                }
                if idx != self.active {
                    self.channels[idx].unread =
                        self.channels[idx].unread.saturating_add(n);
                }
            }
        }
    }

    // -- navigation ------------------------------------------------------------

    fn goto(&mut self, idx: usize) {
        if idx < self.channels.len() {
            self.active = idx;
            self.list_state.select(Some(idx));
            self.channels[idx].unread = 0;
            self.channels[idx].follow = true;
            self.hist_pos = None;
        }
    }

    fn next_channel(&mut self) {
        let n = self.channels.len().max(1);
        self.goto((self.active + 1) % n);
    }

    fn prev_channel(&mut self) {
        let n = self.channels.len().max(1);
        self.goto((self.active + n - 1) % n);
    }

    // -- input editing ----------------------------------------------------------

    fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let mut chars: Vec<char> = self.input.chars().collect();
        let at = self.cursor.min(chars.len());
        let mut i = at;
        for c in s.chars() {
            // Single-line composer: turn newlines into spaces.
            let c = if c == '\n' || c == '\r' { ' ' } else { c };
            chars.insert(i, c);
            i += 1;
        }
        self.input = chars.into_iter().collect();
        self.cursor = at + s.chars().count();
        self.hist_pos = None;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 && !self.input.is_empty() {
            let mut chars: Vec<char> = self.input.chars().collect();
            let at = self.cursor.min(chars.len());
            chars.remove(at - 1);
            self.input = chars.into_iter().collect();
            self.cursor -= 1;
            self.hist_pos = None;
        }
    }

    fn delete_char(&mut self) {
        let mut chars: Vec<char> = self.input.chars().collect();
        if self.cursor < chars.len() {
            chars.remove(self.cursor);
            self.input = chars.into_iter().collect();
            self.hist_pos = None;
        }
    }

    fn delete_word(&mut self) {
        let mut chars: Vec<char> = self.input.chars().collect();
        let mut at = self.cursor.min(chars.len());
        while at > 0 && chars[at - 1].is_whitespace() {
            chars.remove(at - 1);
            at -= 1;
        }
        while at > 0 && !chars[at - 1].is_whitespace() {
            chars.remove(at - 1);
            at -= 1;
        }
        self.input = chars.into_iter().collect();
        self.cursor = at;
        self.hist_pos = None;
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.hist_pos {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.hist_pos = Some(next);
        self.input = self.history[next].clone();
        self.cursor = self.input.chars().count();
    }

    fn history_next(&mut self) {
        let Some(i) = self.hist_pos else { return };
        if i + 1 >= self.history.len() {
            self.hist_pos = None;
            self.input.clear();
            self.cursor = 0;
        } else {
            self.hist_pos = Some(i + 1);
            self.input = self.history[i + 1].clone();
            self.cursor = self.input.chars().count();
        }
    }

    fn push_history(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.history.last().map(|l| l == line).unwrap_or(false) {
            return;
        }
        self.history.push(line.to_string());
        if self.history.len() > 100 {
            let drop = self.history.len() - 100;
            self.history.drain(..drop);
        }
    }

    // -- submit -------------------------------------------------------------------

    fn submit(&mut self) {
        let line = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.hist_pos = None;
        let text = line.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.push_history(&text);
        match Command::parse(&text) {
            Command::Quit => self.quit = true,
            Command::Clear => {
                let ch = &mut self.channels[self.active];
                ch.messages.clear();
                ch.scroll = 0;
            }
            Command::Help => self.show_help = true,
            Command::Channels => self.list_channels(),
            Command::Status => {
                self.local_status();
                self.spawn_status();
            }
            Command::Forget => {
                if self.is_readonly() {
                    self.say_active("system", DIM, "", "nothing to forget here — memory lives in #general.");
                    return;
                }
                clear_history(self.chan());
                self.say_active("system", DIM, "", "forgot the conversation here.");
            }
            Command::Ai(arg) => {
                if arg.trim().is_empty() {
                    self.spawn_status();
                } else {
                    self.handle_ai_cmd(&arg);
                }
            }
            Command::War(arg) => self.handle_bool_config("war_mode", &arg, "war mode"),
            Command::Sayas(arg) => self.handle_bool_config("sayas_enabled", &arg, "say-as-artix"),
            Command::Notify(arg) => self.handle_notify(&arg),
            Command::Filter(arg) => self.handle_filter(&arg),
            Command::Say { target, text } => self.send_as_bot(target, text),
            Command::Typing(t) => {
                if t.trim().is_empty() {
                    self.say_active("system", DIM, "", "usage: /typing #<id|name>");
                } else {
                    self.typing_as_bot(t);
                }
            }
            Command::Websearch(q) => self.spawn_search(q),
            Command::Local(msg) => self.chat(msg),
            Command::Unknown(hint) => {
                // In a remembered channel, bot commands run on discord as
                // artixy — no /say <id> needed.
                if self.try_forward_to_discord(&text) {
                    return;
                }
                self.say_active("system", DIM, "", &hint);
                self.notify(&hint);
            }
        }
    }

    /// In a remembered (live) channel, `/cmd` runs on discord when it names
    /// a real bot command (`//cmd` forces it). Returns true when handled.
    fn try_forward_to_discord(&mut self, text: &str) -> bool {
        if self.channels[self.active].kind != ChannelKind::Live {
            return false;
        }
        let t = text.trim();
        let forced = t.starts_with("//");
        let body = t.trim_start_matches('/').trim();
        let first = body.split_whitespace().next().unwrap_or("").to_lowercase();
        if first.is_empty() {
            return false;
        }
        let known = crate::tuirelay::bot_command_names().contains(&first.as_str());
        if !forced && (Command::is_native(&first) || !known) {
            return false;
        }
        let forward = format!("/{body}");
        self.forward_command(forward);
        true
    }

    /// Post `/cmd …` as artixy so the bot executes it like on discord.
    /// The message id is claimed so ONLY this exact message may run.
    fn forward_command(&mut self, text: String) {
        if !self.bot_live() {
            self.say_active("system", DIM, "", "bot looks OFFLINE — start it first or the command posts as text and never runs.");
            self.notify("bot offline — command not run");
            return;
        }
        let Some(token) = self.settings.token.clone() else {
            self.say_active("system", DIM, "", "no discord token — set discord_token, then restart tui.");
            return;
        };
        let id = self.chan();
        let tag = self.channel_tag(id);
        let tx = self.tx.clone();
        let reply_to = id;
        *self.pending.entry(reply_to).or_insert(0) += 1;
        let shown: String = text.chars().take(80).collect();
        tokio::spawn(async move {
            let http = serenity::Http::new(&token);
            // DIRECT bot post (author = the bot itself): the only kind the
            // relay check may execute. Webhook posts never execute.
            let msg = match serenity::ChannelId::new(id).say(&http, &text).await {
                Ok(m) => {
                    crate::tuirelay::claim_message(m.id.get());
                    format!("ran {shown} as artixy in {tag} — reply lands here.")
                }
                Err(e) => format!("command post to {tag} failed: {e}"),
            };
            let _ = tx.send(Job {
                channel: reply_to,
                reply: Reply::Text(vec![msg]),
            });
        });
        self.notify("running on discord…");
    }

    fn chat(&mut self, msg: String) {
        if self.is_spy() {
            self.say_active("system", DIM, "", "spy watches everything — Tab into a #channel to talk, or /say #ch <text>.");
            return;
        }
        if self.channels[self.active].kind == ChannelKind::Live {
            // Remembered channel: just type — it goes out as artixy.
            let tag = self.channel_tag(self.chan());
            self.send_as_bot(tag, msg);
            return;
        }
        if mentions_name(&msg) {
            let prompt = strip_name(&msg);
            if prompt.trim().is_empty() {
                self.say_active("artixy", ACCENT, "", "ping me with a question — `artixy <question>`");
            } else {
                self.spawn_ai(prompt);
            }
        } else {
            let idx = self.active;
            self.say(idx, "you", MINE, "", &msg);
        }
    }

    fn list_channels(&mut self) {
        if self.id_to_name.is_empty() {
            self.say_active("system", DIM, "", "no discord channels yet — run the bot with a token and chat in discord.");
            return;
        }
        let mut pairs: Vec<(u64, String)> =
            self.id_to_name.iter().map(|(k, v)| (*k, v.clone())).collect();
        pairs.sort_by_key(|(id, _)| *id);
        let mut lines = vec![format!("{} live channels (use with /say):", pairs.len())];
        for (id, name) in pairs.iter().take(20) {
            let typing = self.typing_in(*id);
            let t = if typing.is_empty() {
                String::new()
            } else {
                format!(" — typing: {}", typing.join(", "))
            };
            lines.push(format!("- #{name} (<#{id}>){t}"));
        }
        if pairs.len() > 20 {
            lines.push(format!("…and {} more", pairs.len() - 20));
        }
        self.say_active("system", DIM, "", &lines.join("\n"));
    }

    fn local_status(&mut self) {
        let live = if self.bot_live() { "LIVE" } else { "OFFLINE" };
        let typing = self.typing_summary();
        let typing_txt = if typing.is_empty() {
            "nobody typing".to_string()
        } else {
            let names: Vec<String> = typing
                .iter()
                .take(3)
                .map(|(ch, who)| format!("{who} in {}", self.channel_tag(*ch)))
                .collect();
            if typing.len() <= 3 {
                format!("typing: {}", names.join(" · "))
            } else {
                format!("typing: {} +{} more", names.join(" · "), typing.len() - 3)
            }
        };
        let token_txt = if self.settings.token.is_some() { "set" } else { "MISSING" };
        self.say_active(
            "system",
            DIM,
            "",
            &format!(
                "bot {live} · {} msgs · {} channels · {typing_txt} · token {token_txt} · filter {} · `{}`",
                self.feed_total,
                self.id_to_name.len(),
                if self.filter_mentions { "mentions" } else { "all" },
                self.settings.ai_model,
            ),
        );
    }

    fn handle_filter(&mut self, arg: &str) {
        match arg.trim().to_lowercase().as_str() {
            "" => self.say_active(
                "system",
                DIM,
                "",
                &format!(
                    "filter is `{}` — /filter mentions (only to artixy) or /filter all.",
                    if self.filter_mentions { "mentions" } else { "all" }
                ),
            ),
            "mentions" | "mention" | "artixy" | "on" => {
                self.filter_mentions = true;
                self.say_active("system", DIM, "", "#spy shows only messages to artixy. (/filter all to undo)");
                self.notify("spy filter: mentions");
            }
            "all" | "off" => {
                self.filter_mentions = false;
                self.say_active("system", DIM, "", "#spy shows all discord traffic.");
                self.notify("spy filter: all");
            }
            _ => self.say_active("system", DIM, "", "usage: /filter mentions|all"),
        }
    }

    fn handle_ai_cmd(&mut self, arg: &str) {
        let a = arg.trim();
        let low = a.to_lowercase();
        if low.is_empty() || low == "status" {
            self.spawn_status();
            return;
        }
        let mut want_on: Option<bool> = None;
        let mut model = String::new();
        let tokens: Vec<String> = a.split_whitespace().map(|w| w.to_string()).collect();
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
            } else if t.starts_with("model:") || t.starts_with("model=") {
                let raw = &tokens[i];
                if let Some(pos) = raw.find(':').or_else(|| raw.find('=')) {
                    model = raw[pos + 1..].trim().to_string();
                }
            } else if tokens.len() == 1 && valid_model_name(&tokens[i]) {
                model = tokens[i].trim().to_string();
            }
            i += 1;
        }
        if model.is_empty() && want_on.is_none() {
            if tokens.len() == 1 && valid_model_name(&tokens[0]) {
                let wlow = tokens[0].to_lowercase();
                if wlow != "on" && wlow != "off" && wlow != "status" {
                    model = tokens[0].clone();
                }
            }
        }
        if model.is_empty() && want_on.is_none() {
            self.say_active("system", DIM, "", "usage: /ai on|off|model <name>|status");
            return;
        }
        if !model.is_empty() && !valid_model_name(&model) {
            self.say_active("system", DIM, "", "bad model name — e.g. llama3.1 (letters, numbers, ._-:/).");
            return;
        }
        let m_opt = if model.is_empty() { None } else { Some(model.clone()) };
        match update_file_config(|c| {
            if let Some(on) = want_on {
                c.ai_enabled = on;
            } else if m_opt.is_some() {
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
                    DIM,
                    "",
                    &format!("ai saved: enabled={} model=`{}` (bot hot-applies).", cfg.ai_enabled, cfg.ai_model),
                );
                self.notify("ai settings saved");
            }
            Err(e) => self.say_active("system", DIM, "", &format!("config save failed: {e}")),
        }
    }

    fn handle_notify(&mut self, arg: &str) {
        let a = arg.trim();
        if a.is_empty() || a.eq_ignore_ascii_case("status") {
            match crate::config::load_file_config().notify_channel {
                Some(id) => self.say_active("system", DIM, "", &format!("boot messages go to <#{id}>. (/notify off)")),
                None => self.say_active("system", DIM, "", "boot messages are OFF. (/notify <channel-id>)"),
            }
            return;
        }
        if a.eq_ignore_ascii_case("off") {
            match update_file_config(|c| c.notify_channel = None) {
                Ok(_) => {
                    self.say_active("system", DIM, "", "boot messages OFF (saved).");
                    self.notify("notify off");
                }
                Err(e) => self.say_active("system", DIM, "", &format!("config save failed: {e}")),
            }
            return;
        }
        match a.parse::<u64>() {
            Ok(id) if id != 0 => match update_file_config(|c| c.notify_channel = Some(id)) {
                Ok(_) => {
                    self.say_active("system", DIM, "", &format!("boot messages → <#{id}> (saved)."));
                    self.notify("notify saved");
                }
                Err(e) => self.say_active("system", DIM, "", &format!("config save failed: {e}")),
            },
            _ => self.say_active("system", DIM, "", "usage: /notify <channel-id>|off"),
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
            self.say_active("system", DIM, "", &format!("{label} is {on}. (/{cmd} on|off)"));
            return;
        }
        if !matches!(low.as_str(), "on" | "off" | "true" | "false") {
            self.say_active("system", DIM, "", &format!("usage: /{cmd} on|off"));
            return;
        }
        let on = low == "on" || low == "true";
        let res = update_file_config(|c| match key {
            "war_mode" => c.war_mode = on,
            "sayas_enabled" => c.sayas_enabled = on,
            _ => {}
        });
        match res {
            Ok(_) => {
                self.say_active("system", DIM, "", &format!("{label} = {on} (saved, bot hot-applies)."));
                self.notify(&format!("{label} {on}"));
            }
            Err(e) => self.say_active("system", DIM, "", &format!("config save failed: {e}")),
        }
    }

    // -- keys --------------------------------------------------------------------

    fn scroll_by(&mut self, delta: isize) {
        let ch = &mut self.channels[self.active];
        if delta < 0 {
            ch.follow = false;
            ch.scroll = ch.scroll.saturating_sub((-delta) as usize);
        } else {
            ch.scroll = ch.scroll.saturating_add(delta as usize);
            // Reaching the bottom re-follows; clamped at render time.
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Help popup eats everything except close keys.
        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::F(1) => {
                    self.show_help = false;
                }
                _ => {}
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('d') | KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('u') | KeyCode::Char('k') => {
                    self.input.clear();
                    self.cursor = 0;
                }
                KeyCode::Char('w') => self.delete_word(),
                KeyCode::Char('l') => {
                    let ch = &mut self.channels[self.active];
                    ch.messages.clear();
                    ch.scroll = 0;
                }
                KeyCode::Up => self.scroll_by(-1),
                KeyCode::Down => self.scroll_by(1),
                KeyCode::End => {
                    self.channels[self.active].follow = true;
                }
                _ => {}
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(c) = key.code {
                if let Some(d) = c.to_digit(10) {
                    let idx = (d as usize).saturating_sub(1);
                    if idx < self.channels.len() {
                        self.goto(idx);
                        return;
                    }
                }
            }
        }
        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => {
                self.input.clear();
                self.cursor = 0;
                self.hist_pos = None;
            }
            KeyCode::Tab => self.next_channel(),
            KeyCode::BackTab => self.prev_channel(),
            KeyCode::Up => self.history_prev(),
            KeyCode::Down => self.history_next(),
            KeyCode::PageUp => self.scroll_by(-10),
            KeyCode::PageDown => self.scroll_by(10),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_char(),
            KeyCode::F(1) => self.show_help = true,
            KeyCode::Char(c) => self.insert_str(&c.to_string()),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// config helper
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// view
// ---------------------------------------------------------------------------

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

fn render(frame: &mut Frame, app: &mut App) {
    let size = frame.size();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(size);
    let (header, main, input_area, footer) = (rows[0], rows[1], rows[2], rows[3]);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(1)])
        .split(main);
    let (side, chat) = (cols[0], cols[1]);

    // Header: brand + status + typing (kept short so it never wraps).
    let live = app.bot_live();
    let typing = app.typing_summary();
    let typing_txt = if typing.is_empty() {
        String::new()
    } else if typing.len() <= 2 {
        format!(
            " · {}",
            typing
                .iter()
                .map(|(ch, who)| format!("{who} in {}", app.channel_tag(*ch)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(" · {} typing", typing.len())
    };
    let pending: usize = app.pending.values().sum();
    let work = if pending > 0 { " · working…" } else { "" };
    let filter = if app.filter_mentions { " · mentions" } else { "" };
    let header_line = Line::from(vec![
        Span::styled(" artixy ", Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(" control center "),
        Span::styled(
            if live { " ● live " } else { " ○ off " },
            Style::default().fg(if live { Color::Green } else { DIM }),
        ),
        Span::styled(
            format!("{}{}{}", app.active_channel().name, filter, work),
            Style::default().fg(Color::White),
        ),
        Span::styled(typing_txt, Style::default().fg(DIM)),
    ]);
    frame.render_widget(Paragraph::new(header_line), header);

    // Sidebar: grouped, highlighted, with unread + typing marks.
    let mut items: Vec<ListItem> = Vec::new();
    for (i, c) in app.channels.iter().enumerate() {
        let mark = if !app.typing_in(c.id).is_empty() {
            " …"
        } else {
            ""
        };
        let unread = if c.unread > 0 {
            format!(" ({})", c.unread)
        } else {
            String::new()
        };
        let icon = match c.kind {
            ChannelKind::Local => "#",
            ChannelKind::Spy => "○",
            ChannelKind::Live => "#",
        };
        let label = format!("{icon} {}{unread}{mark}", c.name);
        let mut style = Style::default().fg(DIM);
        if i == app.active {
            style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
        } else if c.unread > 0 {
            style = Style::default().fg(Color::Yellow);
        }
        items.push(ListItem::new(Line::from(Span::styled(label, style))));
    }
    app.list_state.select(Some(app.active));
    let side_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(DIM))
                .title(" channels "),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(side_list, side, &mut app.list_state);

    // Chat: one header line per message (author · time · tag), body as-is.
    // Paragraph::wrap handles display wrapping — no manual wrapping.
    let mut lines: Vec<Line> = Vec::new();
    for m in &app.active_channel().messages {
        let mut head: Vec<Span> = vec![Span::styled(
            m.author.clone(),
            Style::default().fg(m.color).add_modifier(Modifier::BOLD),
        )];
        head.push(Span::styled(format!("  {}", m.time), Style::default().fg(DIM)));
        if !m.tag.is_empty() {
            head.push(Span::styled(format!("  {}", m.tag), Style::default().fg(DIM)));
        }
        lines.push(Line::from(head));
        for part in m.text.split('\n') {
            lines.push(Line::from(Span::raw(format!("  {part}"))));
        }
        lines.push(Line::from(""));
    }
    let cur_typing = app.typing_in(app.chan());
    if !cur_typing.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  … {} typing", cur_typing.join(", ")),
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("  nothing here yet", Style::default().fg(DIM))));
    }
    let view_h = chat.height.saturating_sub(2) as usize;
    let max_off = lines.len().saturating_sub(view_h);
    {
        let ch = &mut app.channels[app.active];
        if ch.follow {
            ch.scroll = max_off;
        } else {
            ch.scroll = ch.scroll.min(max_off);
        }
    }
    let scroll = app.channels[app.active].scroll;
    let ch = app.active_channel();
    let mut title = format!(" #{} ", ch.name);
    if ch.kind == ChannelKind::Spy {
        title.push_str("· watch-only ");
    } else if ch.kind == ChannelKind::Live {
        title.push_str("· as artixy ");
    }
    if !ch.follow && max_off > 0 {
        title.push_str("· ↑ history ");
    }
    let body = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(DIM))
                .title(title),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));
    frame.render_widget(body, chat);
    // Thin scrollbar so position is visible while scrolled back.
    if max_off > 0 {
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        let mut bar_state = ScrollbarState::new(max_off).position(scroll);
        frame.render_stateful_widget(bar, chat, &mut bar_state);
    }

    // Input: single-line composer, native cursor.
    let state_txt = if !cur_typing.is_empty() {
        format!("{} typing…", cur_typing.join(", "))
    } else if pending > 0 {
        "artixy is typing…".to_string()
    } else if app.is_spy() {
        format!("#{} · watch-only · Tab to a channel", app.active_channel().name)
    } else if app.is_readonly() {
        format!("#{} · message as artixy", app.active_channel().name)
    } else {
        format!("#{} · message", app.active_channel().name)
    };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.is_readonly() { DIM } else { ACCENT }))
        .title(format!(" {state_txt} "));
    let inner = input_block.inner(input_area);
    frame.render_widget(input_block, input_area);
    frame.render_widget(Paragraph::new(app.input.as_str()), inner);
    if !app.show_help {
        let before: String = app.input.chars().take(app.cursor).collect();
        let cx = inner.x.saturating_add(display_width(&before) as u16).min(inner.x + inner.width.saturating_sub(1));
        frame.set_cursor(cx, inner.y);
    }

    // Footer: hints, or a transient notice.
    let footer_line = if let Some(n) = app.fresh_notice() {
        Line::from(vec![Span::styled(format!(" {n}"), Style::default().fg(Color::Yellow))])
    } else {
        Line::from(vec![Span::styled(
            " Tab channels · ↑↓ history · PgUp/Dn scroll · F1 help · /quit ",
            Style::default().fg(DIM),
        )])
    };
    frame.render_widget(Paragraph::new(footer_line), footer);

    // Help popup.
    if app.show_help {
        let area = centered_rect(70, 78, size);
        frame.render_widget(Clear, area);
        let mut help_lines: Vec<Line> = vec![
            Line::from(Span::styled("keys", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
            Line::from("  Tab / Shift+Tab   next / previous channel"),
            Line::from("  ↑ / ↓             input history"),
            Line::from("  PgUp / PgDn       scroll chat (Ctrl+↑/↓ = 1 line)"),
            Line::from("  Alt+1..9          jump to channel"),
            Line::from("  Ctrl+U / Ctrl+W   clear input / delete word"),
            Line::from("  F1                this help · Esc closes"),
            Line::from(""),
            Line::from(Span::styled("commands", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        ];
        for (cmd, desc) in Command::help_table() {
            help_lines.push(Line::from(vec![
                Span::styled(format!("  {cmd:<20}"), Style::default().fg(Color::White)),
                Span::styled(desc, Style::default().fg(DIM)),
            ]));
        }
        help_lines.push(Line::from(""));
        help_lines.push(Line::from(Span::styled("  Esc / F1 closes", Style::default().fg(DIM))));
        let help = Paragraph::new(help_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT))
                .title(" help "),
        );
        frame.render_widget(help, area);
    }
}

// ---------------------------------------------------------------------------
// controller
// ---------------------------------------------------------------------------

pub(crate) async fn run_tui(settings: TuiSettings) -> Result<(), Error> {
    crossterm::terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    crossterm::execute!(
        out,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste,
    )?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new(settings);

    let res = run_loop(&mut terminal, &mut app).await;

    let _ = crossterm::terminal::disable_raw_mode();
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
    )?;
    terminal.show_cursor()?;
    res
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<(), Error> {
    let mut frame: u64 = 0;
    loop {
        app.drain();
        app.prune_typing();
        frame += 1;
        if frame % 8 == 0 {
            app.poll_feed();
        }
        terminal.draw(|f| render(f, app))?;
        if app.quit {
            break;
        }
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(k) => {
                    if k.kind == KeyEventKind::Release {
                        continue;
                    }
                    app.on_key(k);
                }
                Event::Paste(s) => app.insert_str(&s),
                _ => {}
            }
        }
    }
    Ok(())
}

pub(crate) fn local_settings(
    ai_enabled: bool,
    ai_model: String,
    ollama_host: String,
    ollama_key_cfg: &str,
    token: Option<String>,
) -> TuiSettings {
    TuiSettings {
        ai_enabled,
        ai_model,
        ollama_host,
        ollama_key: resolve_ollama_key(ollama_key_cfg),
        token,
    }
}
