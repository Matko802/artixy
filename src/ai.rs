use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

use crate::Error;

struct HistoryItem {
    role: String,
    content: String,
}

fn history_map() -> &'static Mutex<HashMap<u64, VecDeque<HistoryItem>>> {
    static HISTORY: OnceLock<Mutex<HashMap<u64, VecDeque<HistoryItem>>>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn snapshot(channel: u64) -> Vec<HistoryItem> {
    history_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&channel)
        .map(|q| {
            q.iter()
                .map(|e| HistoryItem {
                    role: e.role.clone(),
                    content: e.content.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn push(channel: u64, role: String, content: String) {
    let mut map = history_map().lock().unwrap_or_else(|e| e.into_inner());
    if !map.contains_key(&channel) && map.len() >= 200 {
        if let Some(k) = map.keys().next().cloned() {
            map.remove(&k);
        }
    }
    let q = map.entry(channel).or_insert_with(VecDeque::new);
    q.push_back(HistoryItem { role, content });
    while q.len() > 10 {
        q.pop_front();
    }
    loop {
        let total: usize = q.iter().map(|e| e.content.len()).sum();
        if total <= 3000 || q.is_empty() {
            break;
        }
        q.pop_front();
    }
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .expect("reqwest client builds")
    })
}

pub(crate) fn default_model() -> String {
    "llama3.1".to_string()
}

pub(crate) fn default_host() -> String {
    "http://127.0.0.1:11434".to_string()
}

pub(crate) fn resolve_host(configured: &str) -> String {
    for key in ["OLLAMA_HOST", "OLLAMA_URL"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim().trim_end_matches('/').to_string();
            if !v.is_empty() {
                return v;
            }
        }
    }
    let v = configured.trim().trim_end_matches('/').to_string();
    if v.is_empty() {
        default_host()
    } else {
        v
    }
}

pub(crate) fn valid_model_name(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    if s.starts_with(['.', '-', '/', ':']) || s.ends_with(['.', '-', '/', ':']) {
        return false;
    }
    if s.contains("..") || s.contains("//") || s.contains(' ') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/'))
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    message: Option<ChatMessageOwned>,
    #[serde(default)]
    response: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessageOwned {
    #[serde(default)]
    content: String,
}

pub(crate) fn clean_reply(text: &str) -> String {
    let mut collapsed = String::new();
    let mut blanks = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks <= 1 {
                collapsed.push('\n');
            }
        } else {
            blanks = 0;
            collapsed.push_str(line);
            collapsed.push('\n');
        }
    }
    collapsed.trim().to_string()
}

pub(crate) fn strip_leading_speaker(text: &str, names: &[String]) -> String {
    let mut out = text.trim_start().to_string();
    loop {
        let t = out.trim_start();
        if !t.starts_with('[') {
            break;
        }
        let end = match t.find(']') {
            Some(e) if e > 1 && e <= 65 => e,
            _ => break,
        };
        let after = t[end + 1..].trim_start();
        if !after.starts_with(':') {
            break;
        }
        out = after[1..].trim_start().to_string();
    }
    let mut ordered: Vec<&String> = names.iter().collect();
    ordered.sort_by_key(|n| std::cmp::Reverse(n.len()));
    loop {
        let t = out.trim_start();
        let mut hit = false;
        for n in &ordered {
            let n = n.trim();
            if n.is_empty() {
                continue;
            }
            if t.len() > n.len() && t[..n.len()] == *n && t[n.len()..].starts_with(':') {
                out = t[n.len() + 1..].trim_start().to_string();
                hit = true;
                break;
            }
        }
        if !hit {
            break;
        }
    }
    out
}

fn meta_preamble_len(text: &str) -> usize {
    let starters = [
        "let me summarize",
        "just to sum it up",
        "just to summarize",
        "let's simplify",
        "let me simplify",
        "to simplify,",
        "too much info",
        "i got carried away",
        "i think i got",
        "here is a summary",
        "here's a summary",
        "to summarize,",
        "in summary,",
    ];
    let t = text.trim_start();
    let low = t.to_lowercase();
    for s in starters {
        if !low.starts_with(s) {
            continue;
        }
        let rest = &t[s.len()..];
        if s.ends_with(',') || rest.starts_with([':', ',']) {
            let cut = if rest.starts_with([':', ',']) {
                s.len() + 1
            } else {
                s.len()
            };
            if t[cut..].trim().is_empty() {
                return 0;
            }
            return cut;
        }
        let cut = match rest.find(['.', '!', '?']) {
            Some(i) => s.len() + i + 1,
            None => return 0,
        };
        if t[cut..].trim().is_empty() {
            return 0;
        }
        return cut;
    }
    0
}

pub(crate) fn strip_meta_preamble(text: &str) -> String {
    let mut out = text.to_string();
    loop {
        let cut = meta_preamble_len(&out);
        if cut == 0 {
            break;
        }
        out = out.trim_start()[cut..].trim_start().to_string();
    }
    out
}

pub(crate) fn sanitize_reply(raw: &str, names: &[String]) -> String {
    strip_meta_preamble(&strip_leading_speaker(&clean_reply(raw), names))
}

pub(crate) fn finalize_reply(
    channel: u64,
    tagged: String,
    names: &[String],
    raw: &str,
) -> Result<String, Error> {
    let text = sanitize_reply(raw, names);
    if text.trim().is_empty() {
        return Err("ollama returned an empty reply".into());
    }
    push(channel, "user".to_string(), tagged);
    push(channel, "assistant".to_string(), text.clone());
    Ok(text)
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    #[serde(default)]
    name: String,
}

pub(crate) fn is_rate_limit_err(s: &str) -> bool {
    let t = s.to_lowercase();
    t.contains("429")
        || t.contains("rate limit")
        || t.contains("rate_limit")
        || t.contains("rate-limited")
        || t.contains("too many requests")
        || t.contains("quota")
}

pub(crate) fn is_api_full_err(s: &str) -> bool {
    if is_rate_limit_err(s) {
        return true;
    }
    let t = s.to_lowercase();
    t.contains("503")
        || t.contains("529")
        || t.contains("overload")
        || t.contains("capacity")
        || t.contains("server is busy")
        || t.contains("try again in a bit")
        || t.contains("api full")
}

pub(crate) fn api_full_message() -> String {
    "Sorry, I'm running hot right now (API full/rate limited) — try again in a minute.".to_string()
}

async fn chat_once(
    url: &str,
    model: &str,
    messages: &[serde_json::Value],
) -> Result<ChatMessageOwned, Error> {
    let req = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    let resp = client().post(url).json(&req).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let body: String = body.chars().take(300).collect();
        return Err(format!("ollama {status}: {body}").into());
    }
    let parsed: ChatResponse = resp.json().await?;
    if let Some(m) = parsed.message {
        return Ok(m);
    }
    if let Some(r) = parsed.response {
        if !r.trim().is_empty() {
            return Ok(ChatMessageOwned { content: r });
        }
    }
    Err("ollama returned an empty reply".into())
}

pub(crate) fn stale_history_line(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    t.contains("running hot right now")
}

pub(crate) async fn glitch_text(host: &str, model: &str, system_prompt: &str) -> String {
    // Don't hammer a full API with another call — return a helpful static fallback.
    // Callers that already know the error was api-full should prefer api_full_message().
    let url = format!("{}/api/chat", host.trim_end_matches('/'));
    let mut messages = Vec::new();
    if !system_prompt.trim().is_empty() {
        messages.push(serde_json::json!({"role": "system", "content": system_prompt}));
    }
    messages.push(
        serde_json::json!({"role": "user", "content": "You just glitched out. Tell the user in one short sentence, no details."}),
    );
    match chat_once(&url, model, &messages).await {
        Ok(m) => {
            let text = strip_meta_preamble(&strip_leading_speaker(&clean_reply(&m.content), &[]));
            if text.trim().is_empty() {
                "sorry, glitched out — try again in a sec".to_string()
            } else if is_api_full_err(&text) {
                api_full_message()
            } else {
                text.chars().take(300).collect()
            }
        }
        Err(e) if is_api_full_err(&e.to_string()) => api_full_message(),
        Err(_) => "sorry, glitched out — try again in a sec".to_string(),
    }
}

pub(crate) async fn ollama_chat(
    host: &str,
    model: &str,
    channel: u64,
    speaker: &str,
    prompt: &str,
    system_prompt: &str,
) -> Result<String, Error> {
    let host = host.trim_end_matches('/');
    let url = format!("{host}/api/chat");
    let tagged = format!("[{}]: {}", speaker_tag(speaker), prompt);
    let past = snapshot(channel);
    let mut names: Vec<String> = vec![speaker_tag(speaker)];
    for e in &past {
        if e.role == "assistant" {
            continue;
        }
        let t = e.content.trim_start();
        if let Some(rest) = t.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                if end > 0 && end <= 64 {
                    let n = rest[..end].trim().to_string();
                    if !n.is_empty() && !names.contains(&n) {
                        names.push(n);
                    }
                }
            }
        }
    }
    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(past.len() + 2);
    if !system_prompt.trim().is_empty() {
        messages.push(serde_json::json!({"role": "system", "content": system_prompt}));
    }
    for e in &past {
        let role = if e.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        if role == "assistant" {
            let cleaned = sanitize_reply(&e.content, &names);
            if cleaned.trim().is_empty() || stale_history_line(&cleaned) {
                continue;
            }
            messages.push(serde_json::json!({"role": role, "content": cleaned}));
        } else {
            messages.push(serde_json::json!({"role": role, "content": e.content}));
        }
    }
    messages.push(serde_json::json!({"role": "user", "content": tagged.clone()}));
    // Repeat the backstory last so the model follows the current TOML
    // prompt strictly instead of drifting into old history style.
    if !system_prompt.trim().is_empty() {
        messages.push(serde_json::json!({"role": "system", "content": system_prompt}));
    }
    let first = match chat_once(&url, model, &messages).await {
        Ok(m) => m,
        Err(e) if is_api_full_err(&e.to_string()) => {
            eprintln!("ollama_chat: api full on first call, offline fallback");
            let fb = api_full_message();
            push(channel, tagged.clone(), tagged.clone());
            push(channel, "assistant".to_string(), fb.clone());
            return Ok(fb);
        }
        Err(e) => return Err(e),
    };
    finalize_reply(channel, tagged, &names, &first.content)
}

pub(crate) fn clear_history(channel: u64) {
    if let Ok(mut map) = history_map().lock() {
        map.remove(&channel);
    }
}

pub(crate) fn clear_all_history() {
    if let Ok(mut map) = history_map().lock() {
        map.clear();
    }
}

pub(crate) fn record_artixy(channel: u64, text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    let kept: String = t.chars().take(1500).collect();
    push(channel, "assistant".to_string(), kept);
}

pub(crate) async fn model_present(host: &str, model: &str) -> Option<bool> {
    let url = format!("{}/api/tags", host.trim_end_matches('/'));
    let resp = client().get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let tags: TagsResponse = resp.json().await.ok()?;
    let want = model.trim().to_lowercase();
    let base = want.split(':').next().unwrap_or(&want);
    Some(tags.models.iter().any(|m| {
        let n = m.name.to_lowercase();
        n == want || n.split(':').next().unwrap_or(&n) == base
    }))
}

pub(crate) fn speaker_tag(raw: &str) -> String {
    let flat: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean: String = flat
        .chars()
        .map(|c| match c {
            '[' | ']' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let clean: String = clean.chars().take(64).collect();
    if clean.trim().is_empty() {
        "someone".to_string()
    } else {
        clean
    }
}

pub(crate) fn strip_mention(content: &str, bot_id: u64) -> String {
    content
        .replace(&format!("<@{bot_id}>"), "")
        .replace(&format!("<@!{bot_id}>"), "")
        .trim()
        .to_string()
}

pub(crate) fn mentions_name(content: &str) -> bool {
    content.to_lowercase().contains("artixy")
}

pub(crate) fn strip_name(content: &str) -> String {
    content
        .split_whitespace()
        .filter(|w| !w.to_lowercase().contains("artixy"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn chunk_reply(s: &str) -> Vec<String> {
    const MAX: usize = 1900;
    const MAX_CHUNKS: usize = 4;
    let s = s.trim();
    if s.chars().count() <= MAX {
        return vec![s.to_string()];
    }
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for line in s.lines() {
        let line_len = line.chars().count() + 1;
        if line_len > MAX {
            if !cur.trim().is_empty() {
                chunks.push(cur.trim_end().to_string());
                cur = String::new();
                cur_len = 0;
            }
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(MAX) {
                chunks.push(piece.iter().collect());
                if chunks.len() >= MAX_CHUNKS {
                    break;
                }
            }
            if chunks.len() >= MAX_CHUNKS {
                break;
            }
            continue;
        }
        if cur_len + line_len > MAX {
            chunks.push(cur.trim_end().to_string());
            cur = String::new();
            cur_len = 0;
            if chunks.len() >= MAX_CHUNKS {
                break;
            }
        }
        cur.push_str(line);
        cur.push('\n');
        cur_len += line_len;
    }
    if !cur.trim().is_empty() && chunks.len() < MAX_CHUNKS {
        chunks.push(cur.trim_end().to_string());
    }
    if chunks.is_empty() {
        vec![s.chars().take(MAX).collect()]
    } else {
        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_detects_429_and_quota() {
        assert!(is_rate_limit_err("ollama 429: too many requests"));
        assert!(is_rate_limit_err("Rate limit exceeded, try again"));
        assert!(is_rate_limit_err("quota exceeded"));
        assert!(!is_rate_limit_err("connection refused"));
    }

    #[test]
    fn api_full_covers_overload_and_503() {
        assert!(is_api_full_err("ollama 503: overloaded"));
        assert!(is_api_full_err("server is busy, try again in a bit"));
        assert!(is_api_full_err("ollama 429: too many requests"));
        assert!(!is_api_full_err("connection refused"));
    }

    #[test]
    fn stale_history_skips_api_full_lines() {
        assert!(stale_history_line(&api_full_message()));
    }
}
