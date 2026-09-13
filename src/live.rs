use poise::serenity_prelude as serenity;

use crate::{
    scrub::scrub_public_ip,
    termrender::TermFonts,
    util::{plain_tail, random_suffix, valid_runas},
    vm::{guest_exec, guest_launch_raw, guest_status},
    webhook::{edit_posted, resolve_poster, Poster},
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

pub(crate) const LIVE_TIMEOUT_SECS: u64 = 0; // 0 = no timeout, interactive apps stay alive
pub(crate) const LIVE_POLL: std::time::Duration = std::time::Duration::from_millis(180);
pub(crate) const LIVE_QUICK: std::time::Duration = std::time::Duration::from_millis(180);
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
    // Guest-exec inherits qemu-ga's cwd (often / or a root-owned service dir
    // like /etc/dinit.d), and plain `su user` keeps root's $HOME. Both break
    // builds: `git clone` can't mkdir and `makepkg` refuses with
    // "You do not have write permission for $BUILDDIR".
    // So the inner shell always cds somewhere writable first. With a login
    // `su -` (see live_run) ~ and $HOME already point at the user's home;
    // the `cd ~user` prefix covers non-login fallbacks too.
    let home_cd = match runas.filter(|u| valid_runas(u)) {
        Some(u) => format!("cd ~{u} 2>/dev/null || cd \"$HOME\" 2>/dev/null || cd /tmp; "),
        None => "cd \"$HOME\" 2>/dev/null || cd /tmp; ".to_string(),
    };
    format!(
        "export CMD_DATA=\"$(echo {b64} | base64 -d)\"; if command -v script >/dev/null 2>&1; then script -qec \"export TERM=xterm-256color; stty cols {cols} rows {rows}; {shell} -c 'export PATH={path}; {home_cd}eval \\\"\\$CMD_DATA\\\"'\" /dev/null <> {input}; else {shell} -c 'export PATH={path}; {home_cd}eval \"$CMD_DATA\"' <> {input}; fi > {out_f} 2>&1; echo $? > {code_f}",
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

// legacy name kept for any external callers — now per-user
#[allow(dead_code)]
pub(crate) async fn abort_live_for_channel(
    live_map: &LiveMap,
    channel: serenity::ChannelId,
) -> Option<LiveEntry> {
    // fallback: remove any one entry for that channel (used only for non-live cleanups)
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
pub(crate) fn terminal_key(text: &str) -> Option<String> {
    const MAX_REPEAT: u32 = 100;
    let mut parts = text.split_whitespace();
    let first = parts.next()?;
    let lower = first.to_ascii_lowercase();
    let base = if lower.starts_with(";ctrl+") {
        let suffix = &lower[6..];
        if suffix.len() == 1 {
            let ch = suffix.chars().next()?;
            if !('a'..='z').contains(&ch) {
                return None;
            }
            let code = (ch as u8 - b'a' + 1) as char;
            code.to_string()
        } else {
            match suffix {
                "return" => "\x7f".to_string(),
                "space" => "\x00".to_string(),
                "enter" => "\n".to_string(),
                "esc" => "\x1b".to_string(),
                "up" => "\x1b[1;5A".to_string(),
                "down" => "\x1b[1;5B".to_string(),
                "right" => "\x1b[1;5C".to_string(),
                "left" => "\x1b[1;5D".to_string(),
                _ => return None,
            }
        }
    } else {
        match lower.as_str() {
            ";return" => "\x7f".to_string(),
            ";space" => " ".to_string(),
            ";enter" => "\r".to_string(),
            ";esc" => "\x1b".to_string(),
            ";up" => "\x1b[A".to_string(),
            ";down" => "\x1b[B".to_string(),
            ";right" => "\x1b[C".to_string(),
            ";left" => "\x1b[D".to_string(),
            _ => return None,
        }
    };
    let count = match parts.next() {
        None => 1,
        Some(n) => {
            let n: u32 = n.parse().ok()?;
            if n == 0 || n > MAX_REPEAT || parts.next().is_some() {
                return None;
            }
            n
        }
    };
    Some(base.repeat(count as usize))
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

async fn live_message(
    fonts: Option<&TermFonts>,
    cmd: &str,
    output: &str,
    region: &mut Option<(u32, u32)>,
) -> (String, Vec<(String, Vec<u8>)>) {
    let caption = format!("$ {}", cmd);
    match fonts.and_then(|f| crate::termrender::render_terminal(f, output.as_bytes(), region)) {
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
        tokio::spawn(async move {
            if let Some(pid) = old.pid {
                crate::vm::guest_kill_tree(&vm_clone, pid).await;
            }
            cleanup_live_files(&vm_clone, &old.out_f, &old.code_f, old.in_f.as_deref()).await;
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
    // `su -` (login) sets HOME/USER and cds into the user's home. Plain
    // `su user` keeps qemu-ga's cwd (e.g. /etc/dinit.d) and HOME=/root,
    // which breaks git/makepkg with "Permission denied" / bad $BUILDDIR.
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
    let mut last_hash: u64 = 0;
    let mut hashed_once = false;
    let (mut dsr_term, mut dsr_processor, dsr_writes) = crate::termrender::new_collecting_term();
    let mut prev_fetched = String::new();
    loop {
        let wait = if first { LIVE_QUICK } else { LIVE_POLL };
        tokio::time::sleep(wait).await;
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
            edit_posted(
                &poster,
                &http,
                channel,
                msg.id,
                plain_tail(&format!(
                    "$ {}\n…stopped after {}s timeout; output truncated{}",
                    cmd,
                    LIVE_TIMEOUT_SECS,
                    if huge {
                        "; runaway process stopped"
                    } else {
                        "; process may still run in guest"
                    }
                )),
                Vec::new(),
            )
            .await;
            cleanup_live_files(&vm, &out_f, &code_f, in_f.as_deref()).await;
            break;
        }
        let fetched = guest_exec(&vm, "/usr/bin/tail", &["-c", LIVE_FRAME_BYTES, &out_f], true, 15)
            .await
            .map(|(_, o, _)| o)
            .unwrap_or_default();
        let fetched = if scrub_ip { scrub_public_ip(&fetched) } else { fetched };
        let done = guest_status(&vm, pid).await.unwrap_or(None);
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
                    if is_long && fonts.is_some() {
                        let (text, files) =
                            live_message(fonts.as_ref(), &cmd, &output, &mut region).await;
                        edit_posted(&poster, &http, channel, msg.id, text, files).await;
                    } else {
                        edit_posted(&poster, &http, channel, msg.id, plain_tail(&combined), Vec::new()).await;
                    }
                } else {
                    let (text, files) =
                        live_message(fonts.as_ref(), &cmd, &output, &mut region).await;
                    edit_posted(&poster, &http, channel, msg.id, text, files).await;
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
                        }
                    }
                }
                prev_fetched = fetched.clone();
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                fetched.hash(&mut hasher);
                let digest = hasher.finish();
                if hashed_once && digest == last_hash {
                    first = false;
                    continue;
                }
                last_hash = digest;
                hashed_once = true;
                let (text, files) =
                    live_message(fonts.as_ref(), &cmd, &fetched, &mut region).await;
                if !edit_posted(&poster, &http, channel, msg.id, text, files).await {
                    cleanup_live_files(&vm, &out_f, &code_f, in_f.as_deref()).await;
                    break;
                }
            }
        }
        first = false;
    }
    remove_live_if_tag(&live_map, channel, author, tag).await;
}
