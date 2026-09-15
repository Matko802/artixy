use poise::serenity_prelude as serenity;

use crate::{
    scrub::scrub_public_ip,
    termrender::TermFonts,
    util::{codeblock, plain_tail, random_suffix, valid_runas},
    vm::{guest_exec, guest_launch_raw, guest_status},
    webhook::{edit_cleared, edit_posted, post_message, resolve_poster, Poster},
};

pub(crate) struct LiveEntry {
    pub(crate) handle: tokio::task::AbortHandle,
    pub(crate) tag: u64,
    pub(crate) out_f: String,
    pub(crate) code_f: String,
    pub(crate) pid: Option<i64>,
    pub(crate) msg_id: serenity::MessageId,
    pub(crate) in_f: Option<String>,
    pub(crate) author_id: serenity::UserId,
    pub(crate) channel: serenity::ChannelId,
}
pub(crate) type LiveMap = std::sync::Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<(serenity::ChannelId, serenity::UserId), LiveEntry>,
    >,
>;

pub(crate) const LIVE_TIMEOUT_SECS: u64 = 0;
pub(crate) const LIVE_POLL: std::time::Duration = std::time::Duration::from_millis(180);
pub(crate) const LIVE_QUICK: std::time::Duration = std::time::Duration::from_millis(180);
pub(crate) const LIVE_EDIT_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(2000);
pub(crate) const LIVE_EDIT_MAX_FAILS: u8 = 5;
pub(crate) const LIVE_GUEST_MAX_FAILS: u8 = 15;

pub(crate) fn frame_due(
    posted_hash: Option<u64>,
    digest: u64,
    elapsed_since_edit_ms: Option<u64>,
    min_interval_ms: u64,
) -> bool {
    if Some(digest) == posted_hash {
        return false;
    }
    match elapsed_since_edit_ms {
        None => true,
        Some(e) => e >= min_interval_ms,
    }
}
const LIVE_FRAME_BYTES: &str = "200000";

const GUEST_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH";

pub(crate) fn build_runner(
    shell: &str,
    b64: &str,
    out_f: &str,
    code_f: &str,
    input: &str,
    runas: Option<&str>,
) -> String {
    use crate::termrender::{TERM_COLS, TERM_ROWS};
    let home_cd = match runas.filter(|u| valid_runas(u)) {
        Some(u) => format!("cd ~{u} 2>/dev/null || cd \"$HOME\" 2>/dev/null || cd /tmp; "),
        None => "cd \"$HOME\" 2>/dev/null || cd /tmp; ".to_string(),
    };
    format!(
        "export CMD_DATA=\"$(echo {b64} | base64 -d)\"; if command -v script >/dev/null 2>&1; then script -qec \"export TERM=xterm-256color TERM_PROGRAM=rustyterm COLORTERM=truecolor; stty cols {cols} rows {rows}; {shell} -c 'export PATH={path}; {home_cd}eval \\\"\\$CMD_DATA\\\"'\" /dev/null <> {input}; else {shell} -c 'export TERM_PROGRAM=rustyterm COLORTERM=truecolor; export PATH={path}; {home_cd}eval \"$CMD_DATA\"' <> {input}; fi > {out_f} 2>&1; echo $? > {code_f}",
        b64 = b64,
        cols = TERM_COLS,
        rows = TERM_ROWS,
        shell = shell,
        path = GUEST_PATH,
        home_cd = home_cd,
        input = input,
        out_f = out_f,
        code_f = code_f,
    )
}
pub(crate) fn mkfifo_script(path: &str, runas: Option<&str>) -> String {
    match runas {
        Some(u) => format!("rm -f {path} && mkfifo -m 600 {path} && chown {u} {path}"),
        None => format!("rm -f {path} && mkfifo -m 600 {path}"),
    }
}
pub(crate) async fn abort_live_for_user(
    live_map: &LiveMap,
    channel: serenity::ChannelId,
    user: serenity::UserId,
) -> Option<LiveEntry> {
    let old = live_map.lock().await.remove(&(channel, user));
    if let Some(ref e) = old {
        e.handle.abort();
    }
    old
}

#[allow(dead_code)]
pub(crate) async fn abort_live_for_channel(
    live_map: &LiveMap,
    channel: serenity::ChannelId,
) -> Option<LiveEntry> {
    let mut m = live_map.lock().await;
    let key = m.keys().find(|(c, _)| *c == channel).cloned();
    if let Some(k) = key {
        let old = m.remove(&k);
        if let Some(ref e) = &old {
            e.handle.abort();
        }
        old
    } else {
        None
    }
}

pub(crate) async fn remove_live_if_tag(
    live_map: &LiveMap,
    channel: serenity::ChannelId,
    user: serenity::UserId,
    tag: u64,
) {
    let mut m = live_map.lock().await;
    let key = (channel, user);
    if m.get(&key).map(|e| e.tag) == Some(tag) {
        m.remove(&key);
    }
}

pub(crate) async fn cleanup_live_files(vm: &str, out_f: &str, code_f: &str, in_f: Option<&str>) {
    match in_f {
        Some(f) => {
            let _ = guest_exec(vm, "/bin/rm", &["-f", out_f, code_f, f], false, 10).await;
        }
        None => {
            let _ = guest_exec(vm, "/bin/rm", &["-f", out_f, code_f], false, 10).await;
        }
    }
}

const KITTY_ANSWER: &str = "\x1b[?0u";
const DA_ANSWER: &str = "\x1b[?1;2c";
const ANSWER_ATTEMPTS: u8 = 3;
#[allow(dead_code)]
const KITTY_QUERY: &str = "\x1b[?u";
#[allow(dead_code)]
const DA_QUERY: &str = "\x1b[c";

#[allow(dead_code)]
pub(crate) fn pending_queries(output: &str, answered_kitty: bool, answered_da: bool) -> Vec<&'static str> {
    let mut found: Vec<(usize, &'static str)> = Vec::new();
    if !answered_kitty {
        if let Some(pos) = output.rfind(KITTY_QUERY) {
            found.push((pos, KITTY_ANSWER));
        }
    }
    if !answered_da {
        if let Some(pos) = output.rfind(DA_QUERY) {
            found.push((pos, DA_ANSWER));
        }
    }
    found.sort();
    found.into_iter().map(|(_, answer)| answer).collect()
}
const MAX_KEY_REPEAT: u32 = 100;
#[allow(dead_code)]
pub(crate) fn terminal_key(text: &str) -> Option<String> {
    let mut parts = text.split_whitespace();
    let base = key_base(parts.next()?)?;
    let count = match parts.next() {
        None => 1,
        Some(n) => {
            let n: u32 = n.parse().ok()?;
            if n == 0 || n > MAX_KEY_REPEAT || parts.next().is_some() {
                return None;
            }
            n
        }
    };
    Some(base.repeat(count as usize))
}

fn key_base(word: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    if lower.starts_with(";ctrl+") {
        let suffix = &lower[6..];
        if suffix.len() == 1 {
            let ch = suffix.chars().next()?;
            if !('a'..='z').contains(&ch) {
                return None;
            }
            let code = (ch as u8 - b'a' + 1) as char;
            return Some(code.to_string());
        }
        return match suffix {
            "return" => Some("\x7f".to_string()),
            "space" => Some("\x00".to_string()),
            "enter" => Some("\n".to_string()),
            "esc" => Some("\x1b".to_string()),
            "up" => Some("\x1b[1;5A".to_string()),
            "down" => Some("\x1b[1;5B".to_string()),
            "right" => Some("\x1b[1;5C".to_string()),
            "left" => Some("\x1b[1;5D".to_string()),
            _ => None,
        };
    }
    match lower.as_str() {
        ";return" => Some("\x7f".to_string()),
        ";space" => Some(" ".to_string()),
        ";enter" => Some("\r".to_string()),
        ";esc" => Some("\x1b".to_string()),
        ";up" => Some("\x1b[A".to_string()),
        ";down" => Some("\x1b[B".to_string()),
        ";right" => Some("\x1b[C".to_string()),
        ";left" => Some("\x1b[D".to_string()),
        _ => None,
    }
}
pub(crate) fn unescape_typed_input(text: &str) -> String {
    text.replace("\\n", "\n")
}
fn expand_line(line: &str) -> String {
    let base = line.as_ptr() as usize;
    let mut out = String::new();
    let mut words = line.split_whitespace().peekable();
    let mut end = 0usize;
    while let Some(word) = words.next() {
        let start = word.as_ptr() as usize - base;
        let wend = start + word.len();
        match key_base(word) {
            None => {
                out.push_str(&line[end..start]);
                out.push_str(word);
                end = wend;
            }
            Some(key) => {
                let mut count = 1u32;
                let mut stop = wend;
                if let Some(&nxt) = words.peek() {
                    if let Ok(n) = nxt.parse::<u32>() {
                        if n >= 1 && n <= MAX_KEY_REPEAT {
                            count = n;
                            let cw = words.next().unwrap();
                            stop = cw.as_ptr() as usize - base + cw.len();
                        }
                    }
                }
                end = stop;
                out.push_str(&key.repeat(count as usize));
            }
        }
    }
    out.push_str(&line[end..]);
    out
}
pub(crate) fn expand_typed_input(text: &str) -> String {
    unescape_typed_input(text)
        .split('\n')
        .map(expand_line)
        .collect::<Vec<_>>()
        .join("\n")
}
pub(crate) async fn forward_terminal_input(
    vm: &str,
    fifo: &str,
    runas: Option<&str>,
    text: &str,
) -> bool {
    use base64::Engine as _;
    let runas = runas.filter(|u| valid_runas(u));
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let inner = format!("echo {} | base64 -d >> {}", b64, fifo);
    let script = match runas {
        Some(u) => format!("timeout 8 su {} -s /bin/bash -c '{}'", u, inner),
        None => format!("timeout 8 bash -c '{}'", inner),
    };
    match guest_exec(vm, "/bin/bash", &["-c", &script], false, 15).await {
        Ok((0, _, _)) => true,
        Ok((code, _, _)) => {
            eprintln!("live: terminal input not delivered (rc {})", code);
            false
        }
        Err(e) => {
            eprintln!("live: terminal input failed ({})", e);
            false
        }
    }
}

pub(crate) async fn cleanup_stale_live_files(vm: &str) {
    let _ = guest_exec(
        vm,
        "/bin/bash",
        &["-c", "rm -f /tmp/podbot-live-*; pkill -f 'podbot-live-' 2>/dev/null; true"],
        false,
        15,
    )
    .await;
}

fn load_terminal_fonts() -> Option<(Vec<u8>, Vec<u8>)> {
    let reg = crate::termrender::system_font_bytes("DejaVu Sans Mono")?;
    let bold = crate::termrender::system_font_bytes("DejaVu Sans Mono:weight=bold")
        .unwrap_or_else(|| reg.clone());
    Some((reg, bold))
}

async fn edit_final(
    poster: &Poster,
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    target: serenity::MessageId,
    content: String,
    files: Vec<(String, Vec<u8>)>,
) -> bool {
    if edit_posted(poster, http, channel, target, content.clone(), files.clone()).await {
        return true;
    }
    tokio::time::sleep(LIVE_EDIT_MIN_INTERVAL).await;
    edit_posted(poster, http, channel, target, content, files).await
}

pub(crate) const LIVE_CLOSED_TEXT: &str = "This live session has been closed.";

pub(crate) fn live_closed_text() -> String {
    codeblock(LIVE_CLOSED_TEXT)
}

pub(crate) fn live_end_closes(code: i64, posted_live_frame: bool, has_fonts: bool) -> bool {
    code == 0 && posted_live_frame && has_fonts
}

pub(crate) async fn close_live_message(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    target: serenity::MessageId,
) {
    let poster = resolve_poster(http, channel).await;
    let text = live_closed_text();
    if !edit_cleared(&poster, http, channel, target, text.clone()).await {
        tokio::time::sleep(LIVE_EDIT_MIN_INTERVAL).await;
        edit_cleared(&poster, http, channel, target, text).await;
    }
}

pub(crate) fn slow_cycle_note(fetch_ms: u128, render_ms: u128, edit_ms: u128) -> Option<String> {
    let total = fetch_ms + render_ms + edit_ms;
    if total < 3000 {
        return None;
    }
    Some(format!("slow cycle {}ms (fetch {}ms render {}ms edit {}ms)", total, fetch_ms, render_ms, edit_ms))
}
async fn note_stalled_feed(
    http: &std::sync::Arc<serenity::Http>,
    channel: serenity::ChannelId,
    cmd: &str,
    reason: &str,
) {
    let _ = post_message(
        http,
        channel,
        plain_tail(&format!("$ {}\n…live updates stopped: {}", cmd, reason)),
        Vec::new(),
    )
    .await;
}

async fn kitty_file_blobs(
    vm: &str,
    text: &str,
    cache: &mut std::collections::HashMap<Vec<u8>, Option<Vec<u8>>>,
) {
    if cache.len() > 32 {
        return;
    }
    let mut fetched = 0;
    for p in crate::kitty::needed_guest_files(text.as_bytes()) {
        if cache.contains_key(&p) || fetched >= 4 {
            continue;
        }
        fetched += 1;
        let got = crate::vm::guest_file_b64(vm, &String::from_utf8_lossy(&p), 16_000_000).await;
        cache.insert(p, got);
    }
}

async fn live_message(
    fonts: Option<&TermFonts>,
    cmd: &str,
    output: &str,
    kfiles: &std::collections::HashMap<Vec<u8>, Option<Vec<u8>>>,
    region: &mut Option<(u32, u32)>,
) -> (String, Vec<(String, Vec<u8>)>) {
    let caption = format!("$ {}", cmd);
    match fonts.and_then(|f| crate::termrender::render_terminal(f, output.as_bytes(), kfiles, region)) {
        Some(png) => (caption, vec![("live.png".to_string(), png)]),
        None => {
            let mut combined = caption.clone();
            if !output.trim().is_empty() {
                combined.push('\n');
                combined.push_str(output.trim_end());
            }
            (plain_tail(&combined), Vec::new())
        }
    }
}

pub(crate) async fn begin_run(
    http: std::sync::Arc<serenity::Http>,
    ack: serenity::Message,
    author_id: u64,
    author_name: &str,
    vm: String,
    cmd: String,
    runas: Option<String>,
    live_map: LiveMap,
    scrub_ip: bool,
) {
    if let Some(ref u) = runas {
        if !valid_runas(u) {
            let _ = crate::webhook::post_message(
                &http,
                ack.channel_id,
                plain_tail("linked linux account is invalid; ask the owner to re-add you."),
                Vec::new(),
            )
            .await;
            return;
        }
    }
    let channel = ack.channel_id;
    let poster = resolve_poster(&http, channel).await;
    let tag = ack.id.get();
    let ack_id = ack.id;
    let author_user = serenity::UserId::new(author_id);
    let rand = random_suffix();
    let out_f = format!("/tmp/podbot-live-{}-{}.out", tag, rand);
    let code_f = format!("/tmp/podbot-live-{}-{}.code", tag, rand);
    let in_f = format!("/tmp/podbot-live-{}-{}.in", tag, rand);
    let in_opt = match guest_exec(
        &vm,
        "/bin/bash",
        &["-c", &mkfifo_script(&in_f, runas.as_deref())],
        false,
        10,
    )
    .await
    {
        Ok((0, _, _)) => Some(in_f.clone()),
        _ => None,
    };
    if let Some(old) = abort_live_for_user(&live_map, channel, author_user).await {
        let vm_clone = vm.clone();
        let http_clone = http.clone();
        tokio::spawn(async move {
            if let Some(pid) = old.pid {
                crate::vm::guest_kill_tree(&vm_clone, pid).await;
            }
            cleanup_live_files(&vm_clone, &old.out_f, &old.code_f, old.in_f.as_deref()).await;
            close_live_message(&http_clone, old.channel, old.msg_id).await;
        });
    }
    let live_map2 = live_map.clone();
    let (vm2, cmd2, runas2, out_f2, code_f2, in_f2) = (
        vm.clone(),
        cmd.clone(),
        runas.clone(),
        out_f.clone(),
        code_f.clone(),
        in_opt.clone(),
    );
    let author_for_run = author_user;
    let handle = tokio::spawn(async move {
        live_run(
            http, ack, channel, tag, vm2, cmd2, runas2, out_f2, code_f2, in_f2, live_map2, scrub_ip, poster, author_for_run,
        )
        .await;
    })
    .abort_handle();
    live_map.lock().await.insert(
        (channel, author_user),
        LiveEntry {
            handle,
            tag,
            out_f,
            code_f,
            pid: None,
            msg_id: ack_id,
            in_f: in_opt,
            author_id: author_user,
            channel,
        },
    );
    eprintln!("run started for {} (id {})", author_name, author_id);
}

pub(crate) async fn live_run(
    http: std::sync::Arc<serenity::Http>,
    msg: serenity::Message,
    channel: serenity::ChannelId,
    tag: u64,
    vm: String,
    cmd: String,
    runas: Option<String>,
    out_f: String,
    code_f: String,
    in_f: Option<String>,
    live_map: LiveMap,
    scrub_ip: bool,
    poster: Poster,
    author: serenity::UserId,
) {
    use base64::Engine as _;
    if let Some(ref u) = runas {
        if !valid_runas(u) {
            edit_posted(
                &poster,
                &http,
                channel,
                msg.id,
                plain_tail("linked linux account is invalid; ask the owner to re-add you."),
                Vec::new(),
            )
            .await;
            remove_live_if_tag(&live_map, channel, author, tag).await;
            return;
        }
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(cmd.as_bytes());
    let input = in_f.as_deref().unwrap_or("/dev/null");
    let script = build_runner("bash", &b64, &out_f, &code_f, input, runas.as_deref());
    let script_sh = build_runner("sh", &b64, &out_f, &code_f, input, runas.as_deref());
    let (lpath, largs): (&str, Vec<&str>) = match &runas {
        Some(u) => ("su", vec!["-", u.as_str(), "-s", "/bin/bash", "-c", &script]),
        None => ("/bin/bash", vec!["-c", &script]),
    };
    let launched = guest_launch_raw(&vm, lpath, &largs, false).await;
    let launched = match launched {
        Err(e) if e.to_string().contains("No such file") => match &runas {
            Some(u) => {
                guest_launch_raw(&vm, "su", &["-", u.as_str(), "-s", "/bin/sh", "-c", &script_sh], false)
                    .await
            }
            None => guest_launch_raw(&vm, "/bin/sh", &["-c", &script_sh], false).await,
        },
        other => other,
    };
    let pid = match launched {
        Ok(p) => p,
        Err(e) => {
            edit_posted(
                &poster,
                &http,
                channel,
                msg.id,
                plain_tail(&e.to_string()),
                Vec::new(),
            )
            .await;
            cleanup_live_files(&vm, &out_f, &code_f, in_f.as_deref()).await;
            remove_live_if_tag(&live_map, channel, author, tag).await;
            return;
        }
    };
    {
        let mut m = live_map.lock().await;
        if let Some(e) = m.get_mut(&(channel, author)) {
            if e.tag == tag {
                e.pid = Some(pid);
            }
        }
    }
    let started = std::time::Instant::now();
    let fonts = match load_terminal_fonts() {
        Some((reg, bold)) => TermFonts::load(&reg, &bold),
        None => {
            eprintln!("live: no monospace font found (fc-match missing?); text fallback");
            None
        }
    };
    let mut first = true;
    let mut region: Option<(u32, u32)> = None;
    let mut answer_fails: u8 = 0;
    let mut last_edit: Option<std::time::Instant> = Some(std::time::Instant::now());
    let mut edit_fails: u8 = 0;
    let mut guest_fails: u8 = 0;
    let mut posted_hash: Option<u64> = None;
    let mut kfiles: std::collections::HashMap<Vec<u8>, Option<Vec<u8>>> =
        std::collections::HashMap::new();
    let (mut dsr_term, mut dsr_processor, dsr_writes) = crate::termrender::new_collecting_term();
    let mut prev_fetched = String::new();
    loop {
        let wait = if first { LIVE_QUICK } else { LIVE_POLL };
        tokio::time::sleep(wait).await;
        let cycle_start = std::time::Instant::now();
        if LIVE_TIMEOUT_SECS != 0 && started.elapsed().as_secs() > LIVE_TIMEOUT_SECS {
            let huge = guest_exec(&vm, "/usr/bin/wc", &["-c", &out_f], true, 10)
                .await
                .map(|(_, o, _)| {
                    o.split_whitespace()
                        .next()
                        .and_then(|n| n.parse::<u64>().ok())
                        .unwrap_or(0)
                })
                .unwrap_or(0)
                > 20_000_000;
            if huge {
                crate::vm::guest_kill_tree(&vm, pid).await;
            }
            let timeout_text = plain_tail(&format!(
                "$ {}\n…stopped after {}s timeout; output truncated{}",
                cmd,
                LIVE_TIMEOUT_SECS,
                if huge {
                    "; runaway process stopped"
                } else {
                    "; process may still run in guest"
                }
            ));
            if !edit_cleared(&poster, &http, channel, msg.id, timeout_text.clone()).await {
                tokio::time::sleep(LIVE_EDIT_MIN_INTERVAL).await;
                edit_cleared(&poster, &http, channel, msg.id, timeout_text).await;
            }
            cleanup_live_files(&vm, &out_f, &code_f, in_f.as_deref()).await;
            break;
        }
        let fetched = match guest_exec(&vm, "/usr/bin/tail", &["-c", LIVE_FRAME_BYTES, &out_f], true, 15).await {
            Ok((_, o, _)) => o,
            Err(e) => {
                guest_fails += 1;
                eprintln!("live: guest fetch failed ({}/{}) for {}: {}", guest_fails, LIVE_GUEST_MAX_FAILS, out_f, e);
                if guest_fails >= LIVE_GUEST_MAX_FAILS {
                    note_stalled_feed(&http, channel, &cmd, "guest agent stopped answering").await;
                    close_live_message(&http, channel, msg.id).await;
                    cleanup_live_files(&vm, &out_f, &code_f, in_f.as_deref()).await;
                    break;
                }
                first = false;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        let fetched = if scrub_ip { scrub_public_ip(&fetched) } else { fetched };
        let done = match guest_status(&vm, pid).await {
            Ok(d) => {
                guest_fails = 0;
                d
            }
            Err(e) => {
                guest_fails += 1;
                eprintln!("live: guest status failed ({}/{}) for {}: {}", guest_fails, LIVE_GUEST_MAX_FAILS, out_f, e);
                if guest_fails >= LIVE_GUEST_MAX_FAILS {
                    note_stalled_feed(&http, channel, &cmd, "guest agent stopped answering").await;
                    close_live_message(&http, channel, msg.id).await;
                    cleanup_live_files(&vm, &out_f, &code_f, in_f.as_deref()).await;
                    break;
                }
                first = false;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        let fetch_ms = cycle_start.elapsed().as_millis();
        match done {
            Some(code) => {
                let full = guest_exec(&vm, "/usr/bin/tail", &["-c", "500000", &out_f], true, 30)
                    .await
                    .map(|(_, o, _)| o)
                    .unwrap_or(fetched);
                let full = if scrub_ip { scrub_public_ip(&full) } else { full };
                let header = format!("$ {}", cmd);
                let mut output = full.trim_end().to_string();
                if code != 0 {
                    output.push_str(&format!("\nexit {}", code));
                }
                if first {
                    let combined = if output.trim().is_empty() {
                        header.clone()
                    } else {
                        format!("{}\n{}", header, output.trim_end())
                    };
                    let is_long = combined.chars().count() > 1800;
                    kitty_file_blobs(&vm, &full, &mut kfiles).await;
                    let posted = if is_long && fonts.is_some() {
                        let (text, files) =
                            live_message(fonts.as_ref(), &cmd, &output, &kfiles, &mut region).await;
                        edit_final(&poster, &http, channel, msg.id, text, files).await
                    } else {
                        edit_final(&poster, &http, channel, msg.id, plain_tail(&combined), Vec::new()).await
                    };
                    if !posted {
                        note_stalled_feed(&http, channel, &cmd, "Discord kept rejecting message edits").await;
                    }
                } else if live_end_closes(code, posted_hash.is_some(), fonts.is_some()) {
                    close_live_message(&http, channel, msg.id).await;
                } else {
                    kitty_file_blobs(&vm, &full, &mut kfiles).await;
                    let (text, files) =
                        live_message(fonts.as_ref(), &cmd, &output, &kfiles, &mut region).await;
                    if !edit_final(&poster, &http, channel, msg.id, text, files).await {
                        note_stalled_feed(&http, channel, &cmd, "Discord kept rejecting message edits").await;
                    }
                }
                cleanup_live_files(&vm, &out_f, &code_f, in_f.as_deref()).await;
                break;
            }
            None => {
                if let Some(ref fifo) = in_f {
                    if answer_fails < ANSWER_ATTEMPTS * 4 {
                        let new_bytes = crate::termrender::new_bytes_since(&prev_fetched, &fetched);
                        if !new_bytes.is_empty() {
                            dsr_processor.advance(&mut dsr_term, new_bytes.as_bytes());
                            let pending: Vec<String> = {
                                let mut w = dsr_writes.lock().unwrap();
                                let v = w.clone();
                                w.clear();
                                v
                            };
                            for answer in pending {
                                if forward_terminal_input(&vm, fifo, runas.as_deref(), &answer).await {
                                    eprintln!("live: answered pty query in {}: {:?}", out_f, answer.escape_debug());
                                } else {
                                    answer_fails += 1;
                                }
                                if answer_fails >= ANSWER_ATTEMPTS * 4 { break; }
                            }
                            for probe in crate::kitty::graphics_query_answers(new_bytes.as_bytes()) {
                                let text = String::from_utf8_lossy(&probe).into_owned();
                                if forward_terminal_input(&vm, fifo, runas.as_deref(), &text).await {
                                    eprintln!("live: answered kitty graphics probe in {}", out_f);
                                } else {
                                    answer_fails += 1;
                                }
                                if answer_fails >= ANSWER_ATTEMPTS * 4 { break; }
                            }
                        }
                    }
                }
                prev_fetched = fetched.clone();
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                fetched.hash(&mut hasher);
                let digest = hasher.finish();
                let since_edit = last_edit.map(|t| t.elapsed().as_millis() as u64);
                if !frame_due(
                    posted_hash,
                    digest,
                    since_edit,
                    LIVE_EDIT_MIN_INTERVAL.as_millis() as u64,
                ) {
                    first = false;
                    continue;
                }
                kitty_file_blobs(&vm, &fetched, &mut kfiles).await;
                let render_start = std::time::Instant::now();
                let (text, files) =
                    live_message(fonts.as_ref(), &cmd, &fetched, &kfiles, &mut region).await;
                let render_ms = render_start.elapsed().as_millis();
                let edit_start = std::time::Instant::now();
                if edit_posted(&poster, &http, channel, msg.id, text, files).await {
                    let edit_ms = edit_start.elapsed().as_millis();
                    if let Some(note) = slow_cycle_note(fetch_ms, render_ms, edit_ms) {
                        eprintln!("live: {} for {}", note, out_f);
                    }
                    last_edit = Some(std::time::Instant::now());
                    edit_fails = 0;
                    posted_hash = Some(digest);
                } else {
                    let edit_ms = edit_start.elapsed().as_millis();
                    if let Some(note) = slow_cycle_note(fetch_ms, render_ms, edit_ms) {
                        eprintln!("live: {} for {} (edit failed)", note, out_f);
                    }
                    edit_fails += 1;
                    last_edit = Some(std::time::Instant::now());
                    eprintln!(
                        "live: edit {}/{} failed for {} (transient unless repeated)",
                        edit_fails, LIVE_EDIT_MAX_FAILS, out_f
                    );
                    if edit_fails >= LIVE_EDIT_MAX_FAILS {
                        note_stalled_feed(&http, channel, &cmd, "Discord kept rejecting message edits").await;
                        close_live_message(&http, channel, msg.id).await;
                        cleanup_live_files(&vm, &out_f, &code_f, in_f.as_deref()).await;
                        break;
                    }
                }
            }
        }
        first = false;
    }
    remove_live_if_tag(&live_map, channel, author, tag).await;
}
