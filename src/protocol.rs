use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelState {
    pub level_db: f32,
    pub muted: bool,
    pub compressor_percent: f32,
    pub reverb_send_percent: f32,
    #[serde(default)]
    pub mon1_send_db: Option<f32>,
    #[serde(default)]
    pub mon2_send_db: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MixerState {
    pub mic1: ChannelState,
    pub mic2: ChannelState,
    pub music: InputLevelState,
    pub main: LevelState,
    #[serde(default)]
    pub mon1: Option<LevelState>,
    #[serde(default)]
    pub mon2: Option<LevelState>,
    pub reverb: ReverbState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LevelState {
    pub level_db: f32,
    pub muted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InputLevelState {
    pub level_db: f32,
    pub muted: bool,
    #[serde(default)]
    pub mon1_send_db: Option<f32>,
    #[serde(default)]
    pub mon2_send_db: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReverbState {
    pub preset: ReverbPreset,
    pub decay_percent: f32,
    pub return_level_db: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReverbPreset {
    Default,
    MaleVocal,
    FemaleVocal,
    Chorus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingDirective {
    pub enabled: bool,
    pub recording_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackCommand {
    pub schema_version: u8,
    pub command_id: String,
    pub route_epoch: u64,
    pub action: PlaybackAction,
    pub queue_item_id: Option<String>,
    pub asset_id: Option<String>,
    pub media_url: Option<String>,
    pub position_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub recording: Option<RecordingDirective>,
    pub recording_session_id: Option<String>,
    pub recording_disposition: Option<String>,
    pub previous_recording_session_id: Option<String>,
    pub previous_recording_disposition: Option<String>,
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackAction {
    Play,
    Pause,
    Resume,
    Stop,
    Seek,
    Next,
    Switch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "hello.ack")]
    HelloAck {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "routeEpoch", default)]
        route_epoch: u64,
        #[serde(flatten)]
        fields: BTreeMap<String, Value>,
    },
    #[serde(rename = "playback.command")]
    PlaybackCommand(Box<PlaybackCommand>),
    #[serde(rename = "mixer.apply")]
    MixerApply {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        revision: u64,
        desired: MixerState,
    },
    #[serde(rename = "recording.cancel")]
    RecordingCancel {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "recordingSessionId")]
        recording_session_id: String,
        reason: String,
    },
    #[serde(rename = "recording.upload-authorized")]
    RecordingUploadAuthorized {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "recordingSessionId")]
        recording_session_id: String,
        #[serde(rename = "uploadUrl")]
        upload_url: Option<String>,
        error: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AgentMessage<'a> {
    #[serde(rename = "hello")]
    Hello {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "agentId")]
        agent_id: &'a str,
        version: &'a str,
        #[serde(rename = "snapshotSlot")]
        snapshot_slot: u8,
        capabilities: &'a Value,
        health: &'a Value,
    },
    #[serde(rename = "heartbeat")]
    Heartbeat {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "agentId")]
        agent_id: &'a str,
        health: &'a Value,
    },
    #[serde(rename = "playback.status")]
    PlaybackStatus {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "commandId")]
        command_id: &'a str,
        #[serde(rename = "routeEpoch")]
        route_epoch: u64,
        state: &'a str,
        #[serde(rename = "sampleFrame")]
        sample_frame: u64,
        #[serde(rename = "sampleRate")]
        sample_rate: u32,
        #[serde(rename = "durationMs")]
        duration_ms: u64,
        #[serde(rename = "anchorMonotonicMs")]
        anchor_monotonic_ms: u64,
    },
    #[serde(rename = "playback.error")]
    PlaybackError {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "commandId")]
        command_id: &'a str,
        #[serde(rename = "routeEpoch")]
        route_epoch: u64,
        error: &'a str,
    },
    #[serde(rename = "mixer.sent")]
    MixerSent {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        revision: u64,
    },
    #[serde(rename = "mixer.error")]
    MixerError {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        revision: u64,
        error: &'a str,
    },
    #[serde(rename = "recording.status")]
    RecordingStatus {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "recordingSessionId")]
        recording_session_id: &'a str,
        state: &'a str,
        #[serde(rename = "maxFrame")]
        max_frame: u64,
        error: Option<&'a str>,
    },
    #[serde(rename = "recording.upload-request")]
    RecordingUploadRequest {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        #[serde(rename = "recordingSessionId")]
        recording_session_id: &'a str,
        sha256: &'a str,
        #[serde(rename = "sizeBytes")]
        size_bytes: u64,
    },
}
