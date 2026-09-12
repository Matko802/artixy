use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{live::LiveMap, util::random_suffix, Error};

#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct BotSettings {
    #[serde(default)]
    pub(crate) notify_channel: Option<u64>,
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

