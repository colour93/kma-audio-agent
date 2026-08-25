use std::{
    collections::HashMap,
    fmt,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{fs::File, net::TcpStream};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Error as WebSocketError, Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::{
    VERSION,
    config::Config,
    credentials::{Credentials, store_private},
    device::{AudioDevice, Calibration, RecordingArtifact, RecordingStart, create_device},
    playback::{ApplyResult, MixerRevision, PlaybackCoordinator},
    protocol::{AgentMessage, PlaybackAction, PlaybackCommand, SCHEMA_VERSION, ServerMessage},
    recording::recover_spool,
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Default)]
struct ConnectionState {
    playback: PlaybackCoordinator,
    mixer: MixerRevision,
    active_command: Option<(String, u64, u64)>,
    active_recording: Option<String>,
}

#[derive(Debug)]
struct CredentialsRevoked;

impl fmt::Display for CredentialsRevoked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("audio agent credentials were revoked")
    }
}

impl std::error::Error for CredentialsRevoked {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingState {
    agent_id: Uuid,
    pairing_id: String,
    code: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingCreated {
    id: String,
    code: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingClaimed {
    agent_id: Option<Uuid>,
    room_id: Option<String>,
    token: Option<String>,
    status: Option<String>,
}

pub async fn resolve_server(config: &Config) -> Result<String> {
    if let Some(url) = &config.server_url {
        return Ok(url.trim_end_matches('/').to_owned());
    }
    tokio::task::spawn_blocking(discover_server)
        .await
        .context("join mDNS discovery")?
}

fn discover_server() -> Result<String> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse("_kma._tcp.local.")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut servers = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                if let Some(address) = info.get_addresses().iter().find(|ip| !ip.is_loopback()) {
                    servers.push(format!("http://{}:{}", address, info.get_port()));
                    servers.sort();
                    servers.dedup();
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse("_kma._tcp.local.");
    let _ = daemon.shutdown();
    match servers.as_slice() {
        [server] => Ok(server.clone()),
        [] => Err(anyhow!("no _kma._tcp Server discovered")),
        _ => Err(anyhow!(
            "multiple _kma._tcp Servers discovered; configure server_url explicitly"
        )),
    }
}

pub async fn ensure_credentials(config: &Config, server_url: &str) -> Result<Credentials> {
    if let Some(credentials) = Credentials::load(&config.credentials_path()).await? {
        return Ok(credentials);
    }
    tokio::fs::create_dir_all(&config.data_dir).await?;
    let pairing_path = config.data_dir.join("pairing.json");
    let client = reqwest::Client::new();
    let pairing = match tokio::fs::read(&pairing_path).await {
        Ok(bytes) => serde_json::from_slice::<PairingState>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let agent_id = Uuid::new_v4();
            let created = client
                .post(format!("{server_url}/api/v1/audio-agent/pairing"))
                .json(&json!({
                    "agentId": agent_id,
                    "agentName": config.agent_name(),
                    "roomId": config.room_id,
                }))
                .send()
                .await?
                .error_for_status()?
                .json::<PairingCreated>()
                .await?;
            let pairing = PairingState {
                agent_id,
                pairing_id: created.id,
                code: created.code,
                expires_at: created.expires_at,
            };
            store_private(&pairing_path, &serde_json::to_vec_pretty(&pairing)?).await?;
            pairing
        }
        Err(error) => return Err(error.into()),
    };
    tracing::info!(
        pairing_id = %pairing.pairing_id,
        pairing_code = %pairing.code,
        expires_at = %pairing.expires_at,
        "audio agent pairing approval required"
    );
    loop {
        let response = client
            .post(format!(
                "{server_url}/api/v1/audio-agent/pairing/{}/claim",
                pairing.pairing_id
            ))
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            let claim = response.json::<PairingClaimed>().await?;
            if let (Some(agent_id), Some(room_id), Some(token)) =
                (claim.agent_id, claim.room_id, claim.token)
            {
                let credentials = Credentials {
                    agent_id,
                    room_id,
                    token,
                };
                credentials.store(&config.credentials_path()).await?;
                let _ = tokio::fs::remove_file(&pairing_path).await;
                return Ok(credentials);
            }
            if claim.status.as_deref() != Some("pending") {
                return Err(anyhow!("invalid pairing claim response"));
            }
        } else if status.as_u16() != 202 {
            return Err(anyhow!("pairing claim failed with {status}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

pub async fn run(config: Config) -> Result<()> {
    let server_url = resolve_server(&config).await?;
    'pairing: loop {
        let credentials = ensure_credentials(&config, &server_url).await?;
        let calibration = Calibration::load(&config.calibration_path()).await?;
        let mut device = create_device(&config, server_url.clone(), credentials.token.clone())?;
        let health = serde_json::to_value(device.probe(&config, calibration.is_some()).await)?;
        let recovered = recover_spool(&config.spool_dir())?;
        let mut artifacts = HashMap::new();
        for (_, manifest) in recovered {
            if let Some(artifact) = device.recover_recording(&manifest).await? {
                artifacts.insert(artifact.recording_session_id.clone(), artifact);
            }
        }
        let mut disconnected_since: Option<Instant> = None;
        let mut interrupted = false;
        let mut state = ConnectionState::default();
        loop {
            let mut established = false;
            let result = connect_and_run(
                &config,
                &server_url,
                &credentials,
                &health,
                calibration.as_ref(),
                device.as_mut(),
                &mut artifacts,
                &mut state,
                &mut established,
            )
            .await;
            match result {
                Ok(()) => disconnected_since = None,
                Err(error) if error.downcast_ref::<CredentialsRevoked>().is_some() => {
                    stop_and_preserve_recording(device.as_mut(), &mut state, &mut artifacts).await;
                    match tokio::fs::remove_file(config.credentials_path()).await {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error).context("remove revoked credentials"),
                    }
                    tracing::warn!("audio agent was revoked; returning to pairing state");
                    continue 'pairing;
                }
                Err(error) => {
                    tracing::warn!(error = %error, "audio agent connection lost");
                    if established {
                        disconnected_since = Some(Instant::now());
                        interrupted = false;
                    }
                    let since = disconnected_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_secs(3) && !interrupted {
                        interrupted = true;
                        stop_and_preserve_recording(device.as_mut(), &mut state, &mut artifacts)
                            .await;
                        tracing::warn!(
                            "disconnect grace expired; playback stopped and active recording preserved"
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            if disconnected_since.is_none() {
                interrupted = false;
            }
        }
    }
}

async fn stop_and_preserve_recording(
    device: &mut dyn AudioDevice,
    state: &mut ConnectionState,
    artifacts: &mut HashMap<String, RecordingArtifact>,
) {
    if let Err(error) = device.playback(&synthetic_stop()).await {
        tracing::warn!(error = %error, "failed to stop playback");
    }
    state.active_command = None;
    if let Some(recording_id) = state.active_recording.take() {
        match device.finish_recording(&recording_id, true).await {
            Ok(Some(artifact)) => {
                artifacts.insert(recording_id, artifact);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(error = %error, "failed to preserve interrupted recording");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_and_run(
    config: &Config,
    server_url: &str,
    credentials: &Credentials,
    health: &serde_json::Value,
    calibration: Option<&Calibration>,
    device: &mut dyn AudioDevice,
    artifacts: &mut HashMap<String, RecordingArtifact>,
    state: &mut ConnectionState,
    established: &mut bool,
) -> Result<()> {
    let mut ws_url = url::Url::parse(server_url)?;
    ws_url
        .set_scheme(if ws_url.scheme() == "https" {
            "wss"
        } else {
            "ws"
        })
        .map_err(|_| anyhow!("invalid Server URL scheme"))?;
    ws_url.set_path("/api/v1/audio-agent/connect");
    let mut request = ws_url.as_str().into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", credentials.token))?,
    );
    let (mut socket, _) = match connect_async(request).await {
        Ok(connected) => connected,
        Err(WebSocketError::Http(response)) if matches!(response.status().as_u16(), 401 | 403) => {
            return Err(CredentialsRevoked.into());
        }
        Err(error) => return Err(error.into()),
    };
    *established = true;
    let agent_id = credentials.agent_id.to_string();
    let capabilities = json!({
        "version": VERSION,
        "platform": "linux-amd64",
        "sampleRate": 48_000,
        "captureChannels": 2,
        "formats": ["mp3", "flac"],
    });
    send_agent(
        &mut socket,
        &AgentMessage::Hello {
            schema_version: SCHEMA_VERSION,
            agent_id: &agent_id,
            version: VERSION,
            snapshot_slot: config.snapshot_slot,
            capabilities: &capabilities,
            health,
        },
    )
    .await?;
    for artifact in artifacts.values() {
        send_recording_status(
            &mut socket,
            artifact,
            if artifact.interrupted {
                "interrupted"
            } else {
                "encoding"
            },
            None,
        )
        .await?;
        request_upload(&mut socket, artifact).await?;
    }

    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    let mut status = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                send_agent(&mut socket, &AgentMessage::Heartbeat {
                    schema_version: SCHEMA_VERSION,
                    agent_id: &agent_id,
                    health,
                }).await?;
            }
            _ = status.tick() => {
                if let (Some((command_id, route_epoch, duration_ms)), Some((frame, rate, monotonic_ms))) =
                    (state.active_command.as_ref(), device.anchor())
                {
                    let ended = *duration_ms > 0 && frame >= *duration_ms * rate as u64 / 1_000;
                    send_agent(&mut socket, &AgentMessage::PlaybackStatus {
                        schema_version: SCHEMA_VERSION,
                        command_id,
                        route_epoch: *route_epoch,
                        state: if ended { "ended" } else { "playing" },
                        sample_frame: frame,
                        sample_rate: rate,
                        duration_ms: *duration_ms,
                        anchor_monotonic_ms: monotonic_ms,
                    }).await?;
                    if ended {
                        if let Some(recording_id) = state.active_recording.take()
                            && let Some(artifact) = device.finish_recording(&recording_id, false).await?
                        {
                            artifacts.insert(recording_id.clone(), artifact);
                            let artifact = &artifacts[&recording_id];
                            send_recording_status(&mut socket, artifact, "encoding", None).await?;
                            request_upload(&mut socket, artifact).await?;
                        }
                        state.active_command = None;
                    }
                }
            }
            message = socket.next() => {
                let message = message.ok_or_else(|| anyhow!("Server closed WebSocket"))??;
                if let Message::Close(frame) = &message {
                    if frame.as_ref().is_some_and(|value| value.reason.contains("revoked")) {
                        return Err(CredentialsRevoked.into());
                    }
                    return Err(anyhow!("Server closed WebSocket"));
                }
                let Message::Text(text) = message else { continue; };
                let server_message: ServerMessage = serde_json::from_str(&text)
                    .context("decode Server message")?;
                match server_message {
                    ServerMessage::PlaybackCommand(command) => {
                        match state.playback.apply(&command) {
                            ApplyResult::Duplicate => continue,
                            ApplyResult::StaleEpoch => {
                                send_agent(&mut socket, &AgentMessage::PlaybackError {
                                    schema_version: SCHEMA_VERSION,
                                    command_id: &command.command_id,
                                    route_epoch: command.route_epoch,
                                    error: "stale_route_epoch",
                                }).await?;
                                continue;
                            }
                            ApplyResult::Apply => {}
                        }
                        if command.previous_recording_disposition.as_deref() == Some("save") {
                            if let Some(recording_id) = command.previous_recording_session_id.as_deref()
                                && let Some(artifact) = device.finish_recording(recording_id, false).await?
                            {
                                artifacts.insert(recording_id.to_owned(), artifact);
                                let artifact = &artifacts[recording_id];
                                send_recording_status(&mut socket, artifact, "encoding", None).await?;
                                request_upload(&mut socket, artifact).await?;
                            }
                            state.active_recording = None;
                        }
                        if command.recording_disposition.as_deref() == Some("discard") {
                            if let Some(recording_id) = command.recording_session_id.as_deref() {
                                device.cancel_recording(recording_id).await?;
                                send_agent(&mut socket, &AgentMessage::RecordingStatus {
                                    schema_version: SCHEMA_VERSION,
                                    recording_session_id: recording_id,
                                    state: "discarded",
                                    max_frame: 0,
                                    error: None,
                                }).await?;
                            }
                            state.active_recording = None;
                        }
                        if matches!(command.action, PlaybackAction::Play)
                            && let Some(directive) = &command.recording
                            && directive.enabled
                            && let Some(recording_id) = &directive.recording_session_id
                        {
                            if let Some(calibration) = calibration {
                                device.start_recording(RecordingStart {
                                    recording_session_id: recording_id.clone(),
                                    queue_item_id: command.queue_item_id.clone().unwrap_or_default(),
                                    spool_dir: config.spool_dir(),
                                    playback_to_capture_offset_frames: calibration.playback_to_capture_offset_frames,
                                }).await?;
                                state.active_recording = Some(recording_id.clone());
                                send_agent(&mut socket, &AgentMessage::RecordingStatus {
                                    schema_version: SCHEMA_VERSION,
                                    recording_session_id: recording_id,
                                    state: "recording",
                                    max_frame: 0,
                                    error: None,
                                }).await?;
                            } else {
                                send_agent(&mut socket, &AgentMessage::RecordingStatus {
                                    schema_version: SCHEMA_VERSION,
                                    recording_session_id: recording_id,
                                    state: "failed",
                                    max_frame: 0,
                                    error: Some("calibration_missing"),
                                }).await?;
                            }
                        }
                        let duration_ms = command.duration_ms.unwrap_or(0);
                        match device.playback(&command).await {
                            Ok(()) => {
                                if matches!(command.action, PlaybackAction::Play | PlaybackAction::Switch | PlaybackAction::Resume) {
                                    state.active_command = Some((command.command_id.clone(), command.route_epoch, duration_ms));
                                } else if matches!(command.action, PlaybackAction::Stop) {
                                    state.active_command = None;
                                }
                            }
                            Err(error) => {
                                let error = error.to_string();
                                send_agent(&mut socket, &AgentMessage::PlaybackError {
                                    schema_version: SCHEMA_VERSION,
                                    command_id: &command.command_id,
                                    route_epoch: command.route_epoch,
                                    error: &error,
                                }).await?;
                            }
                        }
                    }
                    ServerMessage::MixerApply { revision, desired, .. } => {
                        match state.mixer.apply(revision) {
                            ApplyResult::StaleEpoch | ApplyResult::Duplicate => continue,
                            ApplyResult::Apply => {}
                        }
                        match device.apply_mixer(&desired, config.snapshot_slot).await {
                            Ok(()) => send_agent(&mut socket, &AgentMessage::MixerSent {
                                schema_version: SCHEMA_VERSION,
                                revision,
                            }).await?,
                            Err(error) => {
                                let error = error.to_string();
                                send_agent(&mut socket, &AgentMessage::MixerError {
                                    schema_version: SCHEMA_VERSION,
                                    revision,
                                    error: &error,
                                }).await?;
                            }
                        }
                    }
                    ServerMessage::RecordingCancel { recording_session_id, .. } => {
                        device.cancel_recording(&recording_session_id).await?;
                        state.active_recording = None;
                        send_agent(&mut socket, &AgentMessage::RecordingStatus {
                            schema_version: SCHEMA_VERSION,
                            recording_session_id: &recording_session_id,
                            state: "discarded",
                            max_frame: 0,
                            error: None,
                        }).await?;
                    }
                    ServerMessage::RecordingUploadAuthorized { recording_session_id, upload_url, error, .. } => {
                        if let Some(error) = error {
                            tracing::warn!(recording_session_id, error, "recording upload was not authorized");
                        } else if let (Some(url), Some(artifact)) = (upload_url, artifacts.get(&recording_session_id)) {
                            upload_artifact(server_url, &credentials.token, &url, artifact).await?;
                            cleanup_artifact(artifact).await;
                            artifacts.remove(&recording_session_id);
                        }
                    }
                    ServerMessage::HelloAck { .. } | ServerMessage::Unknown => {}
                }
            }
        }
    }
}

async fn send_agent(socket: &mut Socket, message: &AgentMessage<'_>) -> Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(message)?.into()))
        .await?;
    Ok(())
}

async fn send_recording_status(
    socket: &mut Socket,
    artifact: &RecordingArtifact,
    state: &str,
    error: Option<&str>,
) -> Result<()> {
    send_agent(
        socket,
        &AgentMessage::RecordingStatus {
            schema_version: SCHEMA_VERSION,
            recording_session_id: &artifact.recording_session_id,
            state,
            max_frame: artifact.max_frame,
            error,
        },
    )
    .await
}

async fn request_upload(socket: &mut Socket, artifact: &RecordingArtifact) -> Result<()> {
    let (sha256, size_bytes) = sha256_file(&artifact.flac_path).await?;
    send_agent(
        socket,
        &AgentMessage::RecordingUploadRequest {
            schema_version: SCHEMA_VERSION,
            recording_session_id: &artifact.recording_session_id,
            sha256: &sha256,
            size_bytes,
        },
    )
    .await
}

async fn upload_artifact(
    server_url: &str,
    token: &str,
    upload_url: &str,
    artifact: &RecordingArtifact,
) -> Result<()> {
    let url = url::Url::parse(server_url)?.join(upload_url)?;
    let (sha256, size) = sha256_file(&artifact.flac_path).await?;
    let file = File::open(&artifact.flac_path).await?;
    reqwest::Client::new()
        .put(url)
        .bearer_auth(token)
        .header("x-sha256", sha256)
        .header(reqwest::header::CONTENT_LENGTH, size)
        .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn cleanup_artifact(artifact: &RecordingArtifact) {
    for path in [
        &artifact.manifest_path,
        &artifact.raw_path,
        &artifact.flac_path,
    ] {
        if let Err(error) = tokio::fs::remove_file(path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), error = %error, "failed to clean completed recording spool file");
        }
    }
}

async fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(path)?;
        let mut sha256 = Sha256::new();
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut size = 0_u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            sha256.update(&buffer[..count]);
            size += count as u64;
        }
        Ok::<_, std::io::Error>((format!("{:x}", sha256.finalize()), size))
    })
    .await?
    .map_err(Into::into)
}

fn synthetic_stop() -> PlaybackCommand {
    PlaybackCommand {
        schema_version: SCHEMA_VERSION,
        command_id: "disconnect-timeout".to_owned(),
        route_epoch: 0,
        action: PlaybackAction::Stop,
        queue_item_id: None,
        asset_id: None,
        media_url: None,
        position_ms: None,
        duration_ms: None,
        recording: None,
        recording_session_id: None,
        recording_disposition: None,
        previous_recording_session_id: None,
        previous_recording_disposition: None,
        metadata: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completed_upload_cleans_all_spool_files() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = RecordingArtifact {
            recording_session_id: "take".to_owned(),
            raw_path: directory.path().join("take.s32le"),
            flac_path: directory.path().join("take.flac"),
            manifest_path: directory.path().join("take.json"),
            max_frame: 42,
            interrupted: false,
        };
        for path in [
            &artifact.raw_path,
            &artifact.flac_path,
            &artifact.manifest_path,
        ] {
            tokio::fs::write(path, b"test").await.unwrap();
        }

        cleanup_artifact(&artifact).await;

        assert!(!artifact.raw_path.exists());
        assert!(!artifact.flac_path.exists());
        assert!(!artifact.manifest_path.exists());
    }
}
