use poise::serenity_prelude as serenity;
use std::path::PathBuf;

use crate::{
    config::persist_runtime,
    live::begin_run,
    util::{attach_name, cap_file_body, codeblock, deployed_via_nix, project_dir, random_suffix, strip_sgr, valid_runas},
    vm::{agent_ping, guest_exec, linked_user, virsh, wait_agent},
    webhook::{is_own_message, mark_self_deleted, post_denied, post_message, post_response, post_text},
    Context, Error,
};

pub(crate) async fn is_authed(ctx: Context<'_>) -> bool {
    let id = ctx.author().id.get();
    let a = ctx.data().allowed.read().await;
    crate::config::access_allowed(a.owner, &a.users, &a.blocked, id)
}

pub(crate) async fn is_owner(ctx: Context<'_>) -> bool {
    ctx.author().id.get() == ctx.data().allowed.read().await.owner
}

pub(crate) async fn need_auth(ctx: Context<'_>) -> Result<bool, Error> {
    if is_authed(ctx).await {
        return Ok(true);
    }
    let u = ctx.author();
    eprintln!("denied: {} (id {})", u.name, u.id.get());
    post_denied(ctx, "Not authorized. Ask the owner to run `/user add @you`.")
        .await?;
    Ok(false)
}

pub(crate) async fn maybe_defer(ctx: Context<'_>) {
    if matches!(ctx, poise::Context::Application(_)) {
        let _ = ctx.defer().await;
    }
}

async fn require_vm(ctx: Context<'_>) -> Option<String> {
    let vm = ctx.data().vm.clone();
    if vm.trim().is_empty() {
        let _ = post_text(
            ctx,
            format!(
                "VM not configured — set `vm_name` in `{}` or `VM_NAME` env, then restart the bot.",
                crate::config::config_file_path().display()
            ),
        )
        .await;
        return None;
    }
    Some(vm)
}

pub(crate) const HELP: &str = "\
**Who needs help? its ez :3** Everything acts on the one hardcoded VM, no names needed. Only the owner + added users can use me. Slash commands only.\n\
\n**VM**\n`/ps` — state of the VM\n`/status` — quick state + agent check\n`/start` — power on + wait for guest agent\n`/stop` — graceful shutdown\n`/restart` — reboot\n`/info` — details + agent status\n\
\n**Who can use me**\n`/user` — one command: `/user list` shows owner + managers, `/user add @user` (owner only) links them and creates their Linux account in Artix, `/user remove @user` (owner only) revokes bot access and deletes their Linux account in the VM\n`/shell [fish|bash]` — your shell interpreter (default bash)\n`/notify <channel-id>` or `/notify off` — owner only: where I post my boot message, unset means silent\n`/purge_replies <user-id> [limit]` — owner only: delete their replies to my messages here\n`/warmode <true|false>` — owner only: arm or stand down the protections\n`/run <command>` — run it for real inside the VM, prints the output. Quick commands answer with plain text, long ones switch to a live image feed on their own, updating about every second.\n
\n**Run real commands in Artix**\n`/run <command>` — runs it for real inside the VM through the guest agent and prints the output. e.g. `/run sudo pacman -Syu`, `/run ls -la`. Runs as YOUR linked linux account (`whoami` proves it). Reply to its live message to type into the running command (type text, `;return` `;space` `;enter` `;esc` `;up` `;down` `;left` `;right` `;ctrl+w` send keys, add a number like `;right 5` to repeat).\n`/shot` — screenshot of the host screen, uploaded here\n`/send <path>` — upload a host file here (absolute path, ~20MB max)\n`/sayas [message] [reply_to] [file] [file2] [file3]` — owner only: `no args` toggles auto say-as-artix mode, `message` and/or attached files send as artix (reply_to = message ID/link). Files attached to the slash command (or to the `;sayas` prefix message) are re-uploaded as artix. Output is ephemeral (only you see it).\n\
\n**Warning:** managers can power this machine on/off. Keep the token secret: it lives only in `.env`, never in git.";

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn help(ctx: Context<'_>) -> Result<(), Error> {
    post_text(ctx, HELP).await?;
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn ps(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    match virsh(&["list", "--all"]).await {
        Ok(o) => {
            post_text(ctx, codeblock(&o)).await?;
        }
        Err(e) => {
            post_text(ctx, codeblock(&e.to_string())).await?;
        }
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn start(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    let Some(vm) = require_vm(ctx).await else { return Ok(()); };
    let state = virsh(&["domstate", &vm]).await.unwrap_or_default();
    let mut started_here = false;
    let mut boot_t0: Option<std::time::Instant> = None;
    if state.trim() == "running" {
        post_text(ctx, format!(
            "`{}` is already on. Waiting for the guest agent…",
            vm
        ))
        .await?;
    } else {
        match virsh(&["start", &vm]).await {
            Ok(_) => {
                started_here = true;
                boot_t0 = Some(std::time::Instant::now());
                post_text(ctx, format!(
                    "`{}` starting. Waiting for the guest agent…",
                    vm
                ))
                .await?;
            }
            Err(e) => {
                post_text(ctx, codeblock(&e.to_string())).await?;
                return Ok(());
            }
        }
    }
    if wait_agent(&vm, 90).await {
        if started_here {
            let secs = boot_t0.map(|t| t.elapsed().as_secs()).unwrap_or(0);
            let took = if secs >= 60 {
                format!("{}m {}s", secs / 60, secs % 60)
            } else {
                format!("{}s", secs)
            };
            post_text(ctx, format!("{} booted in {}.", vm, took)).await?;
        } else {
            post_text(ctx, format!(
                "`{}` is on and the guest agent answers.",
                vm
            ))
            .await?;
        }
    } else {
        post_text(ctx, "Artix bot is on /start to boot artix").await?;
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    let Some(vm) = require_vm(ctx).await else { return Ok(()); };
    post_text(ctx, format!("stopping {}…", vm)).await?;
    match virsh(&["shutdown", &vm]).await {
        Ok(_) => {}
        Err(e) => {
            post_text(ctx, codeblock(&e.to_string())).await?;
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
            post_text(ctx, format!("{} has stopped.", vm)).await?;
            return Ok(());
        }
    }
    post_text(ctx, format!("{} is still stopping — check `;status`.", vm))
        .await?;
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn restart(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let Some(vm) = require_vm(ctx).await else { return Ok(()); };
    match virsh(&["reboot", &vm]).await {
        Ok(_) => {
            post_text(ctx, format!("`{}` rebooting.", vm)).await?;
        }
        Err(e) => {
            post_text(ctx, codeblock(&e.to_string())).await?;
        }
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn info(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let Some(vm) = require_vm(ctx).await else { return Ok(()); };
    match virsh(&["dominfo", &vm]).await {
        Ok(o) => {
            let agent = if agent_ping(&vm).await {
                "guest agent: up"
            } else {
                "guest agent: DOWN (install qemu-guest-agent in Artix)"
            };
            post_text(ctx, codeblock(&format!("{}\n{}", o, agent))).await?;
        }
        Err(e) => {
            post_text(ctx, codeblock(&e.to_string())).await?;
        }
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn shell(
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
            post_text(ctx, format!(
                "Your shell: `{}`. Change with `/shell fish` or `/shell bash`.",
                cur.get(&key).map(|s| s.as_str()).unwrap_or("bash")
            ))
            .await?;
        }
        Some(n) => {
            let n = n.trim().to_lowercase();
            if n != "fish" && n != "bash" {
                post_text(ctx, "Only `fish` or `bash`.").await?;
                return Ok(());
            }
            {
                let mut m = ctx.data().shells.write().await;
                m.insert(key, n.clone());
            }
            persist_runtime(ctx.data()).await?;
            post_text(ctx, format!("Your shell is now `{}`.", n)).await?;
        }
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn botrestart(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    if deployed_via_nix() {
        post_text(ctx,
            "Deployed from Nix — I can't re-exec myself out of a read-only \
             `/nix/store`. Restart with `systemctl --user restart artixy`.",
        )
        .await?;
        return Ok(());
    }
    post_text(ctx, "Restarting…").await?;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("target/debug/artixy"));
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
            post_text(ctx, codeblock(&format!("restart failed to spawn: {}", e)))
                .await?;
            Ok(())
        }
    }
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn run(
    ctx: Context<'_>,
    #[description = "Command to run in the VM"] cmd: String,
) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    if cmd.trim().is_empty() {
        post_text(ctx, "Usage: `/run <command>`.").await?;
        return Ok(());
    }
    maybe_defer(ctx).await;
    let Some(vm) = require_vm(ctx).await else { return Ok(()); };
    if !agent_ping(&vm).await {
        post_text(ctx, "Guest agent is silent. Install `qemu-guest-agent` in Artix first.")
            .await?;
        return Ok(());
    }
    let http = ctx.serenity_context().http.clone();
    let ack = post_response(ctx, format!("`run: {}` starting…", cmd.trim()), Vec::new()).await?;
    begin_run(
        http,
        ack,
        ctx.author().id.get(),
        &ctx.author().name,
        vm,
        cmd.trim().to_string(),
        linked_user(ctx.data(), ctx.author().id.get()).await,
        ctx.data().live.clone(),
        !is_owner(ctx).await,
    )
    .await;
    Ok(())
}

pub(crate) fn parse_message_ref(s: &str, current_channel: u64) -> Option<(u64, u64)> {
    let t = s.trim().trim_matches(|c| c == '<' || c == '>').trim();
    let t = t.split('?').next().unwrap_or(t).trim();
    if let Some((_, rest)) = t.split_once("/channels/") {
        let mut parts = rest.split('/');
        let _guild = parts.next()?;
        let channel = parts.next()?.parse::<u64>().ok()?;
        let msg = parts.next()?.parse::<u64>().ok()?;
        if parts.next().is_some() || channel == 0 || msg == 0 {
            return None;
        }
        return Some((channel, msg));
    }
    let id = t.parse::<u64>().ok()?;
    if id == 0 {
        return None;
    }
    Some((current_channel, id))
}

/// Discord-side cap for a single re-uploaded attachment (~25MB).
pub(crate) const SAYAS_MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;

pub(crate) fn safe_attach_name(raw: &str) -> String {
    let base = std::path::Path::new(raw)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect();
    let clean = clean.trim_matches('.').to_string();
    if clean.is_empty() {
        "file.bin".to_string()
    } else {
        clean.chars().take(100).collect()
    }
}

pub(crate) async fn download_sayas_files(
    attachments: &[serenity::Attachment],
) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
    let mut files = Vec::new();
    let mut failed = Vec::new();
    for a in attachments {
        if a.size as u64 > SAYAS_MAX_FILE_BYTES {
            failed.push(format!("`{}` is over ~25MB — skipped.", a.filename));
            continue;
        }
        match a.download().await {
            Ok(bytes) => {
                if bytes.len() as u64 > SAYAS_MAX_FILE_BYTES {
                    failed.push(format!("`{}` is over ~25MB — skipped.", a.filename));
                    continue;
                }
                files.push((safe_attach_name(&a.filename), bytes));
            }
            Err(e) => {
                failed.push(format!("`{}` download failed: {}", a.filename, e));
            }
        }
    }
    (files, failed)
}

async fn sayas_toggle(ctx: Context<'_>) -> Result<(), Error> {
    let mut s = ctx.data().settings.write().await;
    s.sayas_enabled = !s.sayas_enabled;
    let enabled = s.sayas_enabled;
    drop(s);
    persist_runtime(ctx.data()).await?;
    let msg = if enabled {
        "Say-as-artix: **enabled** — your messages will now be sent as artix (toggle again to disable)."
    } else {
        "Say-as-artix: **disabled**."
    };
    if matches!(ctx, poise::Context::Application(_)) {
        let _ = ctx
            .send(poise::CreateReply::default().content(msg).ephemeral(true))
            .await;
    } else {
        post_text(ctx, msg).await?;
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn sayas(
    ctx: Context<'_>,
    #[description = "Text to send as artix (leave empty to toggle auto mode)"] message: Option<String>,
    #[description = "Message ID or link to reply to"] reply_to: Option<String>,
    #[description = "File to send as artix"] file: Option<serenity::Attachment>,
    #[description = "Extra file to send as artix"] file2: Option<serenity::Attachment>,
    #[description = "Extra file to send as artix"] file3: Option<serenity::Attachment>,
) -> Result<(), Error> {
    if !is_owner(ctx).await {
        if matches!(ctx, poise::Context::Application(_)) {
            let _ = ctx
                .send(poise::CreateReply::default().content("Owner only.").ephemeral(true))
                .await;
        } else {
            post_denied(ctx, "Owner only.").await?;
        }
        return Ok(());
    }
    // Attachments supplied either as slash options or stuck onto the `;sayas`
    // prefix message itself. Presence (not download success) decides whether
    // an otherwise-empty invocation toggles auto mode or sends files.
    let mut pending: Vec<serenity::Attachment> = Vec::new();
    for f in [file, file2, file3].into_iter().flatten() {
        pending.push(f);
    }
    let mut prefix_files: Vec<serenity::Attachment> = Vec::new();
    if let poise::Context::Prefix(pctx) = ctx {
        prefix_files = pctx.msg.attachments.clone();
    }
    let has_files = !pending.is_empty() || !prefix_files.is_empty();
    let raw_text = message.unwrap_or_default();
    let text = raw_text.trim_end().to_string();
    if text.trim().is_empty() && !has_files {
        return sayas_toggle(ctx).await;
    }
    let is_slash = matches!(ctx, poise::Context::Application(_));
    if is_slash {
        let _ = ctx.defer_ephemeral().await;
    } else {
        maybe_defer(ctx).await;
    }
    let http = ctx.serenity_context().http.clone();
    let channel = ctx.channel_id();
    // Grab the bytes *before* deleting the prefix trigger so a `;sayas`
    // message with uploads still forwards them.
    let (mut files, mut problems) = download_sayas_files(&pending).await;
    if !prefix_files.is_empty() {
        let (mut pf, mut pp) = download_sayas_files(&prefix_files).await;
        files.append(&mut pf);
        problems.append(&mut pp);
    }
    // Long text can't ride in message content — ship it as a .txt sidecar
    // alongside any user uploads.
    let mut body = text.clone();
    if body.chars().count() > 2000 {
        files.insert(
            0,
            (
                attach_name(&body),
                cap_file_body(&strip_sgr(&body)).into_bytes(),
            ),
        );
        body = String::new();
    }
    if let poise::Context::Prefix(pctx) = ctx {
        let _ = pctx.msg.delete(&http).await;
    }
    if body.is_empty() && files.is_empty() {
        let note = if problems.is_empty() {
            "Nothing to send — attach a file or type a message.".to_string()
        } else {
            format!("Nothing to send — {} ", problems.join(" "))
        };
        if is_slash {
            let _ = ctx
                .send(poise::CreateReply::default().content(note).ephemeral(true))
                .await;
        } else {
            post_text(ctx, note).await?;
        }
        return Ok(());
    }
    let send_res: Result<(), Error> = async {
        if let Some(target) = reply_to {
            let Some((ch_id, msg_id)) = parse_message_ref(&target, channel.get()) else {
                if is_slash {
                    let _ = ctx
                        .send(
                            poise::CreateReply::default()
                                .content("Couldn't read that reply target — give a message ID or a full message link.")
                                .ephemeral(true),
                        )
                        .await;
                } else {
                    post_text(ctx, "Couldn't read that reply target — give a message ID or a full message link.").await?;
                }
                return Ok(());
            };
            let ch = serenity::ChannelId::new(ch_id);
            let target_msg = match ch.message(&http, serenity::MessageId::new(msg_id)).await {
                Ok(m) => m,
                Err(_) => {
                    if is_slash {
                        let _ = ctx
                            .send(
                                poise::CreateReply::default()
                                    .content("Couldn't fetch that message (wrong channel, or I can't see it).")
                                    .ephemeral(true),
                            )
                            .await;
                    } else {
                        post_text(ctx, "Couldn't fetch that message (wrong channel, or I can't see it).").await?;
                    }
                    return Ok(());
                }
            };
            let mut builder =
                serenity::CreateMessage::new().reference_message((ch, target_msg.id));
            if !body.is_empty() {
                builder = builder.content(&body);
            }
            for (name, bytes) in &files {
                builder =
                    builder.add_file(serenity::CreateAttachment::bytes(bytes.clone(), name.clone()));
            }
            if ch.send_message(&http, builder).await.is_err() {
                if is_slash {
                    let _ = ctx
                        .send(
                            poise::CreateReply::default()
                                .content("Reply failed (missing permission?).")
                                .ephemeral(true),
                        )
                        .await;
                } else {
                    post_text(ctx, "Reply failed (missing permission?).").await?;
                }
            }
        } else if body.chars().count() <= 2000 {
            let _ = post_message(&http, channel, body.clone(), files.clone()).await;
        } else {
            let att = (attach_name(&body), cap_file_body(&strip_sgr(&body)).into_bytes());
            let mut all = vec![att];
            all.extend(files.clone());
            let _ = post_message(&http, channel, String::new(), all).await;
        }
        Ok(())
    }
    .await;
    if is_slash {
        let mut confirm = format!("Sent as artix in <#{}>.", channel.get());
        if !files.is_empty() {
            confirm.push_str(&format!(" ({} file{})", files.len(), if files.len() == 1 { "" } else { "s" }));
        }
        if !problems.is_empty() {
            confirm.push_str(&format!("\n{}", problems.join("\n")));
        }
        let _ = ctx
            .send(poise::CreateReply::default().content(confirm).ephemeral(true))
            .await;
        let _ = send_res;
    } else {
        if !problems.is_empty() {
            let _ = post_text(ctx, problems.join("\n")).await;
        }
        send_res?;
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn shot(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    let path = format!("/tmp/artixy-shot-{}-{}.png", std::process::id(), random_suffix());
    let out = tokio::process::Command::new("grim")
        .arg(&path)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "shot.png".into());
                    if post_response(ctx, String::new(), vec![(name, bytes)]).await.is_err() {
                        eprintln!("shot send failed (missing Attach Files permission?)");
                        post_text(ctx, "Screenshot captured but I can't attach files here — give me the Attach Files permission.").await?;
                    }
                }
                Err(e) => {
                    post_text(ctx, codeblock(&format!("attach failed: {}", e))).await?;
                }
            }
            let _ = tokio::fs::remove_file(&path).await;
        }
        _ => {
            post_text(ctx, "grim failed (are you in a Wayland session?).").await?;
        }
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn send(
    ctx: Context<'_>,
    #[description = "Absolute path of a file inside the bot's project dir (max ~20MB)"] path: String,
) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    let p = path.trim();
    if !p.starts_with('/') {
        post_text(ctx, "Absolute path only.").await?;
        return Ok(());
    }
    let root = match tokio::fs::canonicalize(project_dir()).await {
        Ok(r) => r,
        Err(e) => {
            post_text(ctx, codeblock(&format!("can't resolve project dir: {}", e)))
                .await?;
            return Ok(());
        }
    };
    let target = match tokio::fs::canonicalize(p).await {
        Ok(t) => t,
        Err(_) => {
            post_text(ctx, "No readable file there (must exist, absolute path, under the project dir, ~20MB max).")
                .await?;
            return Ok(());
        }
    };
    if !target.starts_with(&root) {
        post_text(ctx, "That path is outside the bot's project dir — not sending it.")
            .await?;
        return Ok(());
    }
    match tokio::fs::metadata(&target).await {
        Ok(m) if m.is_file() && m.len() < 20 * 1024 * 1024 => {
            match tokio::fs::read(&target).await {
                Ok(bytes) => {
                    let name = std::path::Path::new(&target)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "file.bin".into());
                    if post_response(ctx, String::new(), vec![(name, bytes)]).await.is_err() {
                        eprintln!("send failed (missing Attach Files permission?)");
                        post_text(ctx, "File read but I can't attach files here — give me the Attach Files permission.").await?;
                    }
                }
                Err(e) => {
                    post_text(ctx, codeblock(&format!("attach failed: {}", e))).await?;
                }
            }
        }
        _ => {
            post_text(ctx, "No readable file there (absolute path under the project dir, ~20MB max).")
                .await?;
        }
    }
    Ok(())
}

pub(crate) fn sh_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

async fn guest_mkdir(vm: &str, dir: &str) -> Result<(), Error> {
    let rc = guest_exec(vm, "/bin/mkdir", &["-p", "--", dir], false, 15).await;
    let rc = match rc {
        Err(e) if e.to_string().contains("No such file") => {
            guest_exec(vm, "/usr/bin/mkdir", &["-p", "--", dir], false, 15).await?
        }
        other => other?,
    };
    if rc.0 == 0 {
        Ok(())
    } else {
        Err(format!("mkdir failed (code {})", rc.0).into())
    }
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn upload(
    ctx: Context<'_>,
    #[description = "File to upload into the VM"] file: serenity::Attachment,
    #[description = "Absolute destination dir in the VM (created if missing)"] dir: String,
) -> Result<(), Error> {
    use base64::Engine as _;
    if !need_auth(ctx).await? {
        return Ok(());
    }
    maybe_defer(ctx).await;
    if file.size as u64 > 20 * 1024 * 1024 {
        post_text(ctx, "That file is over ~20MB — too big to upload.").await?;
        return Ok(());
    }
    let dir = dir.trim();
    if !dir.starts_with('/') {
        post_text(ctx, "Absolute dir only.").await?;
        return Ok(());
    }
    let Some(name) = std::path::Path::new(&file.filename)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && n != "." && n != "..")
    else {
        post_text(ctx, "Bad file name.").await?;
        return Ok(());
    };
    let Some(vm) = require_vm(ctx).await else { return Ok(()); };
    let bytes = match file.download().await {
        Ok(b) => b,
        Err(e) => {
            post_text(ctx, codeblock(&format!("download failed: {}", e))).await?;
            return Ok(());
        }
    };
    if bytes.len() as u64 > 20 * 1024 * 1024 {
        post_text(ctx, "That file is over ~20MB — too big to upload.").await?;
        return Ok(());
    }
    if let Err(e) = guest_mkdir(&vm, dir).await {
        post_text(ctx, codeblock(&format!("mkdir failed: {}", e))).await?;
        return Ok(());
    }
    let dest = format!("{}/{}", dir.trim_end_matches('/'), name);
    let tmp = format!("/tmp/artixy-up-{}.b64", random_suffix());
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mut first = true;
    for piece in b64.as_bytes().chunks(512 * 1024) {
        let piece = String::from_utf8_lossy(piece).into_owned();
        let op = if first { ">" } else { ">>" };
        first = false;
        let script = format!("printf '%s' '{piece}' {op} {}", sh_escape(&tmp));
        let rc = guest_exec(&vm, "/bin/bash", &["-c", &script], false, 30).await;
        let (code, _, _) = match rc {
            Err(e) if e.to_string().contains("No such file") => {
                guest_exec(&vm, "/bin/sh", &["-c", &script], false, 30).await?
            }
            other => other?,
        };
        if code != 0 {
            let _ = guest_exec(&vm, "/bin/rm", &["-f", &tmp], false, 10).await;
            post_text(ctx, codeblock(&format!("upload failed (code {})", code))).await?;
            return Ok(());
        }
    }
    let script = format!(
        "base64 -d {} > {} && rm -f {} && wc -c < {}",
        sh_escape(&tmp),
        sh_escape(&dest),
        sh_escape(&tmp),
        sh_escape(&dest)
    );
    let rc = guest_exec(&vm, "/bin/bash", &["-c", &script], true, 60).await;
    let (code, out, _) = match rc {
        Err(e) if e.to_string().contains("No such file") => {
            guest_exec(&vm, "/bin/sh", &["-c", &script], true, 60).await?
        }
        other => other?,
    };
    if code != 0 {
        let _ = guest_exec(&vm, "/bin/rm", &["-f", &tmp], false, 10).await;
        post_text(ctx, codeblock(&format!("decode failed (code {})", code))).await?;
        return Ok(());
    }
    let landed: u64 = out.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
    if landed != bytes.len() as u64 {
        post_text(ctx, codeblock(&format!("size mismatch: sent {} but landed {}", bytes.len(), landed))).await?;
        return Ok(());
    }
    let mut suffix = String::new();
    if let Some(u) = linked_user(ctx.data(), ctx.author().id.get()).await.filter(|u| valid_runas(u)) {
        let rc = guest_exec(&vm, "/usr/bin/chown", &[&format!("{u}:"), &dest], false, 15).await;
        let rc = match rc {
            Err(e) if e.to_string().contains("No such file") => {
                guest_exec(&vm, "/bin/chown", &[&format!("{u}:"), &dest], false, 15).await
            }
            other => other,
        };
        if !matches!(rc, Ok((0, _, _))) {
            suffix = " (root-owned, use sudo)".to_string();
        }
    }
    post_text(ctx, format!("uploaded `{}` ({} bytes) to `{}`{}.", name, bytes.len(), dest, suffix)).await?;
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn status(ctx: Context<'_>) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    let Some(vm) = require_vm(ctx).await else { return Ok(()); };
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
    post_text(ctx, format!("`{}`: {} | {}", vm, state.trim(), agent))
        .await?;
    Ok(())
}

pub(crate) const BOOT_ART: &str = r"          .        :-------:
        ^/ \^      :Im here:
        ●   ●     <:-------:
       /  ω  \
      /_/   \_\";

pub(crate) fn parse_channel(s: &str) -> Option<u64> {
    s.trim().parse::<u64>().ok().filter(|id| *id != 0)
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn notify(
    ctx: Context<'_>,
    #[description = "channel ID for boot messages, or off"] what: Option<String>,
) -> Result<(), Error> {
    if !is_owner(ctx).await {
        post_denied(ctx, "Owner only.").await?;
        return Ok(());
    }
    match what.as_deref().map(str::trim) {
        None => {
            let s = ctx.data().settings.read().await;
            match s.notify_channel {
                Some(id) => {
                    post_text(ctx, format!("Boot messages go to <#{}>.", id)).await?;
                }
                None => {
                    post_text(ctx, "Boot messages are OFF (no channel set).").await?;
                }
            }
        }
        Some(v) if v.eq_ignore_ascii_case("off") => {
            ctx.data().settings.write().await.notify_channel = None;
            persist_runtime(ctx.data()).await?;
            post_text(ctx, "Boot messages OFF.").await?;
        }
        Some(v) => match parse_channel(v) {
            Some(id) => {
                ctx.data().settings.write().await.notify_channel = Some(id);
                persist_runtime(ctx.data()).await?;
                post_text(ctx, format!("Boot messages will go to `<#{id}>`.\n```\n{BOOT_ART}\n```")).await?;
            }
            None => {
                post_text(ctx, "Usage: `/notify <channel-id>` or `/notify off`.").await?;
            }
        },
    }
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn warmode(
    ctx: Context<'_>,
    #[description = "true to arm protections, false to stand down"] enabled: bool,
) -> Result<(), Error> {
    if !is_owner(ctx).await {
        post_denied(ctx, "Owner only.").await?;
        return Ok(());
    }
    ctx.data().settings.write().await.war_mode = enabled;
    persist_runtime(ctx.data()).await?;
    if enabled {
        post_text(ctx, "War mode on! >:3").await?;
        return Ok(());
    }
    post_response(
        ctx,
        "war mode disabled, peace?".to_string(),
        vec![(
            "lapeace.jpg".to_string(),
            include_bytes!("../imgs/lapeace.jpg").to_vec(),
        )],
    )
    .await?;
    Ok(())
}

pub(crate) fn parse_target_id(s: &str) -> Option<u64> {
    let t = s.trim();
    let inner = t
        .strip_prefix("<@")
        .and_then(|r| r.strip_suffix('>'))
        .map(|r| r.strip_prefix('!').unwrap_or(r))
        .unwrap_or(t);
    inner.parse::<u64>().ok().filter(|id| *id != 0)
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn purge_replies(
    ctx: Context<'_>,
    #[description = "User/bot ID whose replies to my messages get deleted"] target: String,
    #[description = "How many recent messages to scan (default 50, max 100)"] limit: Option<u8>,
) -> Result<(), Error> {
    if !is_owner(ctx).await {
        post_denied(ctx, "Owner only.").await?;
        return Ok(());
    }
    let Some(target_id) = parse_target_id(&target) else {
        post_text(ctx, "Usage: `/purge_replies <user-id> [limit]`.").await?;
        return Ok(());
    };
    maybe_defer(ctx).await;
    let http = ctx.serenity_context().http.clone();
    let bot_id = match http.get_current_user().await {
        Ok(u) => u.id,
        Err(e) => {
            post_text(ctx, codeblock(&format!("could not learn my own id: {}", e))).await?;
            return Ok(());
        }
    };
    let channel = ctx.channel_id();
    let n = limit.unwrap_or(50).clamp(1, 100);
    let mut msgs = match channel.messages(&http, serenity::GetMessages::new().limit(n)).await {
        Ok(m) => m,
        Err(e) => {
            post_text(ctx, codeblock(&format!("could not read channel history: {}", e))).await?;
            return Ok(());
        }
    };
    let mut thread_count = 0u32;
    if let Some(guild_id) = ctx.guild_id() {
        if let Ok(active) = guild_id.get_active_threads(&http).await {
            for t in active.threads.iter().filter(|t| t.parent_id == Some(channel)).take(5) {
                if let Ok(ms) = t.id.messages(&http, serenity::GetMessages::new().limit(n)).await {
                    thread_count += 1;
                    msgs.extend(ms);
                }
            }
        }
    }
    let scanned = msgs.len() as u32;
    let mut authored = 0u32;
    let mut deleted = 0u32;
    let mut failed = 0u32;
    for m in &msgs {
        if m.author.id.get() != target_id {
            continue;
        }
        authored += 1;
        let replied_to_me = match &m.referenced_message {
            Some(r) => is_own_message(r.author.id, r.id, bot_id),
            None => match &m.message_reference {
                Some(r) => match r.message_id {
                    Some(mid) => match channel.message(&http, mid).await {
                        Ok(orig) => is_own_message(orig.author.id, orig.id, bot_id),
                        Err(_) => false,
                    },
                    None => false,
                },
                None => false,
            },
        };
        if !replied_to_me {
            continue;
        }
        match m.delete(&http).await {
            Ok(_) => {
                mark_self_deleted(m.id);
                deleted += 1;
            }
            Err(e) => {
                failed += 1;
                eprintln!("purge_replies: failed to delete {}: {}", m.id.get(), e);
            }
        }
    }
    post_text(ctx, format!(
        "Scanned {} recent messages ({} threads), `<@{target_id}>` authored {}, deleted {} replies to my messages, {} deletes failed.",
        scanned, thread_count, authored, deleted, failed
    ))
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, poise::ChoiceParameter)]
pub(crate) enum UserAction {
    #[name = "add"]
    Add,
    #[name = "list"]
    List,
    #[name = "remove"]
    Remove,
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn user(
    ctx: Context<'_>,
    #[description = "What to do"] action: UserAction,
    #[description = "User for add/remove"] user: Option<serenity::User>,
) -> Result<(), Error> {
    match (action, user) {
        (UserAction::Add, Some(u)) => do_useradd(ctx, &u).await,
        (UserAction::Add, None) => {
            post_text(ctx, "Pick a user: `/user action:add user:@user`.").await?;
            Ok(())
        }
        (UserAction::Remove, Some(u)) => do_userdel(ctx, &u).await,
        (UserAction::Remove, None) => {
            post_text(ctx, "Pick a user: `/user action:remove user:@user`.").await?;
            Ok(())
        }
        (UserAction::List, _) => do_users(ctx).await,
    }
}

pub(crate) async fn uname(http: &serenity::Http, uid: u64) -> String {
    serenity::UserId::new(uid)
        .to_user(http)
        .await
        .map(|u| u.name.clone())
        .unwrap_or_else(|_| uid.to_string())
}

pub(crate) async fn do_users(ctx: Context<'_>) -> Result<(), Error> {
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
        msg.push_str(" none yet — owner runs `/user add @user`");
    } else {
        for (u, n) in pairs {
            let name = uname(ctx.http(), u).await;
            match n {
                Some(n) => msg.push_str(&format!("\n`{}` → `{}`", name, n)),
                None => msg.push_str(&format!("\n`{}` → (no linux account)", name)),
            }
        }
    }
    post_text(ctx, msg).await?;
    Ok(())
}

pub(crate) fn sanitize_discord_name(s: &str) -> Option<String> {
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

/// Bash snippet (run as root in the guest) that grants `user` passwordless
/// sudo. Idempotent: safe to re-run for new and already-linked accounts.
/// Returns `None` for invalid names (including `root`) so we never write a
/// sudoers file with junk in it.
pub(crate) fn sudoers_script(user: &str) -> Option<String> {
    if !valid_runas(user) {
        return None;
    }
    // valid_runas() only allows [a-z0-9_-] starting with [a-z_], so
    // interpolating into single quotes below cannot break out.
    Some(format!(
        "set -eu; u='{u}'; f=\"/etc/sudoers.d/$u\"; \
        printf '%s ALL=(ALL) NOPASSWD: ALL\\n' \"$u\" >\"$f.tmp\"; \
        printf 'Defaults:%s !requiretty\\n' \"$u\" >>\"$f.tmp\"; \
        chmod 0440 \"$f.tmp\"; mv -f \"$f.tmp\" \"$f\"; chmod 0440 \"$f\"; \
        if command -v visudo >/dev/null 2>&1; then visudo -c -f \"$f\" >/dev/null; fi; \
        if getent group wheel >/dev/null 2>&1; then usermod -aG wheel \"$u\" || true; \
        elif getent group sudo >/dev/null 2>&1; then usermod -aG sudo \"$u\" || true; fi; \
        h=\"$(getent passwd \"$u\" | cut -d: -f6)\"; \
        if [ -n \"$h\" ] && [ -d \"$h\" ]; then chown \"$u\" \"$h\" 2>/dev/null || true; \
        for d in \"$h/.cargo\" \"$h/.rustup\"; do [ -e \"$d\" ] && chown -R \"$u\" \"$d\" 2>/dev/null || true; done; fi; true",
        u = user,
    ))
}

pub(crate) async fn ensure_passwordless_sudo(vm: &str, user: &str) -> Result<(), Error> {
    let Some(script) = sudoers_script(user) else {
        return Err(format!("refusing sudo for invalid linux name `{user}`").into());
    };
    let rc = guest_exec(vm, "/bin/bash", &["-c", &script], false, 15).await;
    let (code, _, _) = match rc {
        Err(e) if e.to_string().contains("No such file") => {
            guest_exec(vm, "/bin/sh", &["-c", &script], false, 15).await?
        }
        other => other?,
    };
    if code == 0 {
        Ok(())
    } else {
        Err(format!("sudo setup for `{user}` failed (code {code})").into())
    }
}

pub(crate) async fn do_useradd(ctx: Context<'_>, user: &serenity::User) -> Result<(), Error> {
    if !is_owner(ctx).await {
        post_denied(ctx, "Owner only.").await?;
        return Ok(());
    }
    maybe_defer(ctx).await;
    let uid = user.id.get();
    let name = sanitize_discord_name(&user.name).unwrap_or_else(|| format!("u{}", uid));
    {
        let mut a = ctx.data().allowed.write().await;
        if a.owner != uid && !a.users.contains(&uid) {
            a.users.push(uid);
            drop(a);
            persist_runtime(ctx.data()).await?;
        }
    }
    let Some(vm) = require_vm(ctx).await else { return Ok(()); };
    let state = virsh(&["domstate", &vm]).await.unwrap_or_default();
    if state.trim() != "running" {
        post_text(ctx, format!(
            "Authorized `{}` in the bot, but `{}` is off — run `/start` first, then rerun `/user add @user` to create their Linux account.",
            uid, vm
        ))
        .await?;
        return Ok(());
    }
    if !wait_agent(&vm, 60).await {
            post_text(ctx, format!("Authorized `{}` in the bot, but the guest agent is silent — no Linux account created. Install `qemu-guest-agent` in Artix, then rerun `/user add @user`.", uid)).await?;
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
            post_text(ctx, format!("Linux user `{}` already exists, linking it.", name)).await?;
        }
        Ok((c, _, _)) => {
            post_text(ctx, format!(
                "Authorized `{}` in the bot, but `useradd` in the VM failed (code {}).",
                uid, c
            ))
            .await?;
            return Ok(());
        }
        Err(e) => {
            post_text(ctx, format!(
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
    }
    persist_runtime(ctx.data()).await?;
    // Grant passwordless sudo to the new account plus every other linked
    // account, so `sudo` stops complaining about missing permissions for
    // anyone the bot manages (new users AND pre-existing ones).
    let targets: Vec<String> = {
        let a = ctx.data().allowed.read().await;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for n in std::iter::once(&name).chain(a.linux.values()) {
            if valid_runas(n) && seen.insert(n.clone()) {
                out.push(n.clone());
            }
        }
        out
    };
    let mut sudo_failed = Vec::new();
    for t in &targets {
        if let Err(e) = ensure_passwordless_sudo(&vm, t).await {
            eprintln!("sudo setup for `{t}` failed: {e}");
            sudo_failed.push(t.clone());
        }
    }
    if sudo_failed.is_empty() {
        post_text(ctx, format!(
            "added user \"{}\" linked to `{}` — account created, passwordless sudo enabled.",
            name, uid
        ))
        .await?;
    } else {
        post_text(ctx, format!(
            "added user \"{}\" linked to `{}` — account created, but passwordless sudo failed for: `{}`. Re-run `/user add @user` once the guest is healthy.",
            name,
            uid,
            sudo_failed.join("`, `")
        ))
        .await?;
    }
    Ok(())
}

pub(crate) async fn do_userdel(ctx: Context<'_>, user: &serenity::User) -> Result<(), Error> {
    if !is_owner(ctx).await {
        post_denied(ctx, "Owner only.").await?;
        return Ok(());
    }
    maybe_defer(ctx).await;
    let uid = user.id.get();
    let mut a = ctx.data().allowed.write().await;
    if let Some(i) = a.users.iter().position(|u| *u == uid) {
        a.users.remove(i);
        let linked = a.linux.remove(&uid.to_string());
        drop(a);
        persist_runtime(ctx.data()).await?;
        match linked {
            Some(n) if valid_runas(&n) => {
                let Some(vm) = require_vm(ctx).await else { return Ok(()); };
                // Drop their NOPASSWD drop-in first so a deleted user never
                // keeps sudo (best-effort; userdel below is the real removal).
                let dropin = format!("/etc/sudoers.d/{n}");
                let _ = guest_exec(&vm, "/bin/rm", &["-f", &dropin], false, 10).await;
                let mut rc = guest_exec(&vm, "/usr/sbin/userdel", &["-r", &n], false, 30).await;
                if let Err(e) = &rc {
                    if e.to_string().contains("No such file") {
                        rc = guest_exec(&vm, "/usr/bin/userdel", &["-r", &n], false, 30).await;
                    }
                }
                match rc {
                    Ok((0, _, _)) => {
                        post_text(ctx, format!("Removed <@{}> and deleted linux `{}`.", uid, n)).await?;
                    }
                    Ok((c, _, _)) => {
                        post_text(ctx, format!("Removed <@{}> from the bot, but deleting linux `{}` failed (code {}). Remove it by hand in the VM.", uid, n, c)).await?;
                    }
                    Err(e) => {
                        post_text(ctx, format!("Removed <@{}> from the bot, but deleting linux `{}` failed:\n{}", uid, n, codeblock(&e.to_string()))).await?;
                    }
                };
            }
            Some(n) => {
                post_text(ctx, format!("Removed <@{}> (linked name `{}` looked invalid, left alone in the VM).", uid, n)).await?;
            }
            None => {
                post_text(ctx, format!("Removed <@{}>.", uid)).await?;
            }
        };
    } else {
        post_text(ctx, format!("<@{}> was not a manager.", uid)).await?;
    }
    Ok(())
}
