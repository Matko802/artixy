use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

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
        .map(|q| q.iter().map(|e| HistoryItem { role: e.role.clone(), content: e.content.clone() }).collect())
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
    while q.len() > 20 {
        q.pop_front();
    }
    loop {
        let total: usize = q.iter().map(|e| e.content.len()).sum();
        if total <= 6000 || q.is_empty() {
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

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
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

#[derive(Deserialize, Default)]
#[allow(non_snake_case)]
struct DdgResponse {
    #[serde(default)]
    Answer: String,
    #[serde(default)]
    AbstractText: String,
    #[serde(default)]
    AbstractURL: String,
    #[serde(default)]
    RelatedTopics: Vec<DdgTopic>,
}

#[derive(Deserialize, Default)]
#[allow(non_snake_case)]
struct DdgTopic {
    #[serde(default)]
    Text: String,
    #[serde(default)]
    FirstURL: String,
}

#[derive(Deserialize, Default)]
struct WikiResponse {
    #[serde(default)]
    query: WikiQuery,
}

#[derive(Deserialize, Default)]
struct WikiQuery {
    #[serde(default)]
    search: Vec<WikiItem>,
}

#[derive(Deserialize, Default)]
struct WikiItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    snippet: String,
}

pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else if b == b' ' {
            out.push_str("%20");
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn find_urls(s: &str) -> Vec<String> {
    s.split_whitespace()
        .filter_map(|w| {
            let t = w.trim_matches(|c| matches!(c, '<' | '>' | '"' | '\'' | '(' | ')')).to_string();
            let t = t.trim_end_matches(|c| matches!(c, '.' | ',' | ';' | '!' | '?' | ':')).to_string();
            if t.starts_with("http://") || t.starts_with("https://") {
                if t.len() <= 500 {
                    return Some(t);
                }
            }
            None
        })
        .take(2)
        .collect()
}

pub(crate) fn needs_search(s: &str) -> bool {
    let l = s.to_lowercase();
    if l.starts_with("search ") || l.starts_with("google ") || l.starts_with("look up ") || l.starts_with("lookup ") {
        return true;
    }
    ["latest", "newest", "today", "yesterday", "current", "news", "price", "weather", "score", "who won", "what happened", "release", "on the internet", "on the web", "search the web", "look it up"]
        .iter()
        .any(|k| l.contains(k))
}

pub(crate) fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut in_script = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !in_tag && s[i..].starts_with("<script") {
            in_script = true;
        }
        if in_script && s[i..].starts_with("</script") {
            in_script = false;
        }
        let c = bytes[i] as char;
        if c == '<' {
            in_tag = true;
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if c == '>' {
            in_tag = false;
            i += 1;
            continue;
        }
        if !in_tag && !in_script {
            out.push(c);
        }
        i += 1;
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn fetch_url_text(url: &str) -> Option<String> {
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        client().get(url).send(),
    )
    .await
    .ok()?
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        resp.text(),
    )
    .await
    .ok()?
    .ok()?;
    let text = strip_html(&body);
    let text: String = text.chars().take(3000).collect();
    if text.trim().is_empty() {
        return None;
    }
    Some(text)
}

async fn ddg_search(query: &str) -> Option<String> {
    let url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
        percent_encode(query)
    );
    let resp = tokio::time::timeout(std::time::Duration::from_secs(12), client().get(&url).send())
        .await
        .ok()?
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: DdgResponse = tokio::time::timeout(std::time::Duration::from_secs(12), resp.json())
        .await
        .ok()?
        .ok()?;
    let mut parts = Vec::new();
    if !data.Answer.trim().is_empty() {
        parts.push(data.Answer.trim().to_string());
    }
    if !data.AbstractText.trim().is_empty() {
        let mut a = data.AbstractText.trim().to_string();
        if !data.AbstractURL.trim().is_empty() {
            a.push_str(&format!(" ({})", data.AbstractURL.trim()));
        }
        parts.push(a);
    }
    for t in data.RelatedTopics.iter().take(3) {
        if !t.Text.trim().is_empty() {
            parts.push(t.Text.trim().to_string());
        }
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("\n- ");
    Some(joined.chars().take(3000).collect())
}

async fn wiki_search(query: &str) -> Option<String> {
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&format=json&srlimit=3",
        percent_encode(query)
    );
    let resp = tokio::time::timeout(std::time::Duration::from_secs(12), client().get(&url).send())
        .await
        .ok()?
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: WikiResponse = tokio::time::timeout(std::time::Duration::from_secs(12), resp.json())
        .await
        .ok()?
        .ok()?;
    if data.query.search.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for item in data.query.search.iter().take(3) {
        let snippet = strip_html(&item.snippet);
        parts.push(format!("{}: {}", item.title.trim(), snippet.trim()));
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("\n- ");
    Some(joined.chars().take(3000).collect())
}

async fn web_context(prompt: &str) -> String {
    let mut blocks = Vec::new();
    for url in find_urls(prompt) {
        if let Some(text) = fetch_url_text(&url).await {
            blocks.push(format!("Page {url}:\n{text}"));
        }
        if blocks.join("\n").len() > 3500 {
            break;
        }
    }
    let q = prompt.trim();
    if !q.is_empty() && q.chars().count() <= 300 && (needs_search(q) || blocks.is_empty() && q.chars().count() > 2) {
        let query: String = q.chars().take(200).collect();
        if needs_search(q) {
            if let Some(r) = ddg_search(&query).await {
                blocks.push(format!("Web search for {query}:\n- {r}"));
            } else if let Some(r) = wiki_search(&query).await {
                blocks.push(format!("Wikipedia search for {query}:\n- {r}"));
            }
        } else if find_urls(q).is_empty() {
            if let Some(r) = ddg_search(&query).await {
                if !r.trim().is_empty() {
                    blocks.push(format!("Web search for {query}:\n- {r}"));
                }
            }
        }
    }
    let joined = blocks.join("\n\n");
    joined.chars().take(4000).collect()
}

const SYSTEM_PROMPT: &str = "You are artixy, a friendly furry artix linux. Talk like a normal neko human, casual and a bit silly and simple messages. \
Be helpful and concise, keep replies under 2000 characters. You can use Discord markdown. \
remember who is who.and type instead of @name just name";

pub(crate) async fn ollama_chat(host: &str, model: &str, channel: u64, speaker: &str, prompt: &str) -> Result<String, Error> {
    let host = host.trim_end_matches('/');
    let url = format!("{host}/api/chat");
    let tagged = format!("[{}]: {}", speaker_tag(speaker), prompt);
    let extra = web_context(prompt).await;
    let full = if extra.trim().is_empty() {
        tagged.clone()
    } else {
        format!("{tagged}\n\n[web info, use it when relevant]\n{extra}")
    };
    let past = snapshot(channel);
    let mut messages = Vec::with_capacity(past.len() + 2);
    messages.push(ChatMessage { role: "system", content: SYSTEM_PROMPT });
    for e in &past {
        let role = if e.role == "assistant" { "assistant" } else { "user" };
        messages.push(ChatMessage { role, content: e.content.as_str() });
    }
    messages.push(ChatMessage { role: "user", content: full.as_str() });
    let req = ChatRequest {
        model,
        messages,
        stream: false,
    };
    let resp = client().post(&url).json(&req).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let body: String = body.chars().take(300).collect();
        return Err(format!("ollama {status}: {body}").into());
    }
    let parsed: ChatResponse = resp.json().await?;
    let text = parsed
        .message
        .map(|m| m.content)
        .or(parsed.response)
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("ollama returned an empty reply".into());
    }
    push(channel, "user".to_string(), tagged);
    push(channel, "assistant".to_string(), text.clone());
    Ok(text)
}

pub(crate) fn clear_history(channel: u64) {
    if let Ok(mut map) = history_map().lock() {
        map.remove(&channel);
    }
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
    fn model_names_accepted_and_rejected() {
        for good in ["llama3.1", "qwen2.5-coder:7b", "mistral:7b-instruct", "hf.co/org/model:tag", "a/b_c-d.e:f"] {
            assert!(valid_model_name(good), "should accept {good}");
        }
        for bad in ["", "  ", "has space", "a/b//c", "a..b", "/lead", ".lead", "-lead", ":lead", "trail/", "trail.", "semi;colon", "quote'x", &"a".repeat(129)] {
            assert!(!valid_model_name(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn host_resolution_prefers_env_then_config_then_default() {
        std::env::remove_var("OLLAMA_HOST");
        std::env::remove_var("OLLAMA_URL");
        assert_eq!(resolve_host(""), default_host());
        assert_eq!(resolve_host("http://x:1234/"), "http://x:1234");
        std::env::set_var("OLLAMA_HOST", "http://env:11434/");
        assert_eq!(resolve_host("http://cfg:11434"), "http://env:11434");
        std::env::remove_var("OLLAMA_HOST");
    }

    #[test]
    fn mention_stripped_to_prompt() {        assert_eq!(strip_mention("<@123> hello", 123), "hello");
        assert_eq!(strip_mention("hey <@!123> hi", 123), "hey  hi");
        assert_eq!(strip_mention("<@123>", 123), "");
        assert_eq!(strip_mention("no mention", 999), "no mention");
    }

    #[test]
    fn speaker_tag_is_single_line_bracket_free_and_capped() {
        assert_eq!(speaker_tag("Bob"), "Bob");
        assert_eq!(speaker_tag("  spaced   name  "), "spaced name");
        assert_eq!(speaker_tag("a[b]c"), "a b c");
        assert_eq!(speaker_tag("line1\nline2"), "line1 line2");
        assert_eq!(speaker_tag("   "), "someone");
        assert_eq!(speaker_tag(""), "someone");
        let long = "x".repeat(200);
        assert_eq!(speaker_tag(&long).chars().count(), 64);
    }

    #[test]
    fn long_replies_chunk_within_limits() {
        let line = "x".repeat(100);
        let body = (0..60).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
        let chunks = chunk_reply(&body);
        assert!(!chunks.is_empty() && chunks.len() <= 4);
        for c in &chunks {
            assert!(c.chars().count() <= 1900, "chunk too big: {}", c.chars().count());
        }
        assert_eq!(chunk_reply("short"), vec!["short".to_string()]);
    }
}
