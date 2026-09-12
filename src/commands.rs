use poise::serenity_prelude as serenity;
use std::path::PathBuf;

use crate::{
    config::persist_runtime,
    live::begin_run,
    util::{attach_name, cap_file_body, codeblock, deployed_via_nix, project_dir, random_suffix, strip_sgr, valid_runas},
    vm::{agent_ping, guest_exec, linked_user, virsh, wait_agent},
    webhook::{is_own_message, mark_self_deleted, post_message, post_response, post_text},
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
    post_text(ctx, "Not authorized. Ask the owner to run `/useradd <your discord id>`.")
        .await?;
    Ok(false)
}

pub(crate) async fn maybe_defer(ctx: Context<'_>) {
    if matches!(ctx, poise::Context::Application(_)) {
        let _ = ctx.defer().await;
    }
}

pub(crate) const HELP: &str = "\
**Who needs help? its ez :3** Everything acts on the one hardcoded VM, no names needed. Only the owner + added users can use me. Slash commands only.\n\
\n**VM**\n`/ps` — state of the VM\n`/status` — quick state + agent check\n`/start` — power on + wait for guest agent\n`/stop` — graceful shutdown\n`/restart` — reboot\n`/info` — details + agent status\n\
\n**Who can use me**\n`/users` / `/userlist` — show owner + managers\n`/useradd @user` — owner only: links them and creates their Linux account in Artix (name from discord name).\n`/userdel @user` — owner only: revokes bot access and deletes their Linux account in the VM\n`/shell [fish|bash]` — your shell interpreter (default bash)\n`/notify <channel-id>` or `/notify off` — owner only: where I post my boot message, unset means silent\n`/purge_replies <user-id> [limit]` — owner only: delete their replies to my messages here\n`/warmode <true|false>` — owner only: arm or stand down the protections\n`/run <command>` — run it for real inside the VM, prints the output. Quick commands answer with plain text, long ones switch to a live image feed on their own, updating about every second.\n
\n**Run real commands in Artix**\n`/run <command>` — runs it for real inside the VM through the guest agent and prints the output. e.g. `/run sudo pacman -Syu`, `/run ls -la`. Runs as YOUR linked linux account (`whoami` proves it). Reply to its live message to type into the running command (`.backspace` `.enter` `.esc` send those keys).\n`/shot` — screenshot of the host screen, uploaded here\n`/send <path>` — upload a host file here (absolute path, ~20MB max)\n`/say <message> [reply_to]` — owner only: say something as me (reply_to takes a message ID or link)\n\
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
    let vm = ctx.data().vm.clone();
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
            post_text(ctx, format!("{} booted in {} (guest agent up).", vm, took)).await?;
        } else {
            post_text(ctx, format!(
                "`{}` is on and the guest agent answers.",
                vm
            ))
            .await?;
        }
    } else {
        post_text(ctx, format!("`{}` is on but the guest agent is silent. Inside Artix run `sudo pacman -S qemu-guest-agent` and enable its service, then `;start` again.", vm)).await?;
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
    let vm = ctx.data().vm.clone();
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
    let vm = ctx.data().vm.clone();
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
    let vm = ctx.data().vm.clone();
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
    let vm = ctx.data().vm.clone();
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

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn say(
    ctx: Context<'_>,
    #[description = "Text to send as artixy"] message: String,
    #[description = "Message ID or link to reply to"] reply_to: Option<String>,
) -> Result<(), Error> {
    if !is_owner(ctx).await {
        post_text(ctx, "Owner only.").await?;
        return Ok(());
    }
    let text = message.trim_end().to_string();
    if text.trim().is_empty() {
        post_text(ctx, "Usage: `/say <message> [reply_to: message ID or link]`.").await?;
        return Ok(());
    }
    maybe_defer(ctx).await;
    let http = ctx.serenity_context().http.clone();
    let channel = ctx.channel_id();
    if let poise::Context::Prefix(pctx) = ctx {
        let _ = pctx.msg.delete(&http).await;
    }
    if let Some(target) = reply_to {
        let Some((ch_id, msg_id)) = parse_message_ref(&target, channel.get()) else {
            post_text(ctx, "Couldn't read that reply target — give a message ID or a full message link.").await?;
            return Ok(());
        };
        let ch = serenity::ChannelId::new(ch_id);
        let target_msg = match ch.message(&http, serenity::MessageId::new(msg_id)).await {
            Ok(m) => m,
            Err(_) => {
                post_text(ctx, "Couldn't fetch that message (wrong channel, or I can't see it).").await?;
                return Ok(());
            }
        };
        if text.chars().count() <= 2000 {
            if target_msg.reply(&http, &text).await.is_err() {
                post_text(ctx, "Reply failed (missing permission?).").await?;
            }
        } else {
            let att = serenity::CreateAttachment::bytes(
                cap_file_body(&strip_sgr(&text)).into_bytes(),
                attach_name(&text),
            );
            let builder = serenity::CreateMessage::new()
                .add_file(att)
                .reference_message((ch, target_msg.id));
            if ch.send_message(&http, builder).await.is_err() {
                post_text(ctx, "Reply failed (missing permission?).").await?;
            }
        }
    } else if text.chars().count() <= 2000 {
        let _ = post_message(&http, channel, text, Vec::new()).await;
    } else {
        let att = (attach_name(&text), cap_file_body(&strip_sgr(&text)).into_bytes());
        let _ = post_message(&http, channel, String::new(), vec![att]).await;
    }
    if let poise::Context::Application(actx) = ctx {
        let _ = actx.interaction.delete_response(&http).await;
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
    post_text(ctx, format!("`{}`: {} | {}", vm, state.trim(), agent))
        .await?;
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn useradd(
    ctx: Context<'_>,
    #[description = "User to authorize"] user: serenity::User,
) -> Result<(), Error> {
    do_useradd(ctx, &user).await
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn userdel(
    ctx: Context<'_>,
    #[description = "User to remove"] user: serenity::User,
) -> Result<(), Error> {
    do_userdel(ctx, &user).await
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
        post_text(ctx, "Owner only.").await?;
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
        post_text(ctx, "Owner only.").await?;
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
        post_text(ctx, "Owner only.").await?;
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

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn users(ctx: Context<'_>) -> Result<(), Error> {
    do_users(ctx).await
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn userlist(ctx: Context<'_>) -> Result<(), Error> {
    do_users(ctx).await
}

#[poise::command(
    slash_command,
    prefix_command,
    subcommands("add", "del"),
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn user(ctx: Context<'_>) -> Result<(), Error> {
    post_text(ctx, "Usage: `;user add <discord id> [linuxname]` or `;user del <discord id>`.")
        .await?;
    Ok(())
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn add(
    ctx: Context<'_>,
    #[description = "User to authorize"] user: serenity::User,
) -> Result<(), Error> {
    do_useradd(ctx, &user).await
}

#[poise::command(
    slash_command,
    prefix_command,
    install_context = "Guild|User",
    interaction_context = "Guild|BotDm|PrivateChannel"
)]
pub(crate) async fn del(
    ctx: Context<'_>,
    #[description = "User to remove"] user: serenity::User,
) -> Result<(), Error> {
    do_userdel(ctx, &user).await
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

pub(crate) async fn do_useradd(ctx: Context<'_>, user: &serenity::User) -> Result<(), Error> {
    if !is_owner(ctx).await {
        post_text(ctx, "Owner only.").await?;
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
    let vm = ctx.data().vm.clone();
    let state = virsh(&["domstate", &vm]).await.unwrap_or_default();
    if state.trim() != "running" {
        match virsh(&["start", &vm]).await {
            Ok(_) => {
                post_text(ctx, format!("`{}` was off, starting it first…", vm)).await?;
            }
            Err(e) => {
                post_text(ctx, format!(
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
            post_text(ctx, format!("Authorized `{}` in the bot, but the guest agent is silent — no Linux account created. Install `qemu-guest-agent` in Artix, then rerun `;useradd <@{}> {}`.", uid, uid, name)).await?;
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
    post_text(ctx, format!(
        "added user \"{}\" linked to `{}` — account created, no password set.",
        name, uid
    ))
    .await?;
    Ok(())
}

pub(crate) async fn do_userdel(ctx: Context<'_>, user: &serenity::User) -> Result<(), Error> {
    if !is_owner(ctx).await {
        post_text(ctx, "Owner only.").await?;
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
                let vm = ctx.data().vm.clone();
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
