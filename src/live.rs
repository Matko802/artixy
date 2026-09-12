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
/// First poll comes sooner: commands finishing inside this window get a plain
/// text reply instead of the live image feed.
pub(crate) const LIVE_QUICK_SECS: u64 = 1;
/// Guest output fetched per poll for the image frame (bytes, not chars).
const LIVE_FRAME_BYTES: &str = "200000";

/// PATH exported inside the guest before running user commands.
const GUEST_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH";

/// Build the guest runner script. The user command travels base64-encoded in
/// CMD_DATA (never embedded in the script text, so quotes in it are safe) and
/// runs under `script(1)` on a pty sized exactly like our render window, so
/// programs wrap and format for what the picture shows and switch to
/// line-buffered streaming output. Child stdin comes from `input`: a fifo for
/// interactive runs (replies to the live message are typed into it), or
/// /dev/null for instant EOF. The fifo is opened read-write (`<>`) so the
/// command starts immediately with no writer present; reads then block until
/// someone types, and never see spurious EOF. Stdout/stderr stay on the pty.
/// Falls back to a plain shell when `script` is missing. Exit code via file.
pub(crate) fn build_runner(shell: &str, b64: &str, out_f: &str, code_f: &str, input: &str) -> String {
    use crate::termrender::{TERM_COLS, TERM_ROWS};
    format!(
        "export CMD_DATA=\"$(echo {b64} | base64 -d)\"; if command -v script >/dev/null 2>&1; then script -qec 'export TERM=xterm-256color; stty cols {cols} rows {rows} -echo; {shell} -c '\\''export PATH={path}; eval \"$CMD_DATA\"'\\'' <> {input}' /dev/null </dev/null; else {shell} -c 'export PATH={path}; eval \"$CMD_DATA\"' <> {input}; fi > {out_f} 2>&1; echo $? > {code_f}",
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
/// Shell snippet creating the input fifo owned by whoever will read it.
/// Root (the writer side) bypasses permission checks, so 600 is enough —
/// importantly the reader must own it, since opening read-write requires
/// write permission too.
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

/// Type one message into a live session: appends the text plus Enter to the
/// session fifo. The open blocks (up to the timeout) when nothing is reading,
/// which is how a finished session reports itself. The write runs as the
/// typist's linked account (root if unlinked), so it only succeeds on a
/// session they started themselves — typing into someone else's session is
/// refused rather than escalated. Returns true on delivery.
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

/// Best-effort purge of this bot's spool files and wrapper processes left
/// behind by a previous bot process (e.g. killed mid-run by a restart).
/// Only touches our own podbot-live-* names.
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

/// Load monospace font bytes (regular + bold) for terminal rendering.
fn load_terminal_fonts() -> Option<(Vec<u8>, Vec<u8>)> {
    let reg = crate::termrender::system_font_bytes("DejaVu Sans Mono")?;
    let bold = crate::termrender::system_font_bytes("DejaVu Sans Mono:weight=bold")
        .unwrap_or_else(|| reg.clone());
    Some((reg, bold))
}

/// Build the live message: `$ cmd` as plain text, full-page terminal
/// screenshot of the output underneath. Falls back to a fenced plain-text
/// block (with the header for context) when rendering is unavailable.
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
    // Fifo for typed input (replies to the live message). Without it the run
    // still works, just not interactively.
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
            // Stop the superseded guest tree too, or it spews into a deleted
            // file forever (invisible disk leak).
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
    // Remember the guest pid so a superseding run (or timeout) can stop the
    // whole process tree instead of orphaning it.
    {
        let mut m = live_map.lock().await;
        if let Some(e) = m.get_mut(&channel) {
            if e.tag == tag {
                e.pid = Some(pid);
            }
        }
    }
    let started = std::time::Instant::now();
    // Fonts load once per run; without them the whole run falls back to text.
    let fonts = match load_terminal_fonts() {
        Some((reg, bold)) => TermFonts::load(&reg, &bold),
        None => {
            eprintln!("live: no monospace font found (fc-match missing?); text fallback");
            None
        }
    };
    let mut first = true;
    // Render region locked by the first frame: same picture size for the
    // whole run (grows only if content outgrows it), so updates never jitter.
    let mut region: Option<(u32, u32)> = None;
    let mut last_hash: u64 = 0;
    let mut hashed_once = false;
    loop {
        let wait = if first { LIVE_QUICK_SECS } else { LIVE_POLL_SECS };
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        if started.elapsed().as_secs() > LIVE_TIMEOUT_SECS {
            // Runaway spew (megabytes of output) gets its tree stopped so it
            // can't fill the guest disk; ordinary long runs are left alone.
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
        // One generous fetch serves both the fallback text and the image frame.
        let fetched = guest_exec(&vm, "/usr/bin/tail", &["-c", LIVE_FRAME_BYTES, &out_f], true, 15)
            .await
            .map(|(_, o, _)| o)
            .unwrap_or_default();
        let fetched = if scrub_ip { scrub_public_ip(&fetched) } else { fetched };
        let done = guest_status(&vm, pid).await.unwrap_or(None);
        match done {
            Some(code) => {
                // Capped tail, not full cat: a runaway command could have
                // megabytes in the file; both consumers only keep the bottom.
                let full = guest_exec(&vm, "/usr/bin/tail", &["-c", "500000", &out_f], true, 30)
                    .await
                    .map(|(_, o, _)| o)
                    .unwrap_or(fetched);
                let full = if scrub_ip { scrub_public_ip(&full) } else { full };
                let header = format!("$ {}", cmd);
                // Exit status lives in the output itself (caption is bare `$ cmd`).
                let mut output = full.trim_end().to_string();
                if code != 0 {
                    output.push_str(&format!("\nexit {}", code));
                }
                if first {
                    // Fast command: plain truncated text, no image, no file.
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
                // Skip render + edit entirely when nothing changed: keeps the
                // 1s cadence cheap and stays clear of Discord rate limits.
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
                // Raw bytes: the emulator handles clears, redraws and
                // scrollback natively, so no text preprocessing here.
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
