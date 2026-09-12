use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{live::LiveMap, util::random_suffix, Error};

fn war_default_on() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct BotSettings {
    #[serde(default)]
    pub(crate) notify_channel: Option<u64>,
    #[serde(default = "war_default_on")]
    pub(crate) war_mode: bool,
}

impl Default for BotSettings {
    fn default() -> Self {
        Self {
            notify_channel: None,
            war_mode: war_default_on(),
        }
    }
}

pub(crate) struct Data {
    pub(crate) allowed: tokio::sync::RwLock<Allowed>,
    pub(crate) vm: String,
    pub(crate) live: LiveMap,
    pub(crate) settings: tokio::sync::RwLock<BotSettings>,
    pub(crate) shells: tokio::sync::RwLock<std::collections::HashMap<String, String>>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct AllowedFile {
    pub(crate) users: Vec<u64>,
    #[serde(default)]
    pub(crate) linux: std::collections::HashMap<String, String>,
}

pub(crate) struct Allowed {
    pub(crate) owner: u64,
    pub(crate) users: Vec<u64>,
    pub(crate) linux: std::collections::HashMap<String, String>,
    pub(crate) path: PathBuf,
    pub(crate) blocked: Vec<u64>,
}

impl Allowed {
    pub(crate) async fn save(&self) -> Result<(), Error> {
        let data = serde_json::to_string_pretty(&AllowedFile {
            users: self.users.clone(),
            linux: self.linux.clone(),
        })?;
        save_json(
            self.path.to_str().unwrap_or("users.json"),
            data,
        )
        .await
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct FileConfig {
    #[serde(default)]
    pub(crate) owner_id: Option<u64>,
    #[serde(default)]
    pub(crate) blocked_ids: Vec<u64>,
    #[serde(default)]
    pub(crate) webhook_urls: Vec<String>,
    #[serde(default)]
    pub(crate) discord_token: Option<String>,
    #[serde(default)]
    pub(crate) vm_name: Option<String>,
}

pub(crate) fn access_allowed(owner: u64, users: &[u64], blocked: &[u64], id: u64) -> bool {
    !blocked.contains(&id) && (id == owner || users.contains(&id))
}

pub(crate) fn config_file_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join("artixy").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

fn normalize_file_config(mut cfg: FileConfig) -> FileConfig {
    for slot in [&mut cfg.discord_token, &mut cfg.vm_name] {
        if slot.as_deref().map(str::trim).unwrap_or("").is_empty() {
            *slot = None;
        }
    }
    cfg
}

pub(crate) fn load_file_config() -> FileConfig {
    let cfg: FileConfig = std::fs::read_to_string(config_file_path())
        .ok()
        .and_then(|r| toml::from_str(&r).ok())
        .unwrap_or_default();
    let cfg = normalize_file_config(cfg);
    lock_config_private();
    cfg
}

pub(crate) fn lock_config_private() {
    use std::os::unix::fs::PermissionsExt;
    let path = config_file_path();
    if let Ok(meta) = std::fs::metadata(&path) {
        let mut perm = meta.permissions();
        if perm.mode() & 0o077 != 0 {
            perm.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perm);
        }
    }
}

pub(crate) fn ensure_config_template(owner_id: u64) {
    let path = config_file_path();
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let token = std::env::var("DISCORD_TOKEN").unwrap_or_default();
    let vm = std::env::var("VM_NAME").unwrap_or_else(|_| "voidvm".into());
    let template = format!(
        "owner_id = {}\ndiscord_token = \"{}\"\nvm_name = \"{}\"\nblocked_ids = []\nwebhook_urls = []\n",
        owner_id, token, vm
    );
    let _ = std::fs::write(path, template);
    lock_config_private();
}

pub(crate) fn load_shells() -> std::collections::HashMap<String, String> {
    std::fs::read_to_string("shells.json")
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default()
}

pub(crate) async fn save_settings(data: &Data) -> Result<(), Error> {
    let s = data.settings.read().await;
    save_json(
        "settings.json",
        serde_json::to_string_pretty(&*s)?,
    )
    .await
}

pub(crate) async fn save_json(path: &str, data: String) -> Result<(), Error> {
    let tmp = format!("{}.{}.tmp", path, random_suffix());
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    let mut f = opts.open(&tmp).await?;
    use tokio::io::AsyncWriteExt;
    f.write_all(data.as_bytes()).await?;
    f.sync_all().await?;
    drop(f);
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> FileConfig {
        toml::from_str(text).expect("test TOML must parse")
    }

    #[test]
    fn file_config_parses_full() {
        let c = parse("owner_id = 123\nblocked_ids = [4, 5]\nwebhook_urls = [\"https://discord.com/api/webhooks/1/abc\"]\n");
        assert_eq!(c.owner_id, Some(123));
        assert_eq!(c.blocked_ids, vec![4, 5]);
        assert_eq!(c.webhook_urls, vec!["https://discord.com/api/webhooks/1/abc".to_string()]);
    }

    #[test]
    fn file_config_parses_secrets_and_vm() {
        let c = parse("discord_token = \"tok123\"\nvm_name = \"artix\"\n");
        assert_eq!(c.discord_token.as_deref(), Some("tok123"));
        assert_eq!(c.vm_name.as_deref(), Some("artix"));
        assert_eq!(c.owner_id, None);
    }

    #[test]
    fn normalize_clears_blank_secrets() {
        let c = normalize_file_config(parse("discord_token = \"  \"\nvm_name = \"\"\n"));
        assert_eq!(c.discord_token, None);
        assert_eq!(c.vm_name, None);
        let c = normalize_file_config(parse("discord_token = \"tok\"\nvm_name = \"v\"\n"));
        assert_eq!(c.discord_token.as_deref(), Some("tok"));
        assert_eq!(c.vm_name.as_deref(), Some("v"));
    }

    #[test]
    fn file_config_missing_keys_default() {
        let c = parse("");
        assert_eq!(c.owner_id, None);
        assert!(c.blocked_ids.is_empty());
        let c = parse("owner_id = 7\n");
        assert_eq!(c.owner_id, Some(7));
        assert!(c.blocked_ids.is_empty());
    }

    #[test]
    fn file_config_invalid_is_default() {
        let c: FileConfig = toml::from_str("owner_id = [unclosed").unwrap_or_default();
        assert_eq!(c.owner_id, None);
        assert!(c.blocked_ids.is_empty());
    }

    #[test]
    fn access_allowed_matrix() {
        assert!(access_allowed(1, &[2], &[], 1));
        assert!(access_allowed(1, &[2], &[], 2));
        assert!(!access_allowed(1, &[2], &[], 3));
        assert!(!access_allowed(1, &[2], &[2], 2));
        assert!(!access_allowed(1, &[2], &[1], 1));
        assert!(!access_allowed(1, &[], &[9], 9));
    }
}

