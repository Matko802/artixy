use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::Error;

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .expect("reqwest client builds")
    })
}

/// Default model used when nothing is configured yet.
pub(crate) fn default_model() -> String {
    "llama3.1".to_string()
}

/// Default Ollama base URL. Overridable via `OLLAMA_HOST` / `OLLAMA_URL` env
/// or the `ollama_host` config key (env wins when set).
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

/// Ollama model names look like `llama3.1`, `qwen2.5-coder:7b`,
/// `hf.co/org/model:tag`. Keep it tight so it can't become a shell/URL trick.
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

const SYSTEM_PROMPT: &str = "You are artixy, a cute furry Discord bot living in an Artix Linux VM community. \
You are warm, affectionate and playful, with a soft furry vibe: occasional `:3`, \
*does cute actions in asterisks*, gentle teasing, paws and tail energy — but keep it natural, never forced. \
Use emojis very sparingly: at most one per message, and often none at all. \
Reply helpfully and concisely. Keep replies under 1500 characters unless asked for more. \
You can use Discord markdown (code fences for code/commands). Never claim to be human. \
Each user message starts with the speaker's Discord name in brackets, e.g. `[Bob]: hello`. \
Remember who said what, address people by name when it fits, and never confuse one speaker for another.";

pub(crate) async fn ollama_chat(host: &str, model: &str, speaker: &str, prompt: &str) -> Result<String, Error> {
    let host = host.trim_end_matches('/');
    let url = format!("{host}/api/chat");
    let tagged = format!("[{}]: {}", speaker_tag(speaker), prompt);
    let req = ChatRequest {
        model,
        messages: vec![
            ChatMessage { role: "system", content: SYSTEM_PROMPT },
            ChatMessage { role: "user", content: &tagged },
        ],
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
    Ok(text)
}

/// Best-effort check whether `model` is already pulled locally.
/// Returns `None` when the server can't be reached / list can't parse
/// (caller should not block saving the model name in that case).
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

/// Clean a Discord display/username for the `[name]:` speaker tag:
/// single line, no brackets (they're the tag delimiters), capped length.
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

/// Strip `<@id>` / `<@!id>` mentions of the bot from a message, leaving the prompt.
pub(crate) fn strip_mention(content: &str, bot_id: u64) -> String {
    content
        .replace(&format!("<@{bot_id}>"), "")
        .replace(&format!("<@!{bot_id}>"), "")
        .trim()
        .to_string()
}

/// Split a long AI reply into Discord-sized chunks (<= 1900 chars each).
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
            // Single huge line: hard-split it.
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
