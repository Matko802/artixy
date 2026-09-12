use poise::serenity_prelude as serenity;

use crate::{
    scrub::scrub_public_ip,
    util::{ansi_tail, codeblock, random_suffix, valid_runas},
    vm::{guest_exec, guest_launch_raw, guest_status},
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
pub(crate) const LIVE_POLL_SECS: u64 = 2;

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
            let _ = ack
                .channel_id
                .say(&http, codeblock("linked linux account is invalid; ask the owner to re-add you."))
                .await;
            return;
        }
    }
    let channel = ack.channel_id;
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
        live_run(http, ack, channel, tag, vm2, cmd2, runas2, out_f2, code_f2, live_map2, scrub_ip).await;
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
    mut msg: serenity::Message,
    channel: serenity::ChannelId,
    tag: u64,
    vm: String,
    cmd: String,
    runas: Option<String>,
    out_f: String,
    code_f: String,
    live_map: LiveMap,
    scrub_ip: bool,
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
        let tail = if scrub_ip { scrub_public_ip(&tail) } else { tail };
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
                    .edit(&http, serenity::EditMessage::new().content(ansi_tail(&body)))
                    .await;
                cleanup_live_files(&vm, &out_f, &code_f).await;
                break;
            }
            None => {
                body.push_str("\n\u{1b}[0;33m…live\u{1b}[0m");
                if msg
                    .edit(&http, serenity::EditMessage::new().content(ansi_tail(&body)))
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

