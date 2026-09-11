use poise::serenity_prelude as serenity;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone)]
struct PrefixSettings {
    dollar: Option<String>,
    semicolon: Option<String>,
}

struct Data {
    allowed: tokio::sync::RwLock<Allowed>,
    vm: String,
    live: LiveMap,
    settings: tokio::sync::RwLock<PrefixSettings>,
    shells: tokio::sync::RwLock<std::collections::HashMap<String, String>>,
}
struct LiveEntry {
    handle: tokio::task::AbortHandle,
    tag: u64,
    out_f: String,
    code_f: String,
}
type LiveMap = std::sync::Arc<
    tokio::sync::Mutex<std::collections::HashMap<serenity::ChannelId, LiveEntry>>,
>;

/// Max wall-clock time for a `$live` session before we stop polling.
const LIVE_TIMEOUT_SECS: u64 = 600;
const LIVE_POLL_SECS: u64 = 2;

/// Only allow safe unprivileged linux account names. Rejects `root` and
/// anything outside `[a-z_][a-z0-9_-]{0,31}` so a poisoned `users.json`
/// can't turn `su <name>` into `su root`.
fn valid_runas(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 || name == "root" {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Cryptographically random 32-char hex password for new Linux accounts.
/// 128 bits from /dev/urandom; falls back to time+pid when unavailable.
fn random_password() -> String {
    if let Ok(bytes) = std::fs::read("/dev/urandom") {
        if bytes.len() >= 16 {
            let mut s = String::with_capacity(32);
            for b in bytes.iter().take(16) {
                s.push_str(&format!("{:02x}", b));
            }
            return s;
        }
    }
    random_suffix().repeat(2)
}

/// Unpredictable hex suffix so guest `/tmp` paths can't be pre-created /
/// symlinked by another guest user. Reads /dev/urandom, falls back to
/// time+pid when unavailable (e.g. tests).
fn random_suffix() -> String {
    if let Ok(bytes) = std::fs::read("/dev/urandom") {
        if bytes.len() >= 8 {
            let mut s = String::with_capacity(16);
            for b in bytes.iter().take(8) {
                s.push_str(&format!("{:02x}", b));
            }
            return s;
        }
    }
    // Fallback: not cryptographically strong, but still per-run unique.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{}", nanos, std::process::id())
}

/// Abort any live task in `channel`, returning its entry so the caller can
/// clean up guest /tmp files. Never panics on missing entry.
async fn abort_live_for_channel(
    live_map: &LiveMap,
    channel: serenity::ChannelId,
) -> Option<LiveEntry> {
    let old = live_map.lock().await.remove(&channel);
    if let Some(ref e) = old {
        e.handle.abort();
    }
    old
}

/// Only remove the live entry if `tag` still matches. Prevents a naturally
/// finishing old task from deleting a newer session in the same channel.
async fn remove_live_if_tag(live_map: &LiveMap, channel: serenity::ChannelId, tag: u64) {
    let mut m = live_map.lock().await;
    if m.get(&channel).map(|e| e.tag) == Some(tag) {
        m.remove(&channel);
    }
}

async fn cleanup_live_files(vm: &str, out_f: &str, code_f: &str) {
    let _ = guest_exec(vm, "/bin/rm", &["-f", out_f, code_f], false, 10).await;
}
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

#[derive(Serialize, Deserialize)]
struct AllowedFile {
    users: Vec<u64>,
    #[serde(default)]
    linux: std::collections::HashMap<String, String>,
}

struct Allowed {
    owner: u64,
    users: Vec<u64>,
    linux: std::collections::HashMap<String, String>,
    path: PathBuf,
}

impl Allowed {
    async fn save(&self) -> Result<(), Error> {
        let data = serde_json::to_string_pretty(&AllowedFile {
            users: self.users.clone(),
            linux: self.linux.clone(),
        })?;
        save_json(
            self.path.to_str().unwrap_or("users.json"),
            data,
        )
        .await
    }
}

async fn is_authed(ctx: Context<'_>) -> bool {
    let id = ctx.author().id.get();
    let a = ctx.data().allowed.read().await;
    id == a.owner || a.users.contains(&id)
}

async fn is_owner(ctx: Context<'_>) -> bool {
    ctx.author().id.get() == ctx.data().allowed.read().await.owner
}

async fn need_auth(ctx: Context<'_>) -> Result<bool, Error> {
    if is_authed(ctx).await {
        return Ok(true);
    }
    let u = ctx.author();
    eprintln!("denied: {} (id {})", u.name, u.id.get());
    ctx.say("Not authorized. Ask the owner to run `/useradd <your discord id>`.")
        .await?;
    Ok(false)
}

fn load_shells() -> std::collections::HashMap<String, String> {
    std::fs::read_to_string("shells.json")
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default()
}

async fn maybe_defer(ctx: Context<'_>) {
    if matches!(ctx, poise::Context::Application(_)) {
        let _ = ctx.defer().await;
    }
}

async fn virsh(args: &[&str]) -> Result<String, Error> {
    let out = tokio::process::Command::new("virsh")
        .args(["--connect", "qemu:///system"])
        .args(args)
        .output()
        .await?;
    if !out.status.success() {
        return Err(format!(
            "virsh {} failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn agent_ping(vm: &str) -> bool {
    let out = tokio::process::Command::new("virsh")
        .args([
            "--connect",
            "qemu:///system",
            "qemu-agent-command",
            vm,
            "{\"execute\":\"guest-ping\"}",
        ])
        .output()
        .await;
    matches!(out, Ok(o) if o.status.success())
}

async fn wait_agent(vm: &str, secs: u64) -> bool {
    for _ in 0..secs.max(1) {
        if agent_ping(vm).await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    agent_ping(vm).await
}

async fn guest_status(vm: &str, pid: i64) -> Result<Option<i64>, Error> {
    let st = tokio::process::Command::new("virsh")
        .args([
            "--connect",
            "qemu:///system",
            "qemu-agent-command",
            vm,
            &serde_json::json!({"execute":"guest-exec-status","arguments":{"pid":pid}})
                .to_string(),
        ])
        .output()
        .await?;
    if !st.status.success() {
        return Ok(None);
    }
    if st.stdout.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(None);
    }
    let s: serde_json::Value = serde_json::from_slice(&st.stdout)
        .map_err(|e| format!("status poll: {}", e))?;
    if s["return"]["exited"].as_bool().unwrap_or(false) {
        Ok(Some(s["return"]["exitcode"].as_i64().unwrap_or(-1)))
    } else {
        Ok(None)
    }
}

async fn guest_exec(
    vm: &str,
    path: &str,
    args: &[&str],
    capture: bool,
    timeout_s: u64,
) -> Result<(i64, String, String), Error> {
    use base64::Engine as _;
    let pid = guest_launch_raw(vm, path, args, capture).await?;
    for _ in 0..timeout_s.max(1) {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let st = tokio::process::Command::new("virsh")
            .args([
                "--connect",
                "qemu:///system",
                "qemu-agent-command",
                vm,
                &serde_json::json!({"execute":"guest-exec-status","arguments":{"pid":pid}})
                    .to_string(),
            ])
            .output()
            .await?;
        if !st.status.success() {
            continue;
        }
        if st.stdout.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let s: serde_json::Value = serde_json::from_slice(&st.stdout)
            .map_err(|e| format!("status: {}", e))?;
        if !s["return"]["exited"].as_bool().unwrap_or(false) {
            continue;
        }
        let code = s["return"]["exitcode"].as_i64().unwrap_or(-1);
        if !capture {
            return Ok((code, String::new(), String::new()));
        }
        let dec = |v: &serde_json::Value| {
            v.as_str()
                .and_then(|b| {
                    base64::engine::general_purpose::STANDARD.decode(b).ok()
                })
                .map(|b| String::from_utf8_lossy(&b).to_string())
                .unwrap_or_default()
        };
        return Ok((
            code,
            dec(&s["return"]["out-data"]),
            dec(&s["return"]["err-data"]),
        ));
    }
    Err("guest-exec timed out waiting for exit (it may still be running in the guest)".into())
}

async fn guest_launch_raw(vm: &str, path: &str, args: &[&str], capture: bool) -> Result<i64, Error> {
    for _ in 0..3 {
        let out = tokio::process::Command::new("virsh")
            .args([
                "--connect",
                "qemu:///system",
                "qemu-agent-command",
                vm,
                &serde_json::json!({
                    "execute": "guest-exec",
                    "arguments": { "path": path, "arg": args, "capture-output": capture }
                })
                .to_string(),
            ])
            .output()
            .await?;
        if !out.status.success() {
            return Err(format!(
                "guest-exec launch failed:\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        if out.stdout.iter().all(|b| b.is_ascii_whitespace()) {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("launch: {}", e))?;
        return v["return"]["pid"]
            .as_i64()
            .ok_or_else(|| "guest-exec: no pid returned".into());
    }
    Err("launch: agent returned empty response 3x".into())
}

fn sanitize_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // Background colors don't exist in Discord: turn `BG + spaces`
    // swatches into foreground-colored blocks instead.
    let mut swatch: Option<i32> = None;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\r' {
            continue;
        }
        if c != '\x1b' {
            if c == ' ' {
                if let Some(fg) = swatch {
                    let mut n = 1;
                    while it.peek() == Some(&' ') {
                        it.next();
                        n += 1;
                    }
                    if n >= 2 {
                        out.push_str(&format!("\x1b[0;{}m", fg));
                        for _ in 0..n {
                            out.push('█');
                        }
                        out.push_str("\x1b[0m");
                    } else {
                        out.push(' ');
                    }
                    continue;
                }
            } else {
                swatch = None;
            }
            out.push(c);
            continue;
        }
        swatch = None;
        match it.peek() {
            Some('[') => {
                it.next();
                let mut params = String::new();
                let mut final_b = None;
                for ch in it.by_ref() {
                    if ('@'..='~').contains(&ch) {
                        final_b = Some(ch);
                        break;
                    }
                    params.push(ch);
                }
                if final_b == Some('m') {
                    let mut kept = vec![];
                    let mut bg = None;
                    for p in params.split(';') {
                        let n: i32 = p.parse().unwrap_or(-1);
                        match n {
                            0 | 1 | 4 | 22 | 24 | 39 | 49 => kept.push(n.to_string()),
                            30..=37 => kept.push(n.to_string()),
                            90..=97 => kept.push((n - 60).to_string()),
                            40..=47 => {
                                if bg.is_none() {
                                    bg = Some(n - 10);
                                }
                            }
                            100..=107 => {
                                if bg.is_none() {
                                    bg = Some(n - 70);
                                }
                            }
                            _ => {}
                        }
                    }
                    swatch = bg;
                    if !kept.is_empty() || params.is_empty() {
                        // Discord only renders the two-part form: normalize
                        // bare `40m` -> `0;40m` and bare `m` -> `0m`.
                        if kept.is_empty() {
                            out.push_str("\x1b[0m");
                        } else if kept.len() == 1 && kept[0] != "0" {
                            out.push_str("\x1b[0;");
                            out.push_str(&kept[0]);
                            out.push('m');
                        } else {
                            out.push_str("\x1b[");
                            out.push_str(&kept.join(";"));
                            out.push('m');
                        }
                    }
                }
            }
            Some(']') => {
                it.next();
                let mut prev = '\0';
                for ch in it.by_ref() {
                    if ch == '\x07' || (ch == '\\' && prev == '\x1b') {
                        break;
                    }
                    prev = ch;
                }
            }
            Some(_) => {
                it.next();
            }
            None => {}
        }
    }
    out
}

fn fit_body(body: &str) -> String {
    const MAX: usize = 1790;
    if body.chars().count() <= MAX {
        return body.to_string();
    }
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.len() < 2 {
        return body.chars().take(MAX).collect();
    }
    let mut head_len = 0usize;
    let mut head_n = 0usize;
    for l in &lines {
        let n = l.chars().count() + 1;
        if head_len + n > 700 {
            break;
        }
        head_len += n;
        head_n += 1;
    }
    let mut tail_parts: Vec<String> = vec![];
    for l in lines.iter().rev() {
        if tail_parts.len() >= lines.len().saturating_sub(head_n).max(1) {
            break;
        }
        let n = l.chars().count() + 1;
        if tail_parts.iter().map(|s: &String| s.chars().count() + 1).sum::<usize>() + n > 900 {
            break;
        }
        tail_parts.push(l.to_string());
    }
    if tail_parts.is_empty() {
        if let Some(last) = lines.last() {
            tail_parts.push(last.chars().take(900).collect());
        }
    }
    tail_parts.reverse();
    let tail = tail_parts.join("\n");
    let omitted = body
        .chars()
        .count()
        .saturating_sub(head_len + tail.chars().count());
    let mut out = lines[..head_n].join("\n");
    out.push_str(&format!("\n…[{} chars omitted]…\n", omitted));
    out.push_str(&tail);
    out
}

fn codeblock(s: &str) -> String {
    let mut t = sanitize_ansi(s.trim_end());
    if t.len() > 1800 {
        t = t.chars().take(1790).collect();
        t.push_str("\n…truncated");
    }
    if t.is_empty() {
        t = "(empty)".into();
    }
    format!("```ansi\n{}\n```", t)
}

const HELP: &str = "\
**artixy — your Artix VM in your pocket.** Everything acts on the one hardcoded VM, no names needed. Only the owner + added users can use me.\n\
\n**VM**\n`$ps` — state of the VM\n`$status` — quick state + agent check\n`$start` — power on + wait for guest agent\n`$stop` — graceful shutdown\n`$restart` — reboot\n`$info` — details + agent status\n\
\n**Who can use me**\n`$users` / `$userlist` — show owner + managers\n`$useradd @user` (prefix only) — owner only: links them and creates their Linux account in Artix (name from discord name).\n`$userdel @user` (prefix only) — owner only\n`$shell [fish|bash]` — your `$` interpreter (default bash)\n`$run <command>` — same as `$`, prefix only\n\
\nPrefixes start OFF — slash commands always work. Owner turns them on in `/settings` (e.g. `/settings dollar $`).\n\
\n**Run real commands in Artix**\n`$<command>` — runs it for real inside the VM through the guest agent and prints the output. e.g. `$sudo pacman -Syu`, `$ls -la`. Runs as YOUR linked linux account (`whoami` proves it).\n`$live <command>` — follows one run live in a single message until it finishes. Starting another run stops it.\n`$shot` — screenshot of the host screen, uploaded here\n`$send <path>` — upload a host file here (absolute path, ~20MB max)\n\
\n**Warning:** managers can power this machine on/off. Keep the token secret: it lives only in `.env`, never in git.";

#[poise::command(slash_command, prefix_command)]
async fn help(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say(HELP).await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn ps(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    match virsh(&["list", "--all"]).await {
        Ok(o) => {
            ctx.say(codeblock(&o)).await?;
        }
        Err(e) => {
            ctx.say(codeblock(&e.to_string())).await?;
        }
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn start(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    let vm = ctx.data().vm.clone();
    let state = virsh(&["domstate", &vm]).await.unwrap_or_default();
    let mut started_here = false;
    if state.trim() == "running" {
        ctx.say(format!(
            "`{}` is already on. Waiting for the guest agent…",
            vm
        ))
        .await?;
    } else {
        match virsh(&["start", &vm]).await {
            Ok(_) => {
                started_here = true;
                ctx.say(format!(
                    "`{}` starting. Waiting for the guest agent…",
                    vm
                ))
                .await?;
            }
            Err(e) => {
                ctx.say(codeblock(&e.to_string())).await?;
                return Ok(());
            }
        }
    }
    if wait_agent(&vm, 90).await {
        if started_here {
            ctx.say(format!("{} has started.", vm)).await?;
        } else {
            ctx.say(format!(
                "`{}` is on and the guest agent answers.",
                vm
            ))
            .await?;
        }
    } else {
        ctx.say(format!("`{}` is on but the guest agent is silent. Inside Artix run `sudo pacman -S qemu-guest-agent` and enable its service, then `$start` again.", vm)).await?;
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    let vm = ctx.data().vm.clone();
    ctx.say(format!("stopping {}…", vm)).await?;
    match virsh(&["shutdown", &vm]).await {
        Ok(_) => {}
        Err(e) => {
            ctx.say(codeblock(&e.to_string())).await?;
            return Ok(());
        }
    }
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if virsh(&["domstate", &vm])
            .await
            .unwrap_or_default()
            .trim()
            == "shut off"
        {
            ctx.say(format!("{} has stopped.", vm)).await?;
            return Ok(());
        }
    }
    ctx.say(format!("{} is still stopping — check `;status`.", vm))
        .await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn restart(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let vm = ctx.data().vm.clone();
    match virsh(&["reboot", &vm]).await {
        Ok(_) => {
            ctx.say(format!("`{}` rebooting.", vm)).await?;
        }
        Err(e) => {
            ctx.say(codeblock(&e.to_string())).await?;
        }
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn info(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let vm = ctx.data().vm.clone();
    match virsh(&["dominfo", &vm]).await {
        Ok(o) => {
            let agent = if agent_ping(&vm).await {
                "guest agent: up"
            } else {
                "guest agent: DOWN (install qemu-guest-agent in Artix)"
            };
            ctx.say(codeblock(&format!("{}\n{}", o, agent))).await?;
        }
        Err(e) => {
            ctx.say(codeblock(&e.to_string())).await?;
        }
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn shell(
    ctx: Context<'_>,
    #[description = "fish or bash (empty shows current)"] name: Option<String>,
) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let key = ctx.author().id.get().to_string();
    match name {
        None => {
            let cur = ctx.data().shells.read().await;
            ctx.say(format!(
                "Your shell: `{}`. Change with `;shell fish` or `;shell bash`.",
                cur.get(&key).map(|s| s.as_str()).unwrap_or("bash")
            ))
            .await?;
        }
        Some(n) => {
            let n = n.trim().to_lowercase();
            if n != "fish" && n != "bash" {
                ctx.say("Only `fish` or `bash`.").await?;
                return Ok(());
            }
            {
                let mut m = ctx.data().shells.write().await;
                m.insert(key, n.clone());
                let data = serde_json::to_string_pretty(&*m)?;
                save_json("shells.json", data).await?;
            }
            ctx.say(format!("Your shell is now `{}`.", n)).await?;
        }
    }
    Ok(())
}

/// True when the running binary lives under `/nix/store`, i.e. it was
/// deployed via Nix rather than built locally with `cargo`. In that case the
/// self-build/self-restart commands are meaningless: the store is immutable
/// and read-only, so a rebuild would go nowhere. They no-op instead of
/// failing and confusing everyone.
fn deployed_via_nix() -> bool {
    std::env::current_exe()
        .map(|p| p.starts_with("/nix/store/"))
        .unwrap_or(false)
}

fn project_dir() -> PathBuf {
    let mut dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    for _ in 0..5 {
        match dir {
            Some(ref p) if p.join("Cargo.toml").exists() => return p.clone(),
            Some(p) => dir = p.parent().map(|p| p.to_path_buf()),
            None => break,
        }
    }
    PathBuf::from(".")
}

async fn save_json(path: &str, data: String) -> Result<(), Error> {
    // Unique tmp so concurrent saves of different files can't clobber each
    // other; 0600 so tokens / ids aren't world-readable via the tmp window.
    let tmp = format!("{}.{}.tmp", path, random_suffix());
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    let mut f = opts.open(&tmp).await?;
    use tokio::io::AsyncWriteExt;
    f.write_all(data.as_bytes()).await?;
    f.sync_all().await?;
    drop(f);
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn build(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    if deployed_via_nix() {
        ctx.say(format!(
            "Deployed from Nix — the binary lives read-only in `/nix/store` and `cargo build` here can't replace it. Rebuild with `nixos-rebuild` instead."
        ))
        .await?;
        return Ok(());
    }
    maybe_defer(ctx).await;
    ctx.say("Building…").await?;
    let out = tokio::process::Command::new("cargo")
        .arg("build")
        .current_dir(project_dir())
        .output()
        .await;
    match out {
        Ok(o) => {
            let mut t = String::from_utf8_lossy(&o.stdout).to_string();
            t.push_str(&String::from_utf8_lossy(&o.stderr));
            let tail: String = t.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
            let ok = o.status.success();
            ctx.say(codeblock(&format!(
                "build {}:\n{}",
                if ok { "OK" } else { "FAILED" },
                tail.trim()
            )))
            .await?;
        }
        Err(e) => {
            ctx.say(codeblock(&format!("could not run cargo: {}", e))).await?;
        }
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn botrestart(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    if deployed_via_nix() {
        ctx.say(
            "Deployed from Nix — I can't re-exec myself out of a read-only \
             `/nix/store`. Restart with `systemctl --user restart artixy`.",
        )
        .await?;
        return Ok(());
    }
    ctx.say("Restarting…").await?;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("target/debug/artixy"));
    // Don't follow a pre-created symlink at the fixed log path (appending
    // as the bot user could otherwise clobber an arbitrary file).
    let log_path = "/tmp/artixy.log";
    if std::fs::symlink_metadata(log_path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(log_path);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .ok();
    let out = log
        .as_ref()
        .and_then(|f| f.try_clone().ok())
        .map(std::process::Stdio::from)
        .unwrap_or_else(|| std::process::Stdio::null());
    let err = log
        .as_ref()
        .and_then(|f| f.try_clone().ok())
        .map(std::process::Stdio::from)
        .unwrap_or_else(|| std::process::Stdio::null());
    let spawned = tokio::process::Command::new("setsid")
        .arg(&exe)
        .current_dir(project_dir())
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn();
    match spawned {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            ctx.say(codeblock(&format!("restart failed to spawn: {}", e)))
                .await?;
            Ok(())
        }
    }
}

#[poise::command(slash_command, prefix_command)]
async fn colortest(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    ctx.say("```ansi\n1 plain\n\u{1b}[0;31m2 red 0;31\u{1b}[0m\n\u{1b}[1;32m3 bold-green 1;32\u{1b}[0m\n\u{1b}[4;34m4 underline-blue 4;34\u{1b}[0m\n\u{1b}[0;37m5 white 0;37\u{1b}[0m\n\u{1b}[0;40m6 bg-black 0;40\u{1b}[0m\n\u{1b}[0;41m7 bg-red 0;41\u{1b}[0m\n```\nTell me which numbered lines show color.").await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn run(
    ctx: Context<'_>,
    #[description = "Command to run in the VM"] cmd: String,
    #[description = "Follow output live instead of one reply"] live: Option<bool>,
) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    if live.unwrap_or(false) {
        return do_live(ctx, cmd).await;
    }
    maybe_defer(ctx).await;
    let vm = ctx.data().vm.clone();
    let uid = ctx.author().id.get();
    if let Some(old) = abort_live_for_channel(&ctx.data().live, ctx.channel_id()).await {
        // Best-effort: aborted task can't clean up after itself.
        cleanup_live_files(&vm, &old.out_f, &old.code_f).await;
    }
    let sh = user_shell(ctx.data(), uid).await;
    let runas = linked_user(ctx.data(), uid).await;
    let (body, code) = run_guest_cmd(&vm, &sh, &cmd, runas.as_deref(), 300).await?;
    ctx.say(codeblock(&fit_body(&body))).await?;
    eprintln!("run for {}: exit {}", ctx.author().name, code);
    Ok(())
}

async fn do_live(ctx: Context<'_>, cmd: String) -> Result<(), Error> {
    maybe_defer(ctx).await;
    if cmd.trim().is_empty() {
        ctx.say("Usage: `$live <command>`.").await?;
        return Ok(());
    }
    let vm = ctx.data().vm.clone();
    if !agent_ping(&vm).await {
        ctx.say("Guest agent is silent. Install `qemu-guest-agent` in Artix first.")
            .await?;
        return Ok(());
    }
    let http = ctx.serenity_context().http.clone();
    let ack = ctx
        .say(format!("`live: {}` starting…", cmd.trim()))
        .await?
        .into_message()
        .await?;
    begin_live(
        http,
        ack,
        ctx.author().id.get(),
        &ctx.author().name,
        vm,
        cmd.trim().to_string(),
        linked_user(ctx.data(), ctx.author().id.get()).await,
        ctx.data().live.clone(),
    )
    .await;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn live(
    ctx: Context<'_>,
    #[description = "Command to follow live"] cmd: String,
) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    do_live(ctx, cmd).await
}

#[poise::command(slash_command, prefix_command)]
async fn shot(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    // Unique path so concurrent `$shot` calls can't overwrite / delete each
    // other's file via the fixed /tmp name.
    let path = format!("/tmp/artixy-shot-{}-{}.png", std::process::id(), random_suffix());
    let out = tokio::process::Command::new("grim")
        .arg(&path)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            match serenity::CreateAttachment::path(&path).await {
                Ok(att) => {
                    if ctx
                        .send(poise::CreateReply::default().attachment(att))
                        .await
                        .is_err()
                    {
                        eprintln!("shot send failed (missing Attach Files permission?)");
                        ctx.say("Screenshot captured but I can't attach files here — give me the Attach Files permission.").await?;
                    }
                }
                Err(e) => {
                    ctx.say(codeblock(&format!("attach failed: {}", e))).await?;
                }
            }
            let _ = tokio::fs::remove_file(&path).await;
        }
        _ => {
            ctx.say("grim failed (are you in a Wayland session?).").await?;
        }
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn send(
    ctx: Context<'_>,
    #[description = "Absolute path of a file inside the bot's project dir (max ~20MB)"] path: String,
) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    let p = path.trim();
    if !p.starts_with('/') {
        ctx.say("Absolute path only.").await?;
        return Ok(());
    }
    // Resolve symlinks on both sides so a link inside the project dir can't
    // point at a file somewhere else on the host.
    let root = match tokio::fs::canonicalize(project_dir()).await {
        Ok(r) => r,
        Err(e) => {
            ctx.say(codeblock(&format!("can't resolve project dir: {}", e)))
                .await?;
            return Ok(());
        }
    };
    let target = match tokio::fs::canonicalize(p).await {
        Ok(t) => t,
        Err(_) => {
            ctx.say("No readable file there (must exist, absolute path, under the project dir, ~20MB max).")
                .await?;
            return Ok(());
        }
    };
    if !target.starts_with(&root) {
        ctx.say("That path is outside the bot's project dir — not sending it.")
            .await?;
        return Ok(());
    }
    match tokio::fs::metadata(&target).await {
        Ok(m) if m.is_file() && m.len() < 20 * 1024 * 1024 => {
            match serenity::CreateAttachment::path(&target).await {
                Ok(att) => {
                    if ctx
                        .send(poise::CreateReply::default().attachment(att))
                        .await
                        .is_err()
                    {
                        eprintln!("send failed (missing Attach Files permission?)");
                        ctx.say("File read but I can't attach files here — give me the Attach Files permission.").await?;
                    }
                }
                Err(e) => {
                    ctx.say(codeblock(&format!("attach failed: {}", e))).await?;
                }
            }
        }
        _ => {
            ctx.say("No readable file there (absolute path under the project dir, ~20MB max).")
                .await?;
        }
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn status(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let vm = ctx.data().vm.clone();
    let state = virsh(&["domstate", &vm]).await.unwrap_or_else(|e| e.to_string());
    let agent = if state.trim() == "running" {
        if agent_ping(&vm).await {
            "agent: up"
        } else {
            "agent: DOWN"
        }
    } else {
        "agent: n/a (off)"
    };
    ctx.say(format!("`{}`: {} | {}", vm, state.trim(), agent))
        .await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn useradd(
    ctx: Context<'_>,
    #[description = "User to authorize"] user: serenity::User,
) -> Result<(), Error> {
    do_useradd(ctx, &user).await
}

#[poise::command(slash_command, prefix_command)]
async fn userdel(
    ctx: Context<'_>,
    #[description = "User to remove"] user: serenity::User,
) -> Result<(), Error> {
    do_userdel(ctx, &user).await
}

fn settings_text(s: &PrefixSettings) -> String {
    let show = |v: &Option<String>| match v {
        Some(p) => format!("ON `{}`", p),
        None => "OFF".into(),
    };
    format!(
        "Prefixes (slash commands always work):\n$ slot: {}\n; slot: {}\nChange (owner only): `/settings dollar <off|$|!|…>`, `/settings semicolon <off|;|…>` — 1-2 symbol chars, or `off`.",
        show(&s.dollar),
        show(&s.semicolon)
    )
}

async fn do_settings_set(ctx: Context<'_>, slot: &str, value: String) -> Result<(), Error> {
    if !is_owner(ctx).await {
        ctx.say("Owner only.").await?;
        return Ok(());
    }
    let v = value.trim();
    {
        let mut s = ctx.data().settings.write().await;
        if v.eq_ignore_ascii_case("off") {
            if slot == "dollar" {
                s.dollar = None;
            } else {
                s.semicolon = None;
            }
        } else {
            let n = v.chars().count();
            if !(1..=2).contains(&n) || !v.chars().all(|c| !c.is_alphanumeric() && !c.is_whitespace()) {
                ctx.say("Use `off` or 1-2 symbol chars (no letters, no spaces).")
                    .await?;
                return Ok(());
            }
            let other = if slot == "dollar" { &s.semicolon } else { &s.dollar };
            if other.as_deref() == Some(v) {
                ctx.say("The other slot already uses that.").await?;
                return Ok(());
            }
            if slot == "dollar" {
                s.dollar = Some(v.into());
            } else {
                s.semicolon = Some(v.into());
            }
        }
    }
    save_settings(ctx.data()).await?;
    let s = ctx.data().settings.read().await;
    ctx.say(settings_text(&s)).await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command, subcommands("dollar", "semicolon"))]
async fn settings(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let s = ctx.data().settings.read().await;
    ctx.say(settings_text(&s)).await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn dollar(
    ctx: Context<'_>,
    #[description = "off or 1-2 symbol chars"] value: String,
) -> Result<(), Error> {
    do_settings_set(ctx, "dollar", value).await
}

#[poise::command(slash_command, prefix_command)]
async fn semicolon(
    ctx: Context<'_>,
    #[description = "off or 1-2 symbol chars"] value: String,
) -> Result<(), Error> {
    do_settings_set(ctx, "semicolon", value).await
}

#[poise::command(slash_command, prefix_command)]
async fn users(ctx: Context<'_>) -> Result<(), Error> {
    do_users(ctx).await
}

#[poise::command(slash_command, prefix_command)]
async fn userlist(ctx: Context<'_>) -> Result<(), Error> {
    do_users(ctx).await
}

#[poise::command(slash_command, prefix_command, subcommands("add", "del"))]
async fn user(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("Usage: `;user add <discord id> [linuxname]` or `;user del <discord id>`.")
        .await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn add(
    ctx: Context<'_>,
    #[description = "User to authorize"] user: serenity::User,
) -> Result<(), Error> {
    do_useradd(ctx, &user).await
}

#[poise::command(slash_command, prefix_command)]
async fn del(
    ctx: Context<'_>,
    #[description = "User to remove"] user: serenity::User,
) -> Result<(), Error> {
    do_userdel(ctx, &user).await
}

async fn uname(http: &serenity::Http, uid: u64) -> String {
    serenity::UserId::new(uid)
        .to_user(http)
        .await
        .map(|u| u.name.clone())
        .unwrap_or_else(|_| uid.to_string())
}

async fn do_users(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let (owner, pairs): (u64, Vec<(u64, Option<String>)>) = {
        let a = ctx.data().allowed.read().await;
        let pairs = a
            .users
            .iter()
            .map(|u| (*u, a.linux.get(&u.to_string()).cloned()))
            .collect();
        (a.owner, pairs)
    };
    let mut msg = format!(
        "Owner: `{}`\nManagers (discord → linux):",
        uname(ctx.http(), owner).await
    );
    if pairs.is_empty() {
        msg.push_str(" none yet — owner runs `/useradd @user`");
    } else {
        for (u, n) in pairs {
            let name = uname(ctx.http(), u).await;
            match n {
                Some(n) => msg.push_str(&format!("\n`{}` → `{}`", name, n)),
                None => msg.push_str(&format!("\n`{}` → (no linux account)", name)),
            }
        }
    }
    ctx.say(msg).await?;
    Ok(())
}

fn sanitize_discord_name(s: &str) -> Option<String> {
    let mut out: String = s
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .take(32)
        .collect();
    while let Some(false) = out
        .chars()
        .next()
        .map(|c| c.is_ascii_lowercase() || c == '_')
    {
        out.remove(0);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

async fn do_useradd(ctx: Context<'_>, user: &serenity::User) -> Result<(), Error> {
    if !is_owner(ctx).await {
        ctx.say("Owner only.").await?;
        return Ok(());
    }
    maybe_defer(ctx).await;
    let uid = user.id.get();
    let name = sanitize_discord_name(&user.name).unwrap_or_else(|| format!("u{}", uid));
    {
        let mut a = ctx.data().allowed.write().await;
        if a.owner != uid && !a.users.contains(&uid) {
            a.users.push(uid);
            a.save().await?;
        }
    }
    let vm = ctx.data().vm.clone();
    let state = virsh(&["domstate", &vm]).await.unwrap_or_default();
    if state.trim() != "running" {
        match virsh(&["start", &vm]).await {
            Ok(_) => {
                ctx.say(format!("`{}` was off, starting it first…", vm)).await?;
            }
            Err(e) => {
                ctx.say(format!(
                    "Authorized `{}` in the bot, but the VM won't start:\n{}",
                    uid,
                    codeblock(&e.to_string())
                ))
                .await?;
                return Ok(());
            }
        }
    }
    if !wait_agent(&vm, 60).await {
            ctx.say(format!("Authorized `{}` in the bot, but the guest agent is silent — no Linux account created. Install `qemu-guest-agent` in Artix, then rerun `$useradd <@{}> {}`.", uid, uid, name)).await?;
        return Ok(());
    }
    let mut rc = guest_exec(&vm, "/usr/bin/useradd", &["-m", "-s", "/bin/bash", &name], false, 30).await;
    if let Err(e) = &rc {
        if e.to_string().contains("No such file") {
            rc = guest_exec(&vm, "/usr/sbin/useradd", &["-m", "-s", "/bin/bash", &name], false, 30).await;
        }
    }
    match rc {
        Ok((0, _, _)) => {}
        Ok((9, _, _)) => {
            ctx.say(format!("Linux user `{}` already exists, linking it.", name)).await?;
        }
        Ok((c, _, _)) => {
            ctx.say(format!(
                "Authorized `{}` in the bot, but `useradd` in the VM failed (code {}).",
                uid, c
            ))
            .await?;
            return Ok(());
        }
        Err(e) => {
            ctx.say(format!(
                "Authorized `{}` in the bot, but `useradd` in the VM failed:\n{}",
                uid,
                codeblock(&e.to_string())
            ))
            .await?;
            return Ok(());
        }
    }
    {
        let mut a = ctx.data().allowed.write().await;
        a.linux.insert(uid.to_string(), name.clone());
        a.save().await?;
    }
    let pw = random_password();
    let pw_ok = virsh(&["set-user-password", &vm, &name, &pw])
        .await
        .is_ok();
    if pw_ok {
        ctx.say(format!(
            "added user \"{}\" linked to `{}` — password `{}`, they log in as themselves everywhere. Tell them to run `passwd` to change it.",
            name, uid, pw
        ))
        .await?;
    } else {
        ctx.say(format!(
            "added user \"{}\" linked to `{}` — but setting their password failed.",
            name, uid
        ))
        .await?;
    }
    Ok(())
}

async fn do_userdel(ctx: Context<'_>, user: &serenity::User) -> Result<(), Error> {
    if !is_owner(ctx).await {
        ctx.say("Owner only.").await?;
        return Ok(());
    }
    let uid = user.id.get();
    let mut a = ctx.data().allowed.write().await;
    if let Some(i) = a.users.iter().position(|u| *u == uid) {
        a.users.remove(i);
        let linked = a.linux.remove(&uid.to_string());
        a.save().await?;
        match linked {
            Some(n) => ctx.say(format!("Removed <@{}> (was linked to linux `{}`; account left in place).", uid, n)).await?,
            None => ctx.say(format!("Removed <@{}>.", uid)).await?,
        };
    } else {
        ctx.say(format!("<@{}> was not a manager.", uid)).await?;
    }
    Ok(())
}

const COMMANDS: &[&str] = &[
    "help", "ps", "status", "start", "stop", "restart", "info", "users", "user",
    "add", "del", "useradd", "userdel", "userlist", "shell",
    "build", "botrestart", "colortest", "run", "live",
    "shot", "send",
    "settings", "dollar", "semicolon",
];
async fn begin_live(
    http: std::sync::Arc<serenity::Http>,
    ack: serenity::Message,
    author_id: u64,
    author_name: &str,
    vm: String,
    cmd: String,
    runas: Option<String>,
    live_map: LiveMap,
) {
    // Fail closed on a poisoned mapping instead of `su root`.
    if let Some(ref u) = runas {
        if !valid_runas(u) {
            let _ = ack
                .channel_id
                .say(&http, codeblock("linked linux account is invalid; ask the owner to re-add you."))
                .await;
            return;
        }
    }
    let channel = ack.channel_id;
    let tag = ack.id.get();
    // Unpredictable paths: a guest user can't pre-create / symlink these.
    let rand = random_suffix();
    let out_f = format!("/tmp/podbot-live-{}-{}.out", tag, rand);
    let code_f = format!("/tmp/podbot-live-{}-{}.code", tag, rand);
    if let Some(old) = abort_live_for_channel(&live_map, channel).await {
        let vm_clone = vm.clone();
        tokio::spawn(async move {
            cleanup_live_files(&vm_clone, &old.out_f, &old.code_f).await;
        });
    }
    let live_map2 = live_map.clone();
    let (vm2, cmd2, runas2, out_f2, code_f2) =
        (vm.clone(), cmd.clone(), runas.clone(), out_f.clone(), code_f.clone());
    let handle = tokio::spawn(async move {
        live_run(http, ack, channel, tag, vm2, cmd2, runas2, out_f2, code_f2, live_map2).await;
    })
    .abort_handle();
    live_map.lock().await.insert(
        channel,
        LiveEntry {
            handle,
            tag,
            out_f,
            code_f,
        },
    );
    eprintln!("live started for {} (id {})", author_name, author_id);
}

async fn live_run(
    http: std::sync::Arc<serenity::Http>,
    mut msg: serenity::Message,
    channel: serenity::ChannelId,
    tag: u64,
    vm: String,
    cmd: String,
    runas: Option<String>,
    out_f: String,
    code_f: String,
    live_map: LiveMap,
) {
    use base64::Engine as _;
    if let Some(ref u) = runas {
        if !valid_runas(u) {
            let _ = msg
                .edit(&http, serenity::EditMessage::new().content(codeblock(
                    "linked linux account is invalid; ask the owner to re-add you.",
                )))
                .await;
            remove_live_if_tag(&live_map, channel, tag).await;
            return;
        }
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(cmd.as_bytes());
    let script = format!(
        "echo {} | base64 -d | bash -c 'export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH; eval \"$(cat)\"' > {} 2>&1; echo $? > {}",
        b64, out_f, code_f
    );
    // Fallback script for `su ... -s /bin/sh` when bash is missing.
    let script_sh = format!(
        "echo {} | base64 -d | sh -c 'export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH; eval \"$(cat)\"' > {} 2>&1; echo $? > {}",
        b64, out_f, code_f
    );
    let (lpath, largs): (&str, Vec<&str>) = match &runas {
        Some(u) => ("su", vec![u.as_str(), "-s", "/bin/bash", "-c", &script]),
        None => ("/bin/bash", vec!["-c", &script]),
    };
    let launched = guest_launch_raw(&vm, lpath, &largs, false).await;
    let launched = match launched {
        // Fail closed: never drop `su <user>` and run as the agent user
        // (often root). Retry with the SAME privilege via /bin/sh.
        Err(e) if e.to_string().contains("No such file") => match &runas {
            Some(u) => {
                guest_launch_raw(&vm, "su", &[u.as_str(), "-s", "/bin/sh", "-c", &script_sh], false)
                    .await
            }
            None => guest_launch_raw(&vm, "/bin/sh", &["-c", &script_sh], false).await,
        },
        other => other,
    };
    let pid = match launched {
        Ok(p) => p,
        Err(e) => {
            let _ = msg
                .edit(&http, serenity::EditMessage::new().content(codeblock(&e.to_string())))
                .await;
            remove_live_if_tag(&live_map, channel, tag).await;
            return;
        }
    };
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(LIVE_POLL_SECS)).await;
        if started.elapsed().as_secs() > LIVE_TIMEOUT_SECS {
            let _ = msg
                .edit(
                    &http,
                    serenity::EditMessage::new().content(codeblock(&format!(
                        "$ {}\n…stopped after {}s timeout; output truncated, process may still run in guest",
                        cmd,
                        LIVE_TIMEOUT_SECS
                    ))),
                )
                .await;
            cleanup_live_files(&vm, &out_f, &code_f).await;
            break;
        }
        let tail = guest_exec(&vm, "/usr/bin/tail", &["-c", "1500", &out_f], true, 10)
            .await
            .map(|(_, o, _)| o)
            .unwrap_or_default();
        let done = guest_status(&vm, pid).await.unwrap_or(None);
        let mut body = format!(
            "\u{1b}[0;32m$ {}\u{1b}[0m\n{}",
            cmd,
            tail.trim_end()
        );
        match done {
            Some(code) => {
                if code != 0 {
                    body.push_str(&format!("\n\u{1b}[0;31mexit {}\u{1b}[0m", code));
                }
                let _ = msg
                    .edit(&http, serenity::EditMessage::new().content(codeblock(&body)))
                    .await;
                cleanup_live_files(&vm, &out_f, &code_f).await;
                break;
            }
            None => {
                body.push_str("\n\u{1b}[0;33m…live\u{1b}[0m");
                if msg
                    .edit(&http, serenity::EditMessage::new().content(codeblock(&body)))
                    .await
                    .is_err()
                {
                    cleanup_live_files(&vm, &out_f, &code_f).await;
                    break;
                }
            }
        }
    }
    remove_live_if_tag(&live_map, channel, tag).await;
}

const MAXTYPE: usize = 120;

async fn user_shell(data: &Data, uid: u64) -> String {
    data.shells
        .read()
        .await
        .get(&uid.to_string())
        .cloned()
        .unwrap_or_else(|| "bash".into())
}

async fn linked_user(data: &Data, uid: u64) -> Option<String> {
    data.allowed
        .read()
        .await
        .linux
        .get(&uid.to_string())
        .cloned()
}

async fn run_guest_cmd(vm: &str, shell: &str, cmd_text: &str, runas: Option<&str>, timeout_s: u64) -> Result<(String, i64), Error> {
    if let Some(u) = runas {
        if !valid_runas(u) {
            return Ok((
                "Linked linux account is invalid; ask the owner to re-add you.".into(),
                -1,
            ));
        }
    }
    if !agent_ping(vm).await {
        return Ok((
            "Guest agent is silent. Install `qemu-guest-agent` in Artix first.".into(),
            -1,
        ));
    }
    let (sh_path, setup, guard) = if shell == "fish" {
        ("/usr/sbin/fish", "set -gx SHELL /usr/sbin/fish; set -gx PATH $HOME/.local/bin $HOME/bin /usr/local/bin $PATH; ", "$status")
    } else {
        ("/bin/bash", "export SHELL=/bin/bash PATH=\"$HOME/.local/bin:$HOME/bin:/usr/local/bin:$PATH\"; ", "$?")
    };
    let inner = cmd_text.trim().trim_end_matches(';').trim_end();
    let shcmd = format!("{}{}; exit {}", setup, inner, guard);
    let (lpath, largs): (&str, Vec<&str>) = match runas {
        Some(u) => ("su", vec![u, "-s", sh_path, "-c", &shcmd]),
        None => (sh_path, vec!["-c", &shcmd]),
    };
    let run = guest_exec(vm, lpath, &largs, true, timeout_s).await;
    let run = match run {
        // Fail closed: retry with the SAME privilege (same `runas`) via
        // /bin/bash. Never fall back from `su <user>` to bare `/bin/bash`
        // (which runs as the agent user, often root).
        Err(e) if e.to_string().contains("No such file") && sh_path != "/bin/bash" => {
            let fallback =
                format!("export SHELL=/bin/bash; {}; exit $?", inner);
            let (lpath2, largs2): (&str, Vec<&str>) = match runas {
                Some(u) => ("su", vec![u, "-s", "/bin/bash", "-c", &fallback]),
                None => ("/bin/bash", vec!["-c", &fallback]),
            };
            guest_exec(vm, lpath2, &largs2, true, timeout_s).await
        }
        other => other,
    };
    match run {
        Ok((code, out, err)) => {
            let mut body = format!(
                "\u{1b}[0;32m$ {}\u{1b}[0m\n{}",
                cmd_text.trim(),
                out.trim_end()
            );
            if !err.trim().is_empty() {
                body.push_str(&format!(
                    "\n\u{1b}[0;31mstderr:\u{1b}[0m\n{}",
                    err.trim_end()
                ));
            }
            if code != 0 {
                body.push_str(&format!("\n\u{1b}[0;31mexit {}\u{1b}[0m", code));
            }
            Ok((body, code))
        }
        Err(e) => Ok((e.to_string(), -1)),
    }
}

fn match_slot<'a>(content: &'a str, s: &PrefixSettings) -> Option<&'a str> {
    if let Some(p) = &s.dollar {
        if content.starts_with(p.as_str()) {
            return Some(&content[p.len()..]);
        }
    }
    if let Some(p) = &s.semicolon {
        if content.starts_with(p.as_str()) {
            return Some(&content[p.len()..]);
        }
    }
    None
}

async fn match_prefix<'a>(
    _ctx: &'a serenity::Context,
    msg: &'a serenity::Message,
    data: &'a Data,
) -> Result<Option<(&'a str, &'a str)>, Error> {
    let s = data.settings.read().await;
    Ok(match_slot(&msg.content, &s).map(|rest| {
        let n = msg.content.len() - rest.len();
        msg.content.split_at(n)
    }))
}

async fn save_settings(data: &Data) -> Result<(), Error> {
    let s = data.settings.read().await;
    save_json(
        "settings.json",
        serde_json::to_string_pretty(&*s)?,
    )
    .await
}

async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    let serenity::FullEvent::Message { new_message } = event else {
        return Ok(());
    };
    if new_message.author.bot {
        return Ok(());
    }
    let text = {
        let s = data.settings.read().await;
        match match_slot(&new_message.content, &s) {
            Some(t) => t,
            None => return Ok(()),
        }
    };
    let text = text.trim_start();
    let text = match text.strip_prefix('/') {
        Some(t) if !t.contains('/') => t.trim_start(),
        _ => text,
    };
    let first = text.trim().split_whitespace().next().unwrap_or("");
    if COMMANDS.contains(&first) {
        return Ok(());
    }
    if text.trim().is_empty() {
        return Ok(());
    }
    let id = new_message.author.id.get();
    let (authed, vm) = {
        let a = data.allowed.read().await;
        (
            (id == a.owner || a.users.contains(&id)),
            data.vm.clone(),
        )
    };
    if !authed {
        eprintln!("denied type: {} (id {})", new_message.author.name, id);
        return Ok(());
    }
    if text.chars().count() > MAXTYPE {
        new_message
            .reply(&ctx.http, "Too long, max 120 chars.")
            .await?;
        return Ok(());
    }
    if first == "live" {
        let cmd = text
            .trim()
            .strip_prefix("live")
            .unwrap_or("")
            .trim()
            .to_string();
        if cmd.is_empty() {
            new_message
                .reply(&ctx.http, "Usage: `$live <command>`.")
                .await?;
            return Ok(());
        }
        if !agent_ping(&vm).await {
            new_message
                .reply(
                    &ctx.http,
                    "Guest agent is silent. Install `qemu-guest-agent` in Artix first.",
                )
                .await?;
            return Ok(());
        }
        let http = ctx.http.clone();
        let ack = new_message
            .reply(&ctx.http, format!("`live: {}` starting…", cmd))
            .await?;
        let runas = linked_user(data, id).await;
        begin_live(
            http,
            ack,
            id,
            &new_message.author.name,
            vm,
            cmd,
            runas,
            data.live.clone(),
        )
        .await;
        return Ok(());
    }
    let sh = user_shell(data, id).await;
    if let Some(old) = abort_live_for_channel(&data.live, new_message.channel_id).await {
        cleanup_live_files(&vm, &old.out_f, &old.code_f).await;
    }
    let runas = linked_user(data, id).await;
    let (body, code) = run_guest_cmd(&vm, &sh, text, runas.as_deref(), 300).await?;
    if new_message.reply(&ctx.http, codeblock(&fit_body(&body))).await.is_err() {
        let _ = new_message.channel_id.say(&ctx.http, codeblock(&fit_body(&body))).await;
    }
    eprintln!(
        "exec for {} (id {}): exit {} runas={:?} cmd={:?}",
        new_message.author.name,
        id,
        code,
        runas,
        text.trim().chars().take(60).collect::<String>()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_becomes_blocks() {
        let out = sanitize_ansi("\x1b[40m   \x1b[41m   \x1b[m");
        assert!(out.contains("\x1b[0;30m███\x1b[0m"), "got {:?}", out);
        assert!(out.contains("\x1b[0;31m███\x1b[0m"), "got {:?}", out);
        assert!(!out.contains("40m"), "got {:?}", out);
    }

    #[test]
    fn middle_cut_keeps_edges() {
        let body = format!("$ cmd\n{}\nexit 0", "x".repeat(3000));
        let out = fit_body(&body);
        assert!(out.starts_with("$ cmd"), "keeps prompt");
        assert!(out.ends_with("exit 0"), "keeps exit line");
        assert!(out.contains("chars omitted"), "marks omission");
        assert!(out.chars().count() <= 1790, "fits");
    }

    #[test]
    fn fg_survives() {
        let out = sanitize_ansi("\x1b[0;32mok\x1b[0m");
        assert_eq!(out, "\x1b[0;32mok\x1b[0m");
    }

    #[test]
    fn runas_validation_blocks_root_and_junk() {
        assert!(valid_runas("matko"));
        assert!(valid_runas("u123"));
        assert!(valid_runas("a-b_c"));
        assert!(!valid_runas("root"));
        assert!(!valid_runas(""));
        assert!(!valid_runas("0abc"));
        assert!(!valid_runas("a/b"));
        assert!(!valid_runas("a b"));
        assert!(!valid_runas("ABC"));
        assert!(!valid_runas(&"a".repeat(33)));
    }

    #[test]
    fn random_suffix_looks_unique_hex() {
        let a = random_suffix();
        let b = random_suffix();
        assert!(!a.is_empty());
        // /dev/urandom path yields 16 hex chars; fallback still non-empty.
        assert!(a.len() >= 8, "got {:?}", a);
        // Very likely unique across two calls (urandom or nanos-based).
        assert_ne!(a, b, "suffix should differ per call");
    }
}

#[tokio::main]
async fn main() {
    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN env missing");
    let owner: u64 = std::env::var("OWNER_ID")
        .expect("OWNER_ID env missing")
        .parse()
        .expect("OWNER_ID must be a number");
    let _ = std::env::set_current_dir(project_dir());
    let vm = std::env::var("VM_NAME").unwrap_or_else(|_| "voidvm".into());
    let prefix_settings: PrefixSettings = tokio::fs::read_to_string("settings.json")
        .await
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    let path = PathBuf::from("users.json");
    let (saved_users, saved_linux): (Vec<u64>, std::collections::HashMap<String, String>) =
        if path.exists() {
            let raw = tokio::fs::read_to_string(&path)
                .await
                .expect("read users.json");
            if raw.trim().is_empty() {
                Default::default()
            } else {
                serde_json::from_str::<AllowedFile>(&raw)
                    .map(|f| (f.users, f.linux))
                    .unwrap_or_default()
            }
    } else {
        Default::default()
    };
    let data = Data {
        allowed: tokio::sync::RwLock::new(Allowed {
            owner,
            users: saved_users,
            path,
            linux: saved_linux,
        }),
        vm,
        live: Default::default(),
        settings: tokio::sync::RwLock::new(prefix_settings),
        shells: tokio::sync::RwLock::new(load_shells()),
    };

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                help(),
                ps(),
                status(),
                start(),
                stop(),
                restart(),
                info(),
                users(),
                userlist(),
                user(),
                useradd(),
                userdel(),
                shell(),
                build(),
                botrestart(),
                run(),
                live(),
                shot(),
                send(),
                settings(),
                colortest(),
            ],
            prefix_options: poise::PrefixFrameworkOptions {
                prefix: None,
                stripped_dynamic_prefix: Some(|ctx, msg, data| {
                    Box::pin(match_prefix(ctx, msg, data))
                }),
                ..Default::default()
            },
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            on_error: |error| {
                Box::pin(async move {
                    eprintln!("framework error: {}", error);
                    if let poise::FrameworkError::Command { ctx, .. } = error {
                        let _ = ctx
                            .say("Something broke on my side — check the terminal log.")
                            .await;
                    }
                })
            },
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(data)
            })
        })
        .build();

    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .expect("client build");
    client.start().await.expect("client start");
}
