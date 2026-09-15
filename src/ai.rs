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

pub(crate) fn explicit_search_asked(prompt: &str) -> bool {
    let l = prompt.trim().to_lowercase();
    l.starts_with("search ")
        || l.starts_with("google ")
        || l.starts_with("look up ")
        || l.starts_with("lookup ")
        || l.contains("search the web")
        || l.contains("look it up")
        || l.contains("google it")
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
#[allow(dead_code)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
#[allow(dead_code)]
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
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize, Serialize, Clone)]
struct ToolCall {
    #[serde(default)]
    function: ToolFunc,
}

#[derive(Deserialize, Serialize, Clone, Default)]
struct ToolFunc {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Deserialize)]
struct SchemaOutput {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool: Option<SchemaTool>,
}

#[derive(Deserialize, Clone)]
struct SchemaTool {
    #[serde(default)]
    name: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    url: String,
}

fn tool_defs() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "websearch",
                "description": "Search the live web for fresh info like latest releases, news, prices, GPUs. Returns titles, links and snippets plus fetched page text.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query, e.g. latest nvidia gpu"
                        }
                    },
                    "required": ["query"]
                }
            }
        }
    ])
}

pub(crate) fn tool_query(args: &serde_json::Value) -> String {
    if let Some(s) = args.as_str() {
        return s.chars().take(200).collect();
    }
    if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
        return q.chars().take(200).collect();
    }
    if let Some(obj) = args.as_object() {
        for (_, v) in obj {
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() {
                    return s.chars().take(200).collect();
                }
            }
        }
    }
    String::new()
}

fn json_candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            let mut depth = 0;
            let mut in_str = false;
            let mut esc = false;
            let mut j = i;
            while j < chars.len() {
                let c = chars[j];
                if in_str {
                    if esc {
                        esc = false;
                    } else if c == '\\' {
                        esc = true;
                    } else if c == '"' {
                        in_str = false;
                    }
                } else if c == '"' {
                    in_str = true;
                } else if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        out.push(chars[i..=j].iter().collect());
                        break;
                    }
                }
                j += 1;
            }
            i = if j < chars.len() { j + 1 } else { chars.len() };
        } else {
            i += 1;
        }
    }
    out
}

fn parse_tool_json(obj: &str) -> Option<(String, String)> {
    if let Ok(parsed) = serde_json::from_str::<SchemaOutput>(obj) {
        if let Some(tool) = parsed.tool {
            if tool.name.eq_ignore_ascii_case("websearch") {
                let q = if !tool.query.trim().is_empty() {
                    tool.query
                } else {
                    tool.url
                };
                if !q.trim().is_empty() {
                    return Some((parsed.content, q.chars().take(200).collect()));
                }
            }
        } else if !parsed.content.trim().is_empty() {
            return None;
        }
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(obj) {
        let name = v
            .get("name")
            .and_then(|n| n.as_str())
            .or_else(|| v.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()))
            .unwrap_or("");
        if name.eq_ignore_ascii_case("websearch") {
            let args = v.get("parameters").or_else(|| v.get("arguments")).or_else(|| v.get("query"));
            let q = match args {
                Some(a) if a.is_string() => a.as_str().unwrap_or("").to_string(),
                Some(a) if a.is_object() => tool_query(a),
                None => v
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .map(tool_query)
                    .unwrap_or_default(),
                _ => String::new(),
            };
            if !q.trim().is_empty() {
                let content = v.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                return Some((content, q.chars().take(200).collect()));
            }
        }
    }
    None
}

pub(crate) fn schema_tool_call(text: &str) -> Option<(String, String)> {
    let mut t = text.trim().to_string();
    if t.starts_with("```") {
        t = t
            .trim_start_matches('`')
            .trim_start_matches("json")
            .trim_start_matches("JSON")
            .trim()
            .trim_end_matches('`')
            .trim()
            .to_string();
    }
    if t.starts_with('{') && t.ends_with('}') {
        if let Some(hit) = parse_tool_json(&t) {
            return Some(hit);
        }
    }
    for cand in json_candidates(&t) {
        if let Some(hit) = parse_tool_json(&cand) {
            return Some(hit);
        }
    }
    None
}

fn is_tool_json_line(line: &str) -> bool {
    let t = line.trim().trim_matches('`').trim();
    if !(t.starts_with('{') && t.ends_with('}')) {
        return false;
    }
    parse_tool_json(t).is_some()
}

pub(crate) fn clean_reply(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !is_tool_json_line(l))
        .map(|l| l.to_string())
        .collect();
    let mut out = lines.join("\n");
    if out.trim().is_empty() {
        if let Some((content, _)) = schema_tool_call(text) {
            out = content;
        }
    }
    let mut collapsed = String::new();
    let mut blanks = 0;
    for line in out.lines() {
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
            if t.len() > n.len()
                && t[..n.len()] == *n
                && t[n.len()..].starts_with(':')
            {
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
            let cut = if rest.starts_with([':', ',']) { s.len() + 1 } else { s.len() };
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

pub(crate) fn finalize_reply(channel: u64, tagged: String, names: &[String], raw: &str) -> Result<String, Error> {
    let text = sanitize_reply(raw, names);
    if text.trim().is_empty() {
        return Err("ollama returned an empty reply".into());
    }
    push(channel, "user".to_string(), tagged);
    push(channel, "assistant".to_string(), text.clone());
    Ok(text)
}

pub(crate) fn looks_like_search_placeholder(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    if t.chars().count() > 140 {
        return false;
    }
    t.starts_with("search")
        || t.starts_with("looking up")
        || t.contains("searched for")
        || t.contains("searching for")
        || t.contains("searching the web")
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

#[allow(dead_code)]
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

fn cookie_host(url: &str) -> String {
    let s = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    s.split('/').next().unwrap_or(s).to_lowercase()
}

fn cookie_jar() -> &'static Mutex<HashMap<String, HashMap<String, String>>> {
    static JAR: OnceLock<Mutex<HashMap<String, HashMap<String, String>>>> = OnceLock::new();
    JAR.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cookie_header(url: &str) -> Option<String> {
    let host = cookie_host(url);
    let jar = cookie_jar().lock().unwrap_or_else(|e| e.into_inner());
    let short = host.strip_prefix("www.").unwrap_or(&host);
    let mut pairs = Vec::new();
    for (h, cookies) in jar.iter() {
        if *h == host || *h == *short || host.ends_with(h.as_str()) {
            for (k, v) in cookies {
                pairs.push(format!("{k}={v}"));
            }
        }
    }
    if pairs.is_empty() {
        None
    } else {
        Some(pairs.join("; "))
    }
}

fn store_cookies(url: &str, resp: &reqwest::Response) {
    let host = cookie_host(url);
    let mut jar = cookie_jar().lock().unwrap_or_else(|e| e.into_inner());
    let entry = jar.entry(host).or_insert_with(HashMap::new);
    for value in resp.headers().get_all("set-cookie").iter() {
        if let Ok(s) = value.to_str() {
            let pair = s.split(';').next().unwrap_or("").trim();
            if let Some((k, v)) = pair.split_once('=') {
                let k = k.trim().to_string();
                let v = v.trim().to_string();
                if !k.is_empty() {
                    entry.insert(k, v);
                }
            }
        }
    }
}

fn web_get(url: &str) -> reqwest::RequestBuilder {
    let mut req = client()
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Accept-Language", "en-US,en;q=0.9");
    if let Some(c) = cookie_header(url) {
        req = req.header("Cookie", c);
    }
    req
}

pub(crate) async fn fetch_url_text(url: &str) -> Option<String> {
    let resp = tokio::time::timeout(std::time::Duration::from_secs(12), web_get(url).send())
        .await
        .ok()?
        .ok()?;
    store_cookies(url, &resp);
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

async fn linked_pages(prompt: &str) -> String {
    let mut blocks = Vec::new();
    for url in find_urls(prompt) {
        if let Some(text) = fetch_url_text(&url).await {
            blocks.push(format!("Page {url}:\n{text}"));
        }
        if blocks.join("\n").len() > 3500 {
            break;
        }
    }
    let joined = blocks.join("\n\n");
    joined.chars().take(4000).collect()
}

pub(crate) fn resolve_ollama_key(configured: &str) -> String {
    if let Ok(v) = std::env::var("OLLAMA_API_KEY") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return v;
        }
    }
    configured.trim().to_string()
}

#[derive(Deserialize, Default)]
struct HostedSearchResponse {
    #[serde(default)]
    results: Vec<HostedResult>,
}

#[derive(Deserialize, Default)]
struct HostedResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

#[derive(Deserialize, Default)]
struct HostedFetchResponse {
    #[serde(default)]
    content: String,
}

pub(crate) async fn hosted_search(key: &str, query: &str) -> Vec<(String, String, String)> {
    let req = serde_json::json!({"query": query, "max_results": 5});
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client()
            .post("https://ollama.com/api/web_search")
            .header("Authorization", format!("Bearer {key}"))
            .json(&req)
            .send(),
    )
    .await
    .ok()
    .and_then(|r| r.ok());
    let resp = match resp {
        Some(r) => r,
        None => return Vec::new(),
    };
    if !resp.status().is_success() {
        eprintln!("hosted_search: status={}", resp.status());
        return Vec::new();
    }
    let data: HostedSearchResponse = tokio::time::timeout(std::time::Duration::from_secs(15), resp.json())
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let out: Vec<(String, String, String)> = data
        .results
        .into_iter()
        .filter(|i| !i.title.trim().is_empty() && !i.url.trim().is_empty())
        .take(5)
        .map(|i| (i.title.trim().to_string(), i.url.trim().to_string(), i.content.trim().to_string()))
        .collect();
    eprintln!("hosted_search: results={}", out.len());
    out
}

pub(crate) async fn hosted_fetch(key: &str, url: &str) -> Option<String> {
    let req = serde_json::json!({"url": url});
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client()
            .post("https://ollama.com/api/web_fetch")
            .header("Authorization", format!("Bearer {key}"))
            .json(&req)
            .send(),
    )
    .await
    .ok()?
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: HostedFetchResponse = tokio::time::timeout(std::time::Duration::from_secs(15), resp.json())
        .await
        .ok()?
        .ok()?;
    let text = data.content.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(3000).collect())
}

pub(crate) async fn run_websearch(key: &str, query: &str) -> String {
    let t0 = std::time::Instant::now();
    let query: String = query.chars().take(200).collect();
    if key.trim().is_empty() {
        return String::new();
    }
    let mut blocks = Vec::new();
    let fresh = hosted_search(key, &query).await;
    if !fresh.is_empty() {
        eprintln!("run_websearch: src=ollama results={}", fresh.len());
        let mut lines = Vec::new();
        for (title, link, snip) in fresh.iter().take(5) {
            if snip.is_empty() {
                lines.push(format!("{title} ({link})"));
            } else {
                lines.push(format!("{title} ({link}): {snip}"));
            }
        }
        blocks.push(format!("Fresh web results for {query}:\n- {}\nAnswer directly with the facts and no preamble about searching, summarizing, or results.", lines.join("\n- ")));
        for (_, link, _) in fresh.iter().take(2) {
            if let Some(text) = hosted_fetch(key, link).await {
                blocks.push(format!("Page {link}:\n{text}"));
            }
            if blocks.join("\n").len() > 3500 {
                break;
            }
        }
    }
    let joined = blocks.join("\n\n");
    let out: String = joined.chars().take(4000).collect();
    eprintln!(
        "run_websearch: query_chars={} blocks={} out_chars={} ms={}",
        query.chars().count(),
        blocks.len(),
        out.chars().count(),
        t0.elapsed().as_millis()
    );
    out
}

pub(crate) async fn web_status(key: &str) -> String {
    if key.trim().is_empty() {
        return "web: FAIL no ollama_api_key in config or OLLAMA_API_KEY env".to_string();
    }
    let t0 = std::time::Instant::now();
    let n = hosted_search(key, "latest gpu").await.len();
    let ms = t0.elapsed().as_millis();
    if n == 0 {
        return format!("web: FAIL ollama hosted search returned nothing ms={ms}");
    }
    format!("web: ok src=ollama results={n} ms={ms}")
}

const SYSTEM_PROMPT: &str = "You are artixy, a friendly artix linux neko cat — always yourself, never anyone else. Chat like a normal neko human: casual, a bit silly, short replies, never starting with a name. \
You have a websearch tool — use it only when explicitly asked to search, and never mention it or output JSON, just give the answer with no preamble about your process.";

async fn chat_once(
    url: &str,
    model: &str,
    messages: &[serde_json::Value],
    with_tools: bool,
) -> Result<ChatMessageOwned, Error> {
    let mut req = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    if with_tools {
        req["tools"] = tool_defs();
    }
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
            return Ok(ChatMessageOwned {
                content: r,
                tool_calls: None,
            });
        }
    }
    Err("ollama returned an empty reply".into())
}

pub(crate) fn stale_history_line(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    t.contains("couldn't reach the web")
        || t.contains("could not reach the web")
        || t == "web search returned no results."
        || t.contains("web search is not configured")
}

pub(crate) async fn glitch_text(host: &str, model: &str) -> String {
    let url = format!("{}/api/chat", host.trim_end_matches('/'));
    let messages = vec![
        serde_json::json!({"role": "system", "content": SYSTEM_PROMPT}),
        serde_json::json!({"role": "user", "content": "You just glitched out. Tell the user cutely in one short sentence, no details."}),
    ];
    match chat_once(&url, model, &messages, false).await {
        Ok(m) => {
            let text = strip_meta_preamble(&strip_leading_speaker(&clean_reply(&m.content), &[]));
            if text.trim().is_empty() {
                "sorry, glitched out — try again in a sec".to_string()
            } else {
                text.chars().take(300).collect()
            }
        }
        Err(_) => "sorry, glitched out — try again in a sec".to_string(),
    }
}

pub(crate) async fn ollama_chat(host: &str, model: &str, okey: &str, channel: u64, speaker: &str, prompt: &str) -> Result<String, Error> {
    let host = host.trim_end_matches('/');
    let url = format!("{host}/api/chat");
    let want_web = explicit_search_asked(prompt);
    let tagged = format!("[{}]: {}", speaker_tag(speaker), prompt);
    let pages = linked_pages(prompt).await;
    let user_text = if pages.trim().is_empty() {
        tagged.clone()
    } else {
        format!("{tagged}\n\n[linked pages below, prefer over training data]\n{pages}")
    };
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
    messages.push(serde_json::json!({"role": "system", "content": SYSTEM_PROMPT}));
    for e in &past {
        let role = if e.role == "assistant" { "assistant" } else { "user" };
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
    messages.push(serde_json::json!({"role": "user", "content": user_text}));
    let mut used_tools = true;
    let first = match chat_once(&url, model, &messages, true).await {
        Ok(m) => m,
        Err(e) if e.to_string().contains("does not support tools") => {
            eprintln!("ollama_chat: model lacks tool support, retry without tools");
            used_tools = false;
            chat_once(&url, model, &messages, false).await?
        }
        Err(e) => return Err(e),
    };
    let mut calls: Vec<(String, String)> = Vec::new();
    if want_web {
        if let Some(list) = first.tool_calls.clone() {
            for c in list {
                if c.function.name.eq_ignore_ascii_case("websearch") {
                    let q = tool_query(&c.function.arguments);
                    if !q.trim().is_empty() {
                        calls.push((c.function.name.clone(), q));
                    }
                }
            }
        }
        if calls.is_empty() {
            if let Some((_, q)) = schema_tool_call(&first.content) {
                calls.push(("websearch".to_string(), q));
                messages.push(serde_json::json!({"role": "assistant", "content": first.content.clone()}));
            }
        } else {
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": first.content.clone(),
                "tool_calls": first.tool_calls.clone().unwrap_or_default().iter().map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null)).collect::<Vec<_>>(),
            }));
        }
        if calls.is_empty() && !used_tools && looks_like_search_placeholder(&first.content) {
            let auto_q: String = prompt.chars().take(200).collect();
            if !auto_q.trim().is_empty() {
                calls.push(("websearch".to_string(), auto_q));
            }
        }
    }
    if calls.is_empty() {
        if !looks_like_search_placeholder(&first.content) {
            return finalize_reply(channel, tagged, &names, &first.content);
        }
        let retry = chat_once(&url, model, &messages, false).await?;
        return finalize_reply(channel, tagged, &names, &retry.content);
    }
    let mut rounds = 0;
    let mut pending: Option<String> = calls.into_iter().next().map(|(_, q)| q);
    loop {
        let query = pending.take().unwrap_or_default();
        if query.trim().is_empty() {
            return Err("ollama returned an empty reply".into());
        }
        let tool_result = run_websearch(okey, &query).await;
        let tool_result = if tool_result.trim().is_empty() {
            "No web results found. Answer briefly from your own knowledge and never mention searching or the web.".to_string()
        } else {
            tool_result
        };
        messages.push(serde_json::json!({"role": "tool", "content": tool_result}));
        let second = chat_once(&url, model, &messages, false).await?;
        let mut next: Option<String> = None;
        if let Some(list) = second.tool_calls.clone() {
            for c in list {
                if c.function.name.eq_ignore_ascii_case("websearch") {
                    let q = tool_query(&c.function.arguments);
                    if !q.trim().is_empty() {
                        next = Some(q);
                        break;
                    }
                }
            }
            if next.is_some() {
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": second.content.clone(),
                    "tool_calls": second.tool_calls.clone().unwrap_or_default().iter().map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null)).collect::<Vec<_>>(),
                }));
            }
        }
        if next.is_none() {
            if let Some((_, q)) = schema_tool_call(&second.content) {
                next = Some(q);
                messages.push(serde_json::json!({"role": "assistant", "content": second.content.clone()}));
            }
        }
        rounds += 1;
        if let Some(q) = next {
            if rounds >= 2 {
                return finalize_reply(channel, tagged, &names, &second.content);
            }
            pending = Some(q);
            continue;
        }
        return finalize_reply(channel, tagged, &names, &second.content);
    }
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

pub(crate) fn mentions_name(content: &str) -> bool {
    content
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("artixy"))
}

pub(crate) fn strip_name(content: &str) -> String {
    content
        .split_whitespace()
        .filter(|w| {
            let t = w.trim_matches(|c: char| !c.is_alphanumeric());
            !t.eq_ignore_ascii_case("artixy")
        })
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
    fn mention_stripped_to_prompt() {
        assert_eq!(strip_mention("<@123> hello", 123), "hello");
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
    fn name_trigger_matches_just_artixy() {
        assert!(mentions_name("artixy hello"));
        assert!(mentions_name("hey ARTIXY help"));
        assert!(!mentions_name("hello there"));
        assert!(!mentions_name("artixyz"));
        assert_eq!(strip_name("artixy what is latest gpu"), "what is latest gpu");
        assert_eq!(strip_name("ARTIXY"), "");
    }

    #[test]
    fn tool_query_parses_object_and_string() {
        let obj = serde_json::json!({"query": "latest nvidia gpu"});
        assert_eq!(tool_query(&obj), "latest nvidia gpu");
        let s = serde_json::Value::String("rtx 5090 price".to_string());
        assert_eq!(tool_query(&s), "rtx 5090 price");
        let empty = serde_json::json!({});
        assert_eq!(tool_query(&empty), "");
    }

    #[test]
    fn schema_output_detects_websearch() {
        let t = r#"{"content": "checking", "tool": {"name": "websearch", "query": "latest nvidia gpu"}}"#;
        let (content, q) = schema_tool_call(t).expect("parses");
        assert_eq!(content, "checking");
        assert_eq!(q, "latest nvidia gpu");
        assert!(schema_tool_call("just a normal reply").is_none());
        assert!(schema_tool_call(r#"{"content": "x", "tool": {"name": "other", "query": "y"}}"#).is_none());
    }

    #[test]
    fn schema_detects_openai_shape_and_fences() {
        let t = "{\"name\": \"websearch\", \"parameters\": {\"query\": \"latest nvidia gpu\"}}";
        let (_, q) = schema_tool_call(t).expect("openai shape parses");
        assert_eq!(q, "latest nvidia gpu");
        let fenced = "```json\n{\"content\": \"checking\", \"tool\": {\"name\": \"websearch\", \"query\": \"rtx 5090\"}}\n```";
        let (_, q2) = schema_tool_call(fenced).expect("fenced parses");
        assert_eq!(q2, "rtx 5090");
        let mixed = "thinking...\n{\"content\": \"\", \"tool\": {\"name\": \"websearch\", \"query\": \"latest gpu\"}}\ndone";
        assert!(schema_tool_call(mixed).is_some());
    }

    #[test]
    fn speaker_prefix_stripped_for_known_names() {
        let names = vec!["Matko802".to_string(), "Bob".to_string()];
        assert_eq!(strip_leading_speaker("[Matko802]: oh, just chillin!", &names), "oh, just chillin!");
        assert_eq!(strip_leading_speaker("Matko802: oh, just chillin!", &names), "oh, just chillin!");
        assert_eq!(strip_leading_speaker("[Bob]: [Matko802]: hi", &names), "hi");
        assert_eq!(strip_leading_speaker("hehe wdym is slang", &names), "hehe wdym is slang");
        assert_eq!(strip_leading_speaker("Note: remember this", &names), "Note: remember this");
        assert_eq!(strip_leading_speaker("[1234]: hi", &[] as &[String]), "hi");
    }

    #[test]
    fn meta_preamble_stripped_only_at_start() {
        assert_eq!(
            strip_meta_preamble("I think I got a bit carried away with the results. Let me summarize: Rust is great."),
            "Rust is great."
        );
        assert_eq!(
            strip_meta_preamble("Too much info! Just to sum it up: Python is great."),
            "Python is great."
        );
        assert_eq!(
            strip_meta_preamble("In summary, cats rule."),
            "cats rule."
        );
        assert_eq!(strip_meta_preamble("Rust is great. In summary, use it."), "Rust is great. In summary, use it.");
        assert_eq!(strip_meta_preamble("hehe wdym is slang"), "hehe wdym is slang");
        assert_eq!(strip_meta_preamble("Let me summarize:"), "Let me summarize:");
    }

    #[test]
    fn simplify_preamble_stripped() {
        assert_eq!(
            strip_meta_preamble("Let's simplify: the latest NVIDIA GPU is fast."),
            "the latest NVIDIA GPU is fast."
        );
        assert_eq!(
            strip_meta_preamble("I think I got a bit carried away again. Let's simplify: RTX PRO 5500."),
            "RTX PRO 5500."
        );
    }

    #[test]
    fn explicit_intent_matches_search_requests_only() {
        assert!(explicit_search_asked("search latest nvidia gpu"));
        assert!(explicit_search_asked("Search the web for rtx 5090"));
        assert!(explicit_search_asked("hey, look it up please"));
        assert!(explicit_search_asked("google it for me"));
        assert!(!explicit_search_asked("what is the latest nvidia gpu"));
        assert!(!explicit_search_asked("how are you"));
        assert!(!explicit_search_asked(""));
    }

    #[test]
    fn stale_history_lines_detected() {
        assert!(stale_history_line("I couldn't reach the web, sorry."));
        assert!(stale_history_line("Web search returned no results."));
        assert!(!stale_history_line("The RTX 5090 is the latest gpu."));
    }

    #[test]
    fn cookie_host_strips_scheme_and_path() {
        assert_eq!(cookie_host("https://example.com/chat/v1/status"), "example.com");
        assert_eq!(cookie_host("http://example.com/a/b"), "example.com");
    }

    #[test]
    fn hosted_search_response_parses_results() {
        let body = r#"{"results": [{"title": "GeForce RTX 50 series", "url": "https://en.wikipedia.org/wiki/GeForce_RTX_50_series", "content": "RTX 5090 in January 2025"}]}"#;
        let data: HostedSearchResponse = serde_json::from_str(body).expect("parses");
        assert_eq!(data.results.len(), 1);
        assert!(data.results[0].content.contains("5090"));
    }

    #[test]
    fn ollama_key_prefers_env_then_config() {
        std::env::remove_var("OLLAMA_API_KEY");
        assert_eq!(resolve_ollama_key(""), "");
        assert_eq!(resolve_ollama_key("  cfgkey  "), "cfgkey");
        std::env::set_var("OLLAMA_API_KEY", "  envkey  ");
        assert_eq!(resolve_ollama_key("cfgkey"), "envkey");
        std::env::remove_var("OLLAMA_API_KEY");
    }

    #[test]
    fn clean_reply_drops_tool_json_lines() {
        let raw = "searched for latest nvidia gpu\n{\"name\": \"websearch\", \"parameters\": {\"query\": \"latest nvidia gpu\"}}";
        let out = clean_reply(raw);
        assert!(!out.contains("websearch"), "got {out:?}");
        assert!(out.contains("searched for"), "got {out:?}");
        assert_eq!(clean_reply("{\"content\": \"\", \"tool\": {\"name\": \"websearch\", \"query\": \"x\"}}"), "");
        assert_eq!(clean_reply("hehe wdym is slang"), "hehe wdym is slang");
    }

    #[test]
    fn placeholder_detection_flags_narration() {
        assert!(looks_like_search_placeholder("searched for latest nvidia gpu"));
        assert!(looks_like_search_placeholder("searching the web now"));
        assert!(!looks_like_search_placeholder("The RTX 5090 launched in early 2025 with GDDR7."));
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

