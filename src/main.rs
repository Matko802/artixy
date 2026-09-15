mod ai;
mod commands;
mod config;
mod events;
mod feed;
mod kitty;
mod live;
mod pngencode;
mod scrub;
mod termrender;
mod tui;
mod util;
mod vm;
mod webhook;

use poise::serenity_prelude as serenity;

use crate::commands::{
    ai, botrestart, help, info, notify, ps, purge_replies, restart, run, sayas, send, shell,
    admin, start, status, stop, upload, user, warmode, websearch,
};
use crate::commands::BOOT_ART;
use crate::config::{Allowed, AllowedFile, BotSettings, Data, apply_legacy_import, config_file_path, ensure_config_template, load_file_config, save_json};
use crate::util::project_dir;

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
pub(crate) type Context<'a> = poise::Context<'a, Data, Error>;


#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("help")
        || args.get(1).map(|s| s.as_str()) == Some("--help")
        || args.get(1).map(|s| s.as_str()) == Some("-h")
    {
        println!("artixy — run with no args to start the Discord bot, or `artixy tui` for the local fake-discord terminal UI.");
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("tui") {
        ensure_config_template();
        let file_config = load_file_config();
        let token = file_config
            .discord_token
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("DISCORD_TOKEN").ok().filter(|s| !s.trim().is_empty()));
        let settings = crate::tui::local_settings(
            file_config.ai_enabled,
            if crate::ai::valid_model_name(&file_config.ai_model) {
                file_config.ai_model.clone()
            } else {
                crate::ai::default_model()
            },
            if file_config.ollama_host.trim().is_empty() {
                crate::ai::default_host()
            } else {
                file_config.ollama_host.clone()
            },
            &file_config.ollama_api_key,
            token,
        );
        if let Err(e) = crate::tui::run_tui(settings).await {
            eprintln!("tui error: {e}");
        }
        return;
    }
    let fresh_config = !config_file_path().exists();
    ensure_config_template();
    let mut file_config = load_file_config();
    let token: String = match file_config.discord_token.clone() {
        Some(t) => t,
        None => std::env::var("DISCORD_TOKEN").unwrap_or_else(|_| {
            eprintln!(
                "error: set discord_token in {} or DISCORD_TOKEN env",
                config_file_path().display()
            );
            std::process::exit(1);
        }),
    };
    let owner: u64 = match file_config.owner_id {
        Some(id) => id,
        None => std::env::var("OWNER_ID")
            .unwrap_or_else(|_| {
                eprintln!(
                    "error: set owner_id in {} or OWNER_ID env",
                    config_file_path().display()
                );
                std::process::exit(1);
            })
            .parse()
            .unwrap_or_else(|_| {
                eprintln!("error: OWNER_ID must be a number");
                std::process::exit(1);
            }),
    };
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
    let vm: String = file_config
        .vm_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("VM_NAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| {
            eprintln!(
                "warning: vm_name not set in {} or VM_NAME env — VM commands will reply with a friendly error until you set it",
                config_file_path().display()
            );
            String::new()
        });
    let data = Data {
        allowed: std::sync::Arc::new(tokio::sync::RwLock::new(Allowed {
            owner,
            users: file_config.managers.clone(),
            linux: file_config.linux.clone(),
            blocked: file_config.blocked_ids.clone(),
            admins: file_config.admin_ids.clone(),
        })),
        vm: std::sync::Arc::new(tokio::sync::RwLock::new(vm)),
        live: Default::default(),
        settings: std::sync::Arc::new(tokio::sync::RwLock::new(BotSettings {
            notify_channel: file_config.notify_channel,
            war_mode: file_config.war_mode,
            sayas_enabled: file_config.sayas_enabled,
            ai_enabled: file_config.ai_enabled,
            ai_model: if crate::ai::valid_model_name(&file_config.ai_model) {
                file_config.ai_model.clone()
            } else {
                crate::ai::default_model()
            },
            ollama_host: if file_config.ollama_host.trim().is_empty() {
                crate::ai::default_host()
            } else {
                file_config.ollama_host.clone()
            },
            ollama_api_key: file_config.ollama_api_key.trim().to_string(),
        })),
        shells: std::sync::Arc::new(tokio::sync::RwLock::new(file_config.shells.clone())),
    };
    tokio::spawn(crate::config::watch_config(data.clone()));

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
                user(),
                admin(),
                shell(),
                botrestart(),
                run(),
                sayas(),
                send(),
                notify(),
                purge_replies(),
                warmode(),
                upload(),
                ai(),
                websearch(),
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
                poise::builtins::register_globally(ctx, &framework.options().commands)
                    .await
                    .map_err(|e| -> Error {
                        let msg = e.to_string();
                        if msg.contains("401") || msg.to_lowercase().contains("unauthorized") {
                            format!("Discord rejected the token (401 Unauthorized) — check discord_token in config: {e}").into()
                        } else {
                            format!("failed to register slash commands (network or Discord API issue): {e}").into()
                        }
                    })?;
                let stale_vm = data.vm.read().await.clone();
                crate::live::cleanup_stale_live_files(&stale_vm).await;
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

    let intents = serenity::GatewayIntents::non_privileged()
        | serenity::GatewayIntents::MESSAGE_CONTENT
        | serenity::GatewayIntents::GUILD_MESSAGE_TYPING
        | serenity::GatewayIntents::DIRECT_MESSAGE_TYPING;
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .expect("client build");
    client.start().await.expect("client start");
}

