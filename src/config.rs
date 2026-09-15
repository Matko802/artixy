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
    pub(crate) ollama_api_key: String,
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
            ollama_api_key: String::new(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Data {
    pub(crate) allowed: std::sync::Arc<tokio::sync::RwLock<Allowed>>,
    pub(crate) vm: std::sync::Arc<tokio::sync::RwLock<String>>,
    pub(crate) live: LiveMap,
    pub(crate) settings: std::sync::Arc<tokio::sync::RwLock<BotSettings>>,
    pub(crate) shells: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
}

pub(crate) fn config_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_file_path())
        .and_then(|m| m.modified())
        .ok()
}

pub(crate) async fn apply_file_config(data: &Data, cfg: &FileConfig) -> Vec<String> {
    let mut changed = Vec::new();
    {
        let mut a = data.allowed.write().await;
        if let Some(owner) = cfg.owner_id {
            if owner != 0 && owner != a.owner {
                a.owner = owner;
                changed.push("owner".to_string());
            }
        }
        if a.users != cfg.managers {
            a.users = cfg.managers.clone();
            changed.push("managers".to_string());
        }
        if a.linux != cfg.linux {
            a.linux = cfg.linux.clone();
            changed.push("linux".to_string());
        }
        if a.blocked != cfg.blocked_ids {
            a.blocked = cfg.blocked_ids.clone();
            changed.push("blocked_ids".to_string());
        }
        if a.admins != cfg.admin_ids {
            a.admins = cfg.admin_ids.clone();
            changed.push("admin_ids".to_string());
        }
    }
    {
        let mut s = data.settings.write().await;
        if s.notify_channel != cfg.notify_channel {
            s.notify_channel = cfg.notify_channel;
            changed.push("notify_channel".to_string());
        }
        if s.war_mode != cfg.war_mode {
            s.war_mode = cfg.war_mode;
            changed.push("war_mode".to_string());
        }
        if s.sayas_enabled != cfg.sayas_enabled {
            s.sayas_enabled = cfg.sayas_enabled;
            changed.push("sayas_enabled".to_string());
        }
        if s.ai_enabled != cfg.ai_enabled {
            s.ai_enabled = cfg.ai_enabled;
            changed.push("ai_enabled".to_string());
        }
        if s.ai_model != cfg.ai_model {
            s.ai_model = cfg.ai_model.clone();
            changed.push("ai_model".to_string());
        }
        if s.ollama_host != cfg.ollama_host {
            s.ollama_host = cfg.ollama_host.clone();
            changed.push("ollama_host".to_string());
        }
        if s.ollama_api_key != cfg.ollama_api_key {
            s.ollama_api_key = cfg.ollama_api_key.clone();
            changed.push("ollama_api_key".to_string());
        }
    }
    {
        let mut m = data.shells.write().await;
        if *m != cfg.shells {
            *m = cfg.shells.clone();
            changed.push("shells".to_string());
        }
    }
    {
        let vm_name = cfg
            .vm_name
            .clone()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_default();
        let mut vm = data.vm.write().await;
        if *vm != vm_name {
            *vm = vm_name;
            changed.push("vm_name".to_string());
        }
    }
    if crate::webhook::current_webhook_urls() != cfg.webhook_urls {
        crate::webhook::set_webhook_urls(cfg.webhook_urls.clone());
        changed.push("webhook_urls".to_string());
    }
    changed
}

pub(crate) async fn watch_config(data: Data) {
    let mut last = config_mtime();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let cur = config_mtime();
        if cur == last {
            continue;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let stable = config_mtime();
        last = stable;
        if stable.is_none() {
            eprintln!("config: file missing, keeping runtime settings");
            continue;
        }
        let cfg = load_file_config();
        let changed = apply_file_config(&data, &cfg).await;
        if changed.is_empty() {
            eprintln!("config: file changed, no effective updates");
        } else {
            eprintln!("config: hot-applied {}", changed.join(", "));
        }
    }
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
    pub(crate) admins: Vec<u64>,
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
    pub(crate) admin_ids: Vec<u64>,
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
    pub(crate) ollama_api_key: String,
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
        cfg.admin_ids = a.admins.clone();
    }
    {
        let s = data.settings.read().await;
        cfg.notify_channel = s.notify_channel;
        cfg.war_mode = s.war_mode;
        cfg.sayas_enabled = s.sayas_enabled;
        cfg.ai_enabled = s.ai_enabled;
        cfg.ai_model = s.ai_model.clone();
        cfg.ollama_host = s.ollama_host.clone();
        cfg.ollama_api_key = s.ollama_api_key.clone();
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

pub(crate) fn elevated_allowed(owner: u64, admins: &[u64], id: u64) -> bool {
    id == owner || admins.contains(&id)
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
    cfg.ollama_api_key = cfg.ollama_api_key.trim().to_string();
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

const CONFIG_TEMPLATE: &str = "owner_id = 0\ndiscord_token = \"\"\nvm_name = \"\"\nblocked_ids = []\nwebhook_urls = []\nwar_mode = false\nai_enabled = false\nai_model = \"llama3.1\"\nollama_host = \"http://127.0.0.1:11434\"\nollama_api_key = \"\"\nmanagers = []\nadmin_ids = []\n\n[linux]\n\n[shells]\n";

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

