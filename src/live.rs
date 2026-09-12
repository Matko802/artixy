use poise::serenity_prelude as serenity;

use crate::{
    scrub::scrub_public_ip,
    util::{frame_text, plain_tail, random_suffix, valid_runas},
    vm::{guest_exec, guest_launch_raw, guest_status},
    webhook::{edit_posted, resolve_poster, Poster},
};

pub(crate) struct LiveEntry {
    pub(crate) handle: tokio::task::AbortHandle,
    pub(crate) tag: u64,
    pub(crate) out_f: String,
    pub(crate) code_f: String,
}
pub(crate) type LiveMap = std::sync::Arc<
    tokio::sync::Mutex<std::collections::HashMap<serenity::ChannelId, LiveEntry>>,
>;

pub(crate) const LIVE_TIMEOUT_SECS: u64 = 600;
pub(crate) const LIVE_POLL_SECS: u64 = 5;
/// Guest output fetched per poll for the image frame (bytes, not chars).
const LIVE_FRAME_BYTES: &str = "200000";

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

pub(crate) async fn cleanup_live_files(vm: &str, out_f: &str, code_f: &str) {
    let _ = guest_exec(vm, "/bin/rm", &["-f", out_f, code_f], false, 10).await;
}

/// Locate a monospace font for frame rendering.
async fn mono_font() -> Option<String> {
    for fam in ["DejaVu Sans Mono", "Liberation Mono"] {
        let out = tokio::process::Command::new("fc-match")
            .args([fam, "--format=%{file}"])
            .output()
            .await
            .ok()?;
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() && std::path::Path::new(&p).exists() {
                return Some(p);
            }
        }
    }
    None
}

fn esc_filter_arg(s: &str) -> String {
    s.replace('\\', "\\\\").replace(':', "\\:").replace(',', "\\,")
}

/// Render stripped terminal text to a PNG via ffmpeg drawtext.
/// Returns PNG bytes, or None when rendering is unavailable (caller falls back to text).
async fn render_frame(text: &str, w: u32, h: u32) -> Option<Vec<u8>> {
    let font = mono_font().await?;
    let tag = random_suffix();
    let dir = std::env::temp_dir();
    let txt = dir.join(format!("artixy-live-{}-{}.txt", std::process::id(), tag));
    let png = dir.join(format!("artixy-live-{}-{}.png", std::process::id(), tag));
    tokio::fs::write(&txt, text).await.ok()?;
    let input = format!("color=c=#0b0e14:s={}x{}", w, h);
    let vf = format!(
        "drawtext=fontfile={}:textfile={}:expansion=none:fontcolor=#e6e6e6:fontsize=16:x=10:y=10",
        esc_filter_arg(&font),
        esc_filter_arg(&txt.to_string_lossy()),
    );
    let png_s = png.to_string_lossy().to_string();
    let run = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::process::Command::new("ffmpeg")
            .args([
                "-y", "-v", "error", "-f", "lavfi", "-i", &input, "-vf", &vf, "-frames:v",
                "1", "-c:v", "png", &png_s,
            ])
            .output(),
    )
    .await;
    let ok = matches!(run, Ok(Ok(ref o)) if o.status.success());
    let _ = tokio::fs::remove_file(&txt).await;
    if !ok {
        let _ = tokio::fs::remove_file(&png).await;
        return None;
    }
    let bytes = tokio::fs::read(&png).await.ok()?;
    let _ = tokio::fs::remove_file(&png).await;
    if bytes.is_empty() { None } else { Some(bytes) }
}

/// Build the live message: short status text plus a rendered image of the
/// output. Falls back to plain text when rendering is unavailable.
async fn live_message(header: &str, output: &str) -> (String, Vec<(String, Vec<u8>)>) {
    let combined = if output.trim().is_empty() {
        header.to_string()
    } else {
        format!("{}\n{}", header, output.trim_end())
    };
    let (img_text, w, h) = frame_text(&combined);
    match render_frame(&img_text, w, h).await {
        Some(png) => (plain_tail(header), vec![("live.png".to_string(), png)]),
        None => (plain_tail(&combined), Vec::new()),
    }
}

pub(crate) async fn begin_live(
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
        live_run(http, ack, channel, tag, vm2, cmd2, runas2, out_f2, code_f2, live_map2, scrub_ip, poster).await;
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
    let script = format!(
        "echo {} | base64 -d | bash -c 'export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH; eval \"$(cat)\"' > {} 2>&1; echo $? > {}",
        b64, out_f, code_f
    );
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
            remove_live_if_tag(&live_map, channel, tag).await;
            return;
        }
    };
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(LIVE_POLL_SECS)).await;
        if started.elapsed().as_secs() > LIVE_TIMEOUT_SECS {
            edit_posted(
                &poster,
                &http,
                channel,
                msg.id,
                plain_tail(&format!(
                    "$ {}\n…stopped after {}s timeout; output truncated, process may still run in guest",
                    cmd, LIVE_TIMEOUT_SECS
                )),
                Vec::new(),
            )
            .await;
            cleanup_live_files(&vm, &out_f, &code_f).await;
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
                let full = guest_exec(&vm, "/bin/cat", &[&out_f], true, 30)
                    .await
                    .map(|(_, o, _)| o)
                    .unwrap_or(fetched);
                let full = if scrub_ip { scrub_public_ip(&full) } else { full };
                let mut output = full.trim_end().to_string();
                if code != 0 {
                    output.push_str(&format!("\nexit {}", code));
                }
                let header = format!("$ {}", cmd);
                let (text, files) = live_message(&header, &output).await;
                edit_posted(&poster, &http, channel, msg.id, text, files).await;
                cleanup_live_files(&vm, &out_f, &code_f).await;
                break;
            }
            None => {
                let header = format!("$ {}\n…live", cmd);
                let (text, files) = live_message(&header, fetched.trim_end()).await;
                if !edit_posted(&poster, &http, channel, msg.id, text, files).await {
                    cleanup_live_files(&vm, &out_f, &code_f).await;
                    break;
                }
            }
        }
    }
    remove_live_if_tag(&live_map, channel, tag).await;
}
