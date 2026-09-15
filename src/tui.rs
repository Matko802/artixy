use std::collections::HashMap;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{prelude::*, widgets::*};

use crate::{
    ai::{
        chunk_reply, clear_history, mentions_name, ollama_chat, resolve_ollama_key, run_websearch,
        strip_name, web_status,
    },
    Error,
};

pub(crate) struct TuiSettings {
    pub(crate) ai_model: String,
    pub(crate) ollama_host: String,
    pub(crate) ollama_key: String,
}

struct Msg {
    author: String,
    color: Color,
    text: String,
    time: String,
}

struct Channel {
    name: &'static str,
    id: u64,
    messages: Vec<Msg>,
    scroll: usize,
    follow: bool,
}

enum Reply {
    Text(Vec<String>),
}

struct Job {
    channel: u64,
    reply: Reply,
}

fn stamp() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60)
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
}

impl App {
    fn new(settings: TuiSettings) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = Self {
            channels: vec![
                Channel {
                    name: "general",
                    id: 1,
                    messages: Vec::new(),
                    scroll: 0,
                    follow: true,
                },
                Channel {
                    name: "bot-commands",
                    id: 2,
                    messages: Vec::new(),
                    scroll: 0,
                    follow: true,
                },
            ],
            active: 0,
            input: String::new(),
            cursor: 0,
            settings,
            pending: HashMap::new(),
            tx,
            rx,
            quit: false,
        };
        app.say(
            0,
            "artixy",
            Color::Magenta,
            "welcome to the local playground. type artixy <question> to chat, /websearch <q>, /ai, /forget, /clear, /quit.",
        );
        app
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
        if ch.messages.len() > 200 {
            let drop = ch.messages.len() - 200;
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
                for c in chunks {
                    self.say(idx, "artixy", Color::Magenta, &c);
                }
            }
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
        if text == "/help" {
            self.say_active(
                "system",
                Color::DarkGray,
                "artixy <question> chat, /websearch <q> search, /ai status, /forget wipe memory, /clear clear, tab switch channel, pgup/pgdn scroll, /quit leave",
            );
            return;
        }
        if text == "/forget" {
            clear_history(self.chan());
            self.say_active("system", Color::DarkGray, "forgot the conversation here.");
            return;
        }
        if text == "/ai" {
            self.spawn_status();
            return;
        }
        if let Some(q) = text.strip_prefix("/websearch") {
            let q = q.trim().to_string();
            if q.is_empty() {
                self.say_active("system", Color::DarkGray, "usage: /websearch <query>");
            } else {
                self.spawn_search(q);
            }
            return;
        }
        if text.starts_with('/') {
            self.say_active("system", Color::DarkGray, "unknown command, try /help");
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
            KeyCode::Tab => self.active = (self.active + 1) % self.channels.len(),
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
        .constraints([Constraint::Length(22), Constraint::Min(1)])
        .split(rows[1]);

    let title = Line::from(vec![
        Span::styled(
            " artixy ",
            Style::default().fg(Color::Black).bg(Color::Magenta),
        ),
        Span::raw(" local chat  |  tab: channel  pgup/pgdn: scroll  /quit: leave "),
    ]);
    frame.render_widget(Paragraph::new(title), rows[0]);

    let names: Vec<ListItem> = app
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let label = if i == app.active {
                format!("> #{}", c.name)
            } else {
                format!("  #{}", c.name)
            };
            let style = if i == app.active {
                Style::default().fg(Color::White)
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
    let view_h = cols[1].height.saturating_sub(2) as usize;
    let max_off = lines.len().saturating_sub(view_h);
    let ch = &mut app.channels[app.active];
    if ch.follow {
        ch.scroll = max_off;
    } else {
        ch.scroll = ch.scroll.min(max_off);
    }
    let scroll = ch.scroll;
    let body = Paragraph::new(lines)
        .block(Block::bordered().title(format!("#{} ", app.channels[app.active].name)))
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
    let pending: usize = app.pending.values().sum();
    let status = if pending > 0 {
        "artixy is typing…"
    } else {
        "message"
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

    loop {
        app.drain();
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
) -> TuiSettings {
    let _ = ai_enabled;
    TuiSettings {
        ai_model,
        ollama_host,
        ollama_key: resolve_ollama_key(ollama_key_cfg),
    }
}
