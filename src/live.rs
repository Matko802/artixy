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
}
pub(crate) type LiveMap = std::sync::Arc<
    tokio::sync::Mutex<std::collections::HashMap<serenity::ChannelId, LiveEntry>>,
>;

pub(crate) const LIVE_TIMEOUT_SECS: u64 = 600;
pub(crate) const LIVE_POLL_SECS: u64 = 1;
pub(crate) const LIVE_QUICK_SECS: u64 = 1;
const LIVE_FRAME_BYTES: &str = "200000";

const GUEST_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH";

pub(crate) fn build_runner(shell: &str, b64: &str, out_f: &str, code_f: &str, input: &str) -> String {
    use crate::termrender::{TERM_COLS, TERM_ROWS};
    format!(
        "export CMD_DATA=\"$(echo {b64} | base64 -d)\"; if command -v script >/dev/null 2>&1; then script -qec \"export TERM=xterm-256color; stty cols {cols} rows {rows}; {shell} -c 'export PATH={path}; eval \\\"\\$CMD_DATA\\\"'\" /dev/null <> {input}; else {shell} -c 'export PATH={path}; eval \"$CMD_DATA\"' <> {input}; fi > {out_f} 2>&1; echo $? > {code_f}",
        b64 = b64,
        cols = TERM_COLS,
        rows = TERM_ROWS,
        shell = shell,
        path = GUEST_PATH,
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
pub(crate) async fn abort_live_for_channel(
    live_map: &LiveMap,
    channel: serenity::ChannelId,
) -> Option<LiveEntry> {
    let old = live_map.lock().await.remove(&channel);
    if let Some(ref e) = old {
        e.handle.abort();
    }
    old
}

pub(crate) async fn remove_live_if_tag(live_map: &LiveMap, channel: serenity::ChannelId, tag: u64) {
    let mut m = live_map.lock().await;
    if m.get(&channel).map(|e| e.tag) == Some(tag) {
        m.remove(&channel);
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

const KITTY_QUERY: &str = "\x1b[?u";
const KITTY_ANSWER: &str = "\x1b[?0u";
const DA_QUERY: &str = "\x1b[c";
const DA_ANSWER: &str = "\x1b[?1;2c";
const ANSWER_ATTEMPTS: u8 = 3;

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
    let base: &str = match parts.next()? {
        ".return" => "\x7f",
        ".space" => " ",
        ".enter" => "\r",
        ".." => "\x1b",
        ".up" => "\x1b[A",
        ".down" => "\x1b[B",
        ".right" => "\x1b[C",
        ".left" => "\x1b[D",
        _ => return None,
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
    if let Some(old) = abort_live_for_channel(&live_map, channel).await {
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
    let handle = tokio::spawn(async move {
        live_run(http, ack, channel, tag, vm2, cmd2, runas2, out_f2, code_f2, in_f2, live_map2, scrub_ip, poster).await;
    })
    .abort_handle();
    live_map.lock().await.insert(
        channel,
        LiveEntry {
            handle,
            tag,
            out_f,
            code_f,
            pid: None,
            msg_id: ack_id,
            in_f: in_opt,
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
            remove_live_if_tag(&live_map, channel, tag).await;
            return;
        }
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(cmd.as_bytes());
    let input = in_f.as_deref().unwrap_or("/dev/null");
    let script = build_runner("bash", &b64, &out_f, &code_f, input);
    let script_sh = build_runner("sh", &b64, &out_f, &code_f, input);
    let (lpath, largs): (&str, Vec<&str>) = match &runas {
        Some(u) => ("su", vec![u.as_str(), "-s", "/bin/bash", "-c", &script]),
        None => ("/bin/bash", vec!["-c", &script]),
    };
    let launched = guest_launch_raw(&vm, lpath, &largs, false).await;
    let launched = match launched {
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
            remove_live_if_tag(&live_map, channel, tag).await;
            return;
        }
    };
    {
        let mut m = live_map.lock().await;
        if let Some(e) = m.get_mut(&channel) {
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
    let mut answered_kitty = false;
    let mut answered_da = false;
    let mut answer_fails: u8 = 0;
    let mut last_hash: u64 = 0;
    let mut hashed_once = false;
    loop {
        let wait = if first { LIVE_QUICK_SECS } else { LIVE_POLL_SECS };
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        if started.elapsed().as_secs() > LIVE_TIMEOUT_SECS {
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
                    edit_posted(&poster, &http, channel, msg.id, plain_tail(&combined), Vec::new()).await;
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
                    if answer_fails < ANSWER_ATTEMPTS * 2 {
                        for answer in pending_queries(&fetched, answered_kitty, answered_da) {
                            let is_kitty = answer == KITTY_ANSWER;
                            if forward_terminal_input(&vm, fifo, runas.as_deref(), answer).await {
                                eprintln!("live: answered a terminal query in {}", out_f);
                                if is_kitty {
                                    answered_kitty = true;
                                } else {
                                    answered_da = true;
                                }
                            } else {
                                answer_fails += 1;
                            }
                        }
                    }
                }
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
    remove_live_if_tag(&live_map, channel, tag).await;
}
