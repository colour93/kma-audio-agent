use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_data_dir() -> PathBuf {
    PathBuf::from("/var/lib/kma-audio-agent")
}

fn default_snapshot_slot() -> u8 {
    15
}

fn default_room_id() -> String {
    "default".to_owned()
}

fn default_audio_device() -> String {
    "FLOW 8".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Config {
    pub server_url: Option<String>,
    pub data_dir: PathBuf,
    pub room_id: String,
    pub agent_name: Option<String>,
    pub snapshot_slot: u8,
    pub audio_device: String,
    pub midi_device: String,
    pub alsa_playback_device: String,
    pub alsa_capture_device: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_url: None,
            data_dir: default_data_dir(),
            room_id: default_room_id(),
            agent_name: None,
            snapshot_slot: default_snapshot_slot(),
            audio_device: default_audio_device(),
            midi_device: default_audio_device(),
            alsa_playback_device: "default".to_owned(),
            alsa_capture_device: "default".to_owned(),
        }
    }
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self> {
        let raw = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("read config {}", path.display()))?;
        let config: Self = toml::from_str(&raw).context("parse config")?;
        anyhow::ensure!(
            (1..=15).contains(&config.snapshot_slot),
            "snapshot_slot must be between 1 and 15"
        );
        Ok(config)
    }

    pub fn agent_name(&self) -> String {
        self.agent_name.clone().unwrap_or_else(|| {
            hostname::get()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
    }

    pub fn credentials_path(&self) -> PathBuf {
        self.data_dir.join("credentials.json")
    }

    pub fn calibration_path(&self) -> PathBuf {
        self.data_dir.join("calibration.json")
    }

    pub fn spool_dir(&self) -> PathBuf {
        self.data_dir.join("spool")
    }
}
