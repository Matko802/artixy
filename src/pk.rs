use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct PkSender {
    pub id: u64,
    pub name: String,
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("pk http client builds")
    })
}

fn cache() -> &'static Mutex<HashMap<u64, Option<PkSender>>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Option<PkSender>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) async fn resolve(message_id: u64) -> Option<PkSender> {
    if let Some(hit) = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&message_id)
        .cloned()
    {
        return hit;
    }
    let found = fetch(message_id).await;
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.len() > 500 {
        c.clear();
    }
    c.insert(message_id, found.clone());
    found
}

fn parse_sender(v: &serde_json::Value) -> Option<u64> {
    let s = v.get("sender")?;
    if let Some(n) = s.as_u64() {
        return if n != 0 { Some(n) } else { None };
    }
    if let Some(n) = s.as_i64() {
        return if n > 0 { Some(n as u64) } else { None };
    }
    // Docs encode snowflakes as strings for precision; accept those too.
    let raw = s.as_str()?.trim();
    let id: u64 = raw.parse().ok()?;
    if id == 0 {
        return None;
    }
    Some(id)
}

async fn fetch(message_id: u64) -> Option<PkSender> {
    let v: serde_json::Value = client()
        .get(format!(
            "https://api.pluralkit.me/v2/messages/{message_id}"
        ))
        .header("User-Agent", "artixy (discord bot)")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let id = parse_sender(&v)?;
    // member is null when the member was deleted; still return the sender id
    // so auth keeps working (display falls back to system name / generic).
    let name = match v.get("member") {
        Some(m) if m.is_object() => m
            .get("display_name")
            .and_then(|n| n.as_str())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| m.get("name").and_then(|n| n.as_str()))
            .unwrap_or("")
            .trim()
            .to_string(),
        _ => String::new(),
    };
    let name = if name.is_empty() {
        v.get("system")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "plural".to_string())
    } else {
        name
    };
    Some(PkSender { id, name })
}
