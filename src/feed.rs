use crate::Error;

pub(crate) struct FeedLine {
    pub(crate) author: String,
    pub(crate) bot: bool,
    pub(crate) channel: u64,
    pub(crate) text: String,
    pub(crate) time: String,
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

pub(crate) async fn log_message(author: &str, bot: bool, channel: u64, content: &str) -> Result<(), Error> {
    let line = serde_json::json!({
        "author": flat(author, 64),
        "bot": bot,
        "channel": channel,
        "text": flat(content, 300),
    })
    .to_string();
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

fn parse_line(line: &str) -> Option<FeedLine> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(FeedLine {
        author: v.get("author")?.as_str()?.to_string(),
        bot: v.get("bot")?.as_bool().unwrap_or(false),
        channel: v.get("channel")?.as_u64()?,
        text: v.get("text")?.as_str().unwrap_or("").to_string(),
        time: String::new(),
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
