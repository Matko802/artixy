use crate::Error;

pub(crate) struct FeedLine {
    pub(crate) kind: String,
    pub(crate) author: String,
    pub(crate) bot: bool,
    pub(crate) channel: u64,
    pub(crate) guild: u64,
    pub(crate) channel_name: String,
    pub(crate) text: String,
    pub(crate) time: String,
    pub(crate) mid: u64,
}

fn feed_path() -> std::path::PathBuf {
    std::env::temp_dir().join("artixy-feed.log")
}

fn flat(s: &str, max: usize) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

async fn append(line: String) -> Result<(), Error> {
    let path = feed_path();
    if let Ok(m) = tokio::fs::metadata(&path).await {
        if m.len() > 1_000_000 {
            let _ = tokio::fs::write(&path, "").await;
        }
    }
    let mut opts = tokio::fs::OpenOptions::new();
    let mut f = opts.create(true).append(true).open(&path).await?;
    use tokio::io::AsyncWriteExt;
    f.write_all(line.as_bytes()).await?;
    f.write_all(b"\n").await?;
    Ok(())
}

pub(crate) async fn log_message(
    author: &str,
    bot: bool,
    channel: u64,
    guild: u64,
    channel_name: &str,
    content: &str,
    mid: u64,
) -> Result<(), Error> {
    append(
        serde_json::json!({
            "kind": "msg",
            "author": flat(author, 64),
            "bot": bot,
            "channel": channel,
            "guild": guild,
            "channel_name": flat(channel_name, 64),
            "text": flat(content, 300),
            "mid": mid,
        })
        .to_string(),
    )
    .await
}

pub(crate) async fn log_typing(author: &str, channel: u64, guild: u64, channel_name: &str) -> Result<(), Error> {
    append(
        serde_json::json!({
            "kind": "typing",
            "author": flat(author, 64),
            "bot": false,
            "channel": channel,
            "guild": guild,
            "channel_name": flat(channel_name, 64),
            "text": "",
        })
        .to_string(),
    )
    .await
}

fn parse_line(line: &str) -> Option<FeedLine> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(FeedLine {
        kind: v
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("msg")
            .to_string(),
        author: v.get("author")?.as_str()?.to_string(),
        bot: v.get("bot")?.as_bool().unwrap_or(false),
        channel: v.get("channel")?.as_u64()?,
        guild: v.get("guild").and_then(|g| g.as_u64()).unwrap_or(0),
        channel_name: v
            .get("channel_name")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string(),
        text: v.get("text")?.as_str().unwrap_or("").to_string(),
        time: String::new(),
        mid: v.get("mid").and_then(|m| m.as_u64()).unwrap_or(0),
    })
}

pub(crate) fn read_new(offset: &mut u64) -> Vec<FeedLine> {
    let data = match std::fs::read(feed_path()) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    if (data.len() as u64) < *offset {
        *offset = 0;
    }
    let start = (*offset as usize).min(data.len());
    let text = String::from_utf8_lossy(&data[start..]).into_owned();
    *offset = data.len() as u64;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(mut e) = parse_line(line) {
            e.time = super::tui::stamp();
            out.push(e);
        }
    }
    out
}
