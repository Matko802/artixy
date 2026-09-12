use poise::serenity_prelude as serenity;
use std::path::PathBuf;

use crate::{
    config::save_json,
    config::save_settings,
    live::{abort_live_for_channel, begin_live, cleanup_live_files},
    scrub::scrub_public_ip,
    util::{attach_name, cap_file_body, codeblock, deployed_via_nix, fence_inline, fit_bottom_lines, project_dir, random_suffix, sanitize_ansi, valid_runas},
    vm::{agent_ping, guest_exec, linked_user, run_guest_cmd, user_shell, virsh, wait_agent},
    Context, Error,
};

pub(crate) async fn is_authed(ctx: Context<'_>) -> bool {
    let id = ctx.author().id.get();
    let a = ctx.data().allowed.read().await;
    id == a.owner || a.users.contains(&id)
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
    ctx.say("Not authorized. Ask the owner to run `/useradd <your discord id>`.")
        .await?;
    Ok(false)
}

pub(crate) async fn maybe_defer(ctx: Context<'_>) {
    if matches!(ctx, poise::Context::Application(_)) {
        let _ = ctx.defer().await;
    }
}

pub(crate) async fn send_output(ctx: Context<'_>, cmd: &str, body: &str) -> Result<(), Error> {
    let clean = sanitize_ansi(body.trim_end());
    let (fitted, truncated) = fit_bottom_lines(&clean);
    if !truncated {
        ctx.say(fence_inline(&fitted)).await?;
        return Ok(());
    }
    let att = serenity::CreateAttachment::bytes(cap_file_body(&clean).into_bytes(), attach_name(cmd));
    ctx.send(poise::CreateReply::default().attachment(att))
        .await?;
    Ok(())
}

pub(crate) const HELP: &str = "\
**artixy — your Artix VM in your pocket.** Everything acts on the one hardcoded VM, no names needed. Only the owner + added users can use me. Slash commands only.\n\
\n**VM**\n`/ps` — state of the VM\n`/status` — quick state + agent check\n`/start` — power on + wait for guest agent\n`/stop` — graceful shutdown\n`/restart` — reboot\n`/info` — details + agent status\n\
\n**Who can use me**\n`/users` / `/userlist` — show owner + managers\n`/useradd @user` — owner only: links them and creates their Linux account in Artix (name from discord name).\n`/userdel @user` — owner only: revokes bot access and deletes their Linux account in the VM\n`/shell [fish|bash]` — your shell interpreter (default bash)\n`/notify <channel-id>` or `/notify off` — owner only: where I post my boot message, unset means silent\n`/run <command>` — run it for real inside the VM, prints the output\n\
\n**Run real commands in Artix**\n`/run <command>` — runs it for real inside the VM through the guest agent and prints the output. e.g. `/run sudo pacman -Syu`, `/run ls -la`. Runs as YOUR linked linux account (`whoami` proves it).\n`/live <command>` — follows one run live in a single message until it finishes. Starting another run stops it.\n`/shot` — screenshot of the host screen, uploaded here\n`/send <path>` — upload a host file here (absolute path, ~20MB max)\n\
\n**Warning:** managers can power this machine on/off. Keep the token secret: it lives only in `.env`, never in git.";

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn help(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say(HELP).await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn ps(ctx: Context<'_>) -> Result<(), Error> {
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
        ctx.say(format!(
            "`{}` is already on. Waiting for the guest agent…",
            vm
        ))
        .await?;
    } else {
        match virsh(&["start", &vm]).await {
            Ok(_) => {
                started_here = true;
                boot_t0 = Some(std::time::Instant::now());
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
            let secs = boot_t0.map(|t| t.elapsed().as_secs()).unwrap_or(0);
            let took = if secs >= 60 {
                format!("{}m {}s", secs / 60, secs % 60)
            } else {
                format!("{}s", secs)
            };
            ctx.say(format!("{} booted in {} (guest agent up).", vm, took)).await?;
        } else {
            ctx.say(format!(
                "`{}` is on and the guest agent answers.",
                vm
            ))
            .await?;
        }
    } else {
        ctx.say(format!("`{}` is on but the guest agent is silent. Inside Artix run `sudo pacman -S qemu-guest-agent` and enable its service, then `;start` again.", vm)).await?;
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn stop(ctx: Context<'_>) -> Result<(), Error> {
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
pub(crate) async fn restart(ctx: Context<'_>) -> Result<(), Error> {
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
            ctx.say(codeblock(&format!("{}\n{}", o, agent))).await?;
        }
        Err(e) => {
            ctx.say(codeblock(&e.to_string())).await?;
        }
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
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
            ctx.say(format!(
                "Your shell: `{}`. Change with `/shell fish` or `/shell bash`.",
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

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn botrestart(ctx: Context<'_>) -> Result<(), Error> {
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
pub(crate) async fn run(
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
        cleanup_live_files(&vm, &old.out_f, &old.code_f).await;
    }
    let sh = user_shell(ctx.data(), uid).await;
    let runas = linked_user(ctx.data(), uid).await;
    let (body, code) = run_guest_cmd(&vm, &sh, &cmd, runas.as_deref(), 300).await?;
    let body = if is_owner(ctx).await {
        body
    } else {
        scrub_public_ip(&body)
    };
    send_output(ctx, &cmd, &body).await?;
    eprintln!("run for {}: exit {}", ctx.author().name, code);
    Ok(())
}

pub(crate) async fn do_live(ctx: Context<'_>, cmd: String) -> Result<(), Error> {
    maybe_defer(ctx).await;
    if cmd.trim().is_empty() {
        ctx.say("Usage: `/live <command>`.").await?;
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
        !is_owner(ctx).await,
    )
    .await;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn live(
    ctx: Context<'_>,
    #[description = "Command to follow live"] cmd: String,
) -> Result<(), Error> {
    if !need_auth(ctx).await? {
        return Ok(());
    }
    do_live(ctx, cmd).await
}

#[poise::command(slash_command, prefix_command)]
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
        ctx.say("Absolute path only.").await?;
        return Ok(());
    }
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
    ctx.say(format!("`{}`: {} | {}", vm, state.trim(), agent))
        .await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn useradd(
    ctx: Context<'_>,
    #[description = "User to authorize"] user: serenity::User,
) -> Result<(), Error> {
    do_useradd(ctx, &user).await
}

#[poise::command(slash_command, prefix_command)]
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

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn notify(
    ctx: Context<'_>,
    #[description = "channel ID for boot messages, or off"] what: Option<String>,
) -> Result<(), Error> {
    if !is_owner(ctx).await {
        ctx.say("Owner only.").await?;
        return Ok(());
    }
    match what.as_deref().map(str::trim) {
        None => {
            let s = ctx.data().settings.read().await;
            match s.notify_channel {
                Some(id) => {
                    ctx.say(format!("Boot messages go to <#{}>.", id)).await?;
                }
                None => {
                    ctx.say("Boot messages are OFF (no channel set).").await?;
                }
            }
        }
        Some(v) if v.eq_ignore_ascii_case("off") => {
            ctx.data().settings.write().await.notify_channel = None;
            save_settings(ctx.data()).await?;
            ctx.say("Boot messages OFF.").await?;
        }
        Some(v) => match parse_channel(v) {
            Some(id) => {
                ctx.data().settings.write().await.notify_channel = Some(id);
                save_settings(ctx.data()).await?;
                ctx.say(format!("Boot messages will go to `<#{id}>`.\n```\n{BOOT_ART}\n```")).await?;
            }
            None => {
                ctx.say("Usage: `/notify <channel-id>` or `/notify off`.").await?;
            }
        },
    }
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn users(ctx: Context<'_>) -> Result<(), Error> {
    do_users(ctx).await
}

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn userlist(ctx: Context<'_>) -> Result<(), Error> {
    do_users(ctx).await
}

#[poise::command(slash_command, prefix_command, subcommands("add", "del"))]
pub(crate) async fn user(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("Usage: `;user add <discord id> [linuxname]` or `;user del <discord id>`.")
        .await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
pub(crate) async fn add(
    ctx: Context<'_>,
    #[description = "User to authorize"] user: serenity::User,
) -> Result<(), Error> {
    do_useradd(ctx, &user).await
}

#[poise::command(slash_command, prefix_command)]
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
    ctx.say(msg).await?;
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
            ctx.say(format!("Authorized `{}` in the bot, but the guest agent is silent — no Linux account created. Install `qemu-guest-agent` in Artix, then rerun `;useradd <@{}> {}`.", uid, uid, name)).await?;
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
    ctx.say(format!(
        "added user \"{}\" linked to `{}` — account created, no password set.",
        name, uid
    ))
    .await?;
    Ok(())
}

pub(crate) async fn do_userdel(ctx: Context<'_>, user: &serenity::User) -> Result<(), Error> {
    if !is_owner(ctx).await {
        ctx.say("Owner only.").await?;
        return Ok(());
    }
    maybe_defer(ctx).await;
    let uid = user.id.get();
    let mut a = ctx.data().allowed.write().await;
    if let Some(i) = a.users.iter().position(|u| *u == uid) {
        a.users.remove(i);
        let linked = a.linux.remove(&uid.to_string());
        a.save().await?;
        drop(a);
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
                        ctx.say(format!("Removed <@{}> and deleted linux `{}`.", uid, n)).await?;
                    }
                    Ok((c, _, _)) => {
                        ctx.say(format!("Removed <@{}> from the bot, but deleting linux `{}` failed (code {}). Remove it by hand in the VM.", uid, n, c)).await?;
                    }
                    Err(e) => {
                        ctx.say(format!("Removed <@{}> from the bot, but deleting linux `{}` failed:\n{}", uid, n, codeblock(&e.to_string()))).await?;
                    }
                };
            }
            Some(n) => {
                ctx.say(format!("Removed <@{}> (linked name `{}` looked invalid, left alone in the VM).", uid, n)).await?;
            }
            None => {
                ctx.say(format!("Removed <@{}>.", uid)).await?;
            }
        };
    } else {
        ctx.say(format!("<@{}> was not a manager.", uid)).await?;
    }
    Ok(())
}

