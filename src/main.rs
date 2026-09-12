mod commands;
mod config;
mod events;
mod live;
mod scrub;
mod util;
mod vm;
mod webhook;

use poise::serenity_prelude as serenity;

use crate::commands::{
    botrestart, help, info, live, notify, ps, purge_replies, restart, run, send, shell, shot,
    start, status, stop, user, useradd, userdel, userlist, users, warmode,
};
use crate::commands::BOOT_ART;
use crate::config::{Allowed, AllowedFile, BotSettings, Data, apply_legacy_import, config_file_path, ensure_config_template, load_file_config, save_json};
use crate::util::project_dir;

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
pub(crate) type Context<'a> = poise::Context<'a, Data, Error>;

#[cfg(test)]
mod tests {
    use crate::{commands::*, scrub::*, util::*};

    #[test]
    fn palette_becomes_blocks() {
        let out = sanitize_ansi("\x1b[40m   \x1b[41m   \x1b[m");
        assert!(out.contains("\x1b[0;30m███\x1b[0m"), "got {:?}", out);
        assert!(out.contains("\x1b[0;31m███\x1b[0m"), "got {:?}", out);
        assert!(!out.contains("40m"), "got {:?}", out);
    }

    #[test]
    fn ansi_tail_shows_bottom_in_one_message() {
        let lines: Vec<String> = (0..30).map(|i| format!("line {:02} {}", i, "x".repeat(70))).collect();
        let body = lines.join("\n");
        assert!(body.chars().count() > 2000);
        let out = ansi_tail(&body);
        assert!(out.starts_with("```ansi\n"), "ansi fence");
        assert!(out.ends_with("\n```"), "closed fence");
        assert!(out.chars().count() <= 2000, "fits Discord limit, got {}", out.chars().count());
        assert!(out.contains("line 29"), "bottom kept");
        assert!(!out.contains("line 00"), "head dropped");
        assert!(out.contains('…'), "truncation marked");
    }

    #[test]
    fn ansi_tail_short_output_unchanged_no_marker() {
        let out = ansi_tail("\x1b[0;32m$ cmd\x1b[0m\nok");
        assert!(out.contains("\x1b[0;32m$ cmd\x1b[0m"), "colors intact, got {:?}", out);
        assert!(!out.contains('…'), "no marker when nothing dropped");
        assert_eq!(ansi_tail(""), "```ansi\n(empty)\n```");
    }

    #[test]
    fn ansi_tail_never_splits_a_line() {
        let lines: Vec<String> = (0..40).map(|i| format!("\x1b[0;3{}mline {:02}\x1b[0m {}", i % 8, i, "y".repeat(60))).collect();
        let out = ansi_tail(&lines.join("\n"));
        assert!(out.chars().count() <= 2000);
        let b = out.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'[' {
                assert!(i > 0 && b[i - 1] == 0x1b, "bare SGR remnant at byte {}", i);
            }
            i += 1;
        }
    }

    #[test]
    fn tabs_expand_to_spaces() {
        let out = sanitize_ansi("a\tb");
        assert_eq!(out, "a        b", "got {:?}", out);
    }

    #[test]
    fn fit_reports_truncation_flag() {
        let (_, t) = fit_bottom_lines("short\nlines");
        assert!(!t);
        let long = (0..100).map(|i| format!("line {:03} {}", i, "x".repeat(12))).collect::<Vec<_>>().join("\n");
        let (fitted, t) = fit_bottom_lines(&long);
        assert!(t, "long output must flag truncated");
        assert!(fitted.contains("line 099"), "bottom kept");
        assert!(!fitted.contains("line 000"), "head dropped");
    }

    #[test]
    fn file_body_caps_huge_output() {
        let huge = "y".repeat(500_000);
        let capped = cap_file_body(&huge);
        assert!(capped.chars().count() <= 400_100, "got {}", capped.chars().count());
        assert!(capped.starts_with('…'), "marks truncation");
        assert_eq!(cap_file_body("small"), "small");
    }

    #[test]
    fn attach_name_is_safe_filename() {
        assert_eq!(attach_name("jefetch --static"), "jefetch.txt");
        assert_eq!(attach_name(""), "output.txt");
        assert_eq!(attach_name("../../../etc/passwd"), "etcpasswd.txt");
        assert_eq!(attach_name("sudo pacman -Syu"), "sudo.txt");
    }

    #[test]
    fn strip_sgr_drops_everything_escapey() {
        assert_eq!(strip_sgr("\x1b[0;32m$\x1b[0m hi"), "$ hi");
        assert_eq!(strip_sgr("\x1b[0;1m\x1b[0;36m'\x1b[0m"), "'");
        assert_eq!(strip_sgr("\x1b[38;2;1;2;3mX\x1b[0m"), "X");
        assert_eq!(strip_sgr("plain"), "plain");
        assert_eq!(strip_sgr("a\x1b]8;;http://x\x07b"), "ab");
        assert_eq!(strip_sgr("a\x1b"), "a");
        assert_eq!(strip_sgr("a\rb"), "ab");
        let intact = "\x1b[0;32mok\x1b[0m";
        assert!(!strip_sgr(intact).contains('\x1b'), "no ESC remains");
        assert_eq!(strip_sgr(intact), "ok");
    }

    #[test]
    fn parse_channel_accepts_id_only() {
        assert_eq!(parse_channel("123456789"), Some(123456789));
        assert_eq!(parse_channel("  123456789  "), Some(123456789));
        assert_eq!(parse_channel("off"), None);
        assert_eq!(parse_channel(""), None);
        assert_eq!(parse_channel("0"), None);
        assert_eq!(parse_channel("abc"), None);
        assert_eq!(parse_channel("<#123456789>"), None);
    }

    #[test]
    fn parse_target_id_accepts_mention_or_id() {
        assert_eq!(parse_target_id("1546673525392146553"), Some(1546673525392146553));
        assert_eq!(parse_target_id("<@1546673525392146553>"), Some(1546673525392146553));
        assert_eq!(parse_target_id("<@!1546673525392146553>"), Some(1546673525392146553));
        assert_eq!(parse_target_id("  123  "), Some(123));
        assert_eq!(parse_target_id(""), None);
        assert_eq!(parse_target_id("0"), None);
        assert_eq!(parse_target_id("abc"), None);
        assert_eq!(parse_target_id("<@abc>"), None);
    }

    #[test]
    fn boot_art_matches_lol_txt_byte_for_byte() {
        let rows: Vec<&str> = crate::commands::BOOT_ART.lines().collect();
        assert_eq!(rows.len(), 5, "five art rows");
        assert_eq!(rows[0], "          .        :-------:");
        assert_eq!(rows[1], "        ^/ \\^      :Im here:");
        assert_eq!(rows[2], "        ●   ●     <:-------:");
        assert_eq!(rows[3], "       /  ω  \\");
        assert_eq!(rows[4], "      /_/   \\_\\");
    }

    #[test]
    fn scrub_redacts_public_ipv4_only() {
        assert_eq!(scrub_public_ip("ip 203.0.113.7 ok"), "ip [redacted] ok");
        assert_eq!(scrub_public_ip("dns 8.8.8.8"), "dns [redacted]");
        assert_eq!(scrub_public_ip("a 1.2.3.4 b 5.6.7.8"), "a [redacted] b [redacted]");
        assert_eq!(scrub_public_ip("local 192.168.1.5"), "local 192.168.1.5");
        assert_eq!(scrub_public_ip("ten 10.0.0.1"), "ten 10.0.0.1");
        assert_eq!(scrub_public_ip("corp 172.16.5.4 and 172.31.255.1"), "corp 172.16.5.4 and 172.31.255.1");
        assert_eq!(scrub_public_ip("not-private 172.32.0.1"), "not-private [redacted]");
        assert_eq!(scrub_public_ip("loop 127.0.0.1"), "loop 127.0.0.1");
        assert_eq!(scrub_public_ip("link 169.254.169.254"), "link 169.254.169.254");
        assert_eq!(scrub_public_ip("cgnat 100.64.0.1"), "cgnat 100.64.0.1");
        assert_eq!(scrub_public_ip("not-cgnat 100.128.0.1"), "not-cgnat [redacted]");
        assert_eq!(scrub_public_ip("kernel 7.2.2-artix1"), "kernel 7.2.2-artix1");
        assert_eq!(scrub_public_ip("mem 1.48 GiB"), "mem 1.48 GiB");
        assert_eq!(scrub_public_ip("bad 999.1.1.1"), "bad 999.1.1.1");
        assert_eq!(scrub_public_ip("see 1.2.3.4."), "see [redacted].");
        assert_eq!(scrub_public_ip("v1.2.3.4 out"), "v1.2.3.4 out");
        assert_eq!(scrub_public_ip("1.2.3.4.5 out"), "1.2.3.4.5 out");
    }

    #[test]
    fn scrub_redacts_public_ipv6_only() {
        assert_eq!(scrub_public_ip("ip 2001:db8::1 ok"), "ip [redacted] ok");
        assert_eq!(scrub_public_ip("full 2001:db8:0:0:0:0:0:1"), "full [redacted]");
        assert_eq!(scrub_public_ip("ll fe80::1"), "ll fe80::1");
        assert_eq!(scrub_public_ip("ll FE80::A"), "ll FE80::A");
        assert_eq!(scrub_public_ip("ula fd00::5"), "ula fd00::5");
        assert_eq!(scrub_public_ip("ula fc12::9"), "ula fc12::9");
        assert_eq!(scrub_public_ip("lo ::1"), "lo ::1");
        assert_eq!(scrub_public_ip("lo 0:0:0:0:0:0:0:1"), "lo 0:0:0:0:0:0:0:1");
        assert_eq!(scrub_public_ip("x :: y"), "x :: y");
        assert_eq!(scrub_public_ip("mac aa:bb:cc:dd:ee:ff"), "mac aa:bb:cc:dd:ee:ff");
        assert_eq!(scrub_public_ip("at 12:34:56"), "at 12:34:56");
        assert_eq!(scrub_public_ip("mapped ::ffff:203.0.113.7"), "mapped [redacted]");
        assert_eq!(scrub_public_ip("mc ff02::1"), "mc [redacted]");
        assert_eq!(scrub_public_ip("bracketed [2001:db8::1]"), "bracketed [[redacted]]");
        assert_eq!(scrub_public_ip("port [2001:db8::1]:443"), "port [[redacted]]:443");
        assert_eq!(scrub_public_ip("v4 still 8.8.8.8 ok"), "v4 still [redacted] ok");
        assert_eq!(scrub_public_ip("v4 local 192.168.0.1 ok"), "v4 local 192.168.0.1 ok");
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
        assert!(a.len() >= 8, "got {:?}", a);
        assert_ne!(a, b, "suffix should differ per call");
    }
}

#[tokio::main]
async fn main() {
    let fresh_config = !config_file_path().exists();
    let mut file_config = load_file_config();
    let token: String = match file_config.discord_token.clone() {
        Some(t) => t,
        None => std::env::var("DISCORD_TOKEN")
            .expect("set discord_token in ~/.config/artixy/config.toml or DISCORD_TOKEN env"),
    };
    let owner: u64 = match file_config.owner_id {
        Some(id) => id,
        None => std::env::var("OWNER_ID")
            .expect("set owner_id in ~/.config/artixy/config.toml or OWNER_ID env")
            .parse()
            .expect("OWNER_ID must be a number"),
    };
    ensure_config_template(owner);
    if fresh_config {
        let legacy_users: AllowedFile = tokio::fs::read_to_string("users.json")
            .await
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        let legacy_shells: std::collections::HashMap<String, String> =
            tokio::fs::read_to_string("shells.json")
                .await
                .ok()
                .and_then(|r| serde_json::from_str(&r).ok())
                .unwrap_or_default();
        let legacy_notify: Option<u64> = tokio::fs::read_to_string("settings.json")
            .await
            .ok()
            .and_then(|r| serde_json::from_str::<BotSettings>(&r).ok())
            .and_then(|s| s.notify_channel);
        apply_legacy_import(&mut file_config, legacy_users, legacy_shells, legacy_notify);
        if let Ok(text) = toml::to_string_pretty(&file_config) {
            let p = config_file_path();
            let ps = p.to_string_lossy().to_string();
            let _ = tokio::fs::remove_file(&p).await;
            let _ = save_json(&ps, text).await;
        }
    }
    crate::webhook::init_webhook_urls(file_config.webhook_urls);
    let _ = std::env::set_current_dir(project_dir());
    let vm = file_config
        .vm_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("VM_NAME").ok())
        .unwrap_or_else(|| "voidvm".into());
    let data = Data {
        allowed: tokio::sync::RwLock::new(Allowed {
            owner,
            users: file_config.managers.clone(),
            linux: file_config.linux.clone(),
            blocked: file_config.blocked_ids.clone(),
        }),
        vm,
        live: Default::default(),
        settings: tokio::sync::RwLock::new(BotSettings {
            notify_channel: file_config.notify_channel,
            war_mode: file_config.war_mode,
        }),
        shells: tokio::sync::RwLock::new(file_config.shells.clone()),
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
                botrestart(),
                run(),
                live(),
                shot(),
                send(),
                notify(),
                purge_replies(),
                warmode(),
            ],
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
            event_handler: |ctx, event, framework, data| {
                Box::pin(events::event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                if let Some(ch) = data.settings.read().await.notify_channel {
                    let _ = crate::webhook::post_message(
                        &ctx.http,
                        serenity::ChannelId::new(ch),
                        format!("```\n{BOOT_ART}\n```"),
                        Vec::new(),
                    )
                    .await;
                }
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

