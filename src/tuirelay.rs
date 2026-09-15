// TUI -> bot command relay.
//
// The TUI and the bot run on the same host (they already share the feed
// file). Discord slash commands can't be triggered by the bot itself, so
// running a real bot command from the TUI works like this:
//
// 1. TUI posts `/cmd …` as artixy with a DIRECT bot message and records the
//    returned message id here (claim file).
// 2. The bot's poise prefix dispatcher (`/` prefix + self-message execution)
//    parses its own message; the global command check + auth gates below
//    only let it through when the exact message id was claimed by the TUI.
//
// Claims are exact message ids with a short TTL — sayas reposts, AI replies
// and other bots' messages can never match, so nothing executes by accident.

use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 64 * 1024;
const CLAIM_TTL_SECS: u64 = 180;

pub(crate) fn relay_path() -> std::path::PathBuf {
    std::env::temp_dir().join("artixy-tui-relay.log")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Names of the real Discord bot commands (mirror of main.rs).
/// Used by the TUI to decide what to forward vs. handle locally.
pub(crate) fn bot_command_names() -> &'static [&'static str] {
    &[
        "help", "ps", "status", "start", "stop", "restart", "info", "user", "admin", "shell",
        "botrestart", "run", "sayas", "send", "notify", "purge_replies", "warmode", "upload",
        "ai", "websearch",
    ]
}

/// Record a TUI-posted message id so the bot may execute it.
/// Only call AFTER the Discord post succeeded.
pub(crate) fn claim_message(msg_id: u64) {
    if msg_id == 0 {
        return;
    }
    let path = relay_path();
    if let Ok(m) = std::fs::metadata(&path) {
        if m.len() > MAX_BYTES {
            let _ = std::fs::write(&path, "");
        }
    }
    let line = serde_json::json!({"id": msg_id, "at": now_secs()}).to_string() + "\n";
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// True when this exact message id was claimed by the TUI recently.
/// Read-only: safe to call from the framework hot path for bot messages.
pub(crate) fn is_claimed(msg_id: u64) -> bool {
    if msg_id == 0 {
        return false;
    }
    let data = match std::fs::read(relay_path()) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let text = String::from_utf8_lossy(&data);
    let now = now_secs();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("id").and_then(|i| i.as_u64()) != Some(msg_id) {
            continue;
        }
        let at = v.get("at").and_then(|a| a.as_u64()).unwrap_or(0);
        if now.saturating_sub(at) <= CLAIM_TTL_SECS {
            return true;
        }
    }
    false
}
