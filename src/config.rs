use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{live::LiveMap, util::random_suffix, Error};

fn war_default_off() -> bool {
    false
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct BotSettings {
    #[serde(default)]
    pub(crate) notify_channel: Option<u64>,
    #[serde(default = "war_default_off")]
    pub(crate) war_mode: bool,
    #[serde(default = "sayas_default_off")]
    pub(crate) sayas_enabled: bool,
    #[serde(default = "ai_default_off")]
    pub(crate) ai_enabled: bool,
    #[serde(default = "crate::ai::default_model")]
    pub(crate) ai_model: String,
    #[serde(default = "crate::ai::default_host")]
    pub(crate) ollama_host: String,
    #[serde(default)]
    pub(crate) gemini_api_key: String,
}

fn sayas_default_off() -> bool {
    false
}

fn ai_default_off() -> bool {
    false
}

impl Default for BotSettings {
    fn default() -> Self {
        Self {
            notify_channel: None,
            war_mode: war_default_off(),
            sayas_enabled: sayas_default_off(),
            ai_enabled: ai_default_off(),
            ai_model: crate::ai::default_model(),
            ollama_host: crate::ai::default_host(),
            gemini_api_key: String::new(),
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

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct AllowedFile {
    pub(crate) users: Vec<u64>,
    #[serde(default)]
    pub(crate) linux: std::collections::HashMap<String, String>,
}

pub(crate) struct Allowed {
    pub(crate) owner: u64,
    pub(crate) users: Vec<u64>,
    pub(crate) linux: std::collections::HashMap<String, String>,
    pub(crate) blocked: Vec<u64>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
pub(crate) struct FileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) owner_id: Option<u64>,
    #[serde(default)]
    pub(crate) blocked_ids: Vec<u64>,
    #[serde(default)]
    pub(crate) webhook_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) discord_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vm_name: Option<String>,
    #[serde(default)]
    pub(crate) war_mode: bool,
    #[serde(default = "sayas_default_off")]
    pub(crate) sayas_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) notify_channel: Option<u64>,
    #[serde(default)]
    pub(crate) managers: Vec<u64>,
    #[serde(default)]
    pub(crate) linux: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(crate) shells: std::collections::HashMap<String, String>,
    #[serde(default = "ai_default_off")]
    pub(crate) ai_enabled: bool,
    #[serde(default = "crate::ai::default_model")]
    pub(crate) ai_model: String,
    #[serde(default = "crate::ai::default_host")]
    pub(crate) ollama_host: String,
    #[serde(default)]
    pub(crate) gemini_api_key: String,
}

pub(crate) fn apply_legacy_import(
    cfg: &mut FileConfig,
    users: AllowedFile,
    shells: std::collections::HashMap<String, String>,
    notify: Option<u64>,
) {
    if cfg.managers.is_empty() && cfg.linux.is_empty()
        && (!users.users.is_empty() || !users.linux.is_empty())
    {
        cfg.managers = users.users;
        cfg.linux = users.linux;
    }
    if cfg.shells.is_empty() && !shells.is_empty() {
        cfg.shells = shells;
    }
    if cfg.notify_channel.is_none() {
        cfg.notify_channel = notify;
    }
}

pub(crate) async fn persist_runtime(data: &Data) -> Result<(), Error> {
    let mut cfg = load_file_config();
    {
        let a = data.allowed.read().await;
        cfg.managers = a.users.clone();
        cfg.linux = a.linux.clone();
    }
    {
        let s = data.settings.read().await;
        cfg.notify_channel = s.notify_channel;
        cfg.war_mode = s.war_mode;
        cfg.sayas_enabled = s.sayas_enabled;
        cfg.ai_enabled = s.ai_enabled;
        cfg.ai_model = s.ai_model.clone();
        cfg.ollama_host = s.ollama_host.clone();
        cfg.gemini_api_key = s.gemini_api_key.clone();
    }
    {
        let m = data.shells.read().await;
        cfg.shells = m.clone();
    }
    let text = toml::to_string_pretty(&cfg)?;
    let path = config_file_path();
    let path_str = path.to_string_lossy().to_string();
    let _ = tokio::fs::remove_file(&path).await;
    save_json(&path_str, text).await
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
    if cfg.owner_id == Some(0) {
        cfg.owner_id = None;
    }
    if !crate::ai::valid_model_name(&cfg.ai_model) {
        cfg.ai_model = crate::ai::default_model();
    } else {
        cfg.ai_model = cfg.ai_model.trim().to_string();
    }
    let host = cfg.ollama_host.trim().trim_end_matches('/').to_string();
    cfg.ollama_host = if host.is_empty() {
        crate::ai::default_host()
    } else {
        host
    };
    cfg.gemini_api_key = cfg.gemini_api_key.trim().to_string();
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

pub(crate) fn ensure_config_template() {
    let path = config_file_path();
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(path, CONFIG_TEMPLATE);
    lock_config_private();
}

const CONFIG_TEMPLATE: &str = "owner_id = 0\ndiscord_token = \"\"\nvm_name = \"\"\nblocked_ids = []\nwebhook_urls = []\nwar_mode = false\nai_enabled = false\nai_model = \"llama3.1\"\nollama_host = \"http://127.0.0.1:11434\"\ngemini_api_key = \"\"\nmanagers = []\n\n[linux]\n\n[shells]\n";

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
    fn template_is_empty_by_default() {
        let c = normalize_file_config(parse(CONFIG_TEMPLATE));
        assert_eq!(c.owner_id, None);
        assert_eq!(c.discord_token, None);
        assert_eq!(c.vm_name, None);
        assert!(c.blocked_ids.is_empty());
        assert!(c.webhook_urls.is_empty());
        assert!(!c.war_mode);
        assert!(!c.sayas_enabled);
        assert!(c.managers.is_empty());
        assert!(c.linux.is_empty());
        assert!(c.shells.is_empty());
        assert_eq!(c.notify_channel, None);
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

    #[test]
    fn war_mode_defaults_off() {
        assert!(!BotSettings::default().war_mode);
        let c = parse("notify_channel = 5\n");
        assert!(!c.war_mode);
        let c = parse("war_mode = true\n");
        assert!(c.war_mode);
    }

    fn sample_legacy() -> (AllowedFile, std::collections::HashMap<String, String>, Option<u64>) {
        let users = AllowedFile {
            users: vec![11, 22],
            linux: [("11".to_string(), "amy".to_string())].into_iter().collect(),
        };
        let mut shells = std::collections::HashMap::new();
        shells.insert("11".to_string(), "fish".to_string());
        (users, shells, Some(99))
    }

    #[test]
    fn legacy_import_fills_empty_config() {
        let mut cfg = FileConfig::default();
        let (users, shells, notify) = sample_legacy();
        apply_legacy_import(&mut cfg, users, shells, notify);
        assert_eq!(cfg.managers, vec![11, 22]);
        assert_eq!(cfg.linux.get("11").map(String::as_str), Some("amy"));
        assert_eq!(cfg.shells.get("11").map(String::as_str), Some("fish"));
        assert_eq!(cfg.notify_channel, Some(99));
    }

    #[test]
    fn legacy_import_never_overwrites_config() {
        let mut cfg = FileConfig {
            managers: vec![1],
            linux: [("1".to_string(), "zed".to_string())].into_iter().collect(),
            shells: [("1".to_string(), "bash".to_string())].into_iter().collect(),
            notify_channel: Some(7),
            ..Default::default()
        };
        let (users, shells, notify) = sample_legacy();
        apply_legacy_import(&mut cfg, users, shells, notify);
        assert_eq!(cfg.managers, vec![1]);
        assert_eq!(cfg.linux.get("1").map(String::as_str), Some("zed"));
        assert_eq!(cfg.shells.get("1").map(String::as_str), Some("bash"));
        assert_eq!(cfg.notify_channel, Some(7));
    }

    #[test]
    fn config_round_trip_preserves_everything() {
        let cfg = FileConfig {
            owner_id: Some(1),
            blocked_ids: vec![2],
            webhook_urls: vec!["https://discord.com/api/webhooks/3/tok".to_string()],
            discord_token: Some("tok".to_string()),
            vm_name: Some("artix".to_string()),
            war_mode: true,
            sayas_enabled: true,
            ai_enabled: true,
            ai_model: "qwen2.5-coder:7b".to_string(),
            ollama_host: "http://127.0.0.1:11434".to_string(),
            gemini_api_key: "gkey".to_string(),
            notify_channel: Some(4),
            managers: vec![5],
            linux: [("5".to_string(), "sam".to_string())].into_iter().collect(),
            shells: [("5".to_string(), "fish".to_string())].into_iter().collect(),
        };
        let text = toml::to_string_pretty(&cfg).expect("serializes");
        let back: FileConfig = toml::from_str(&text).expect("reparses");
        assert_eq!(cfg, back);
    }
}

