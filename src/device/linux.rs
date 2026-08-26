use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use alsa::{Direction, pcm::PCM};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use gstreamer::{self as gst, prelude::*};
use gstreamer_app as gst_app;
use midir::MidiOutput;

use super::{AudioDevice, HealthItem, ProbeReport, RecordingArtifact, RecordingStart};
use crate::{
    config::Config,
    midi::{mixer_messages, snapshot_program},
    protocol::{MixerState, PlaybackAction, PlaybackCommand},
    recording::{
        CHANNELS, DEVICE_CAPTURE_CHANNELS, RecordingState, SAMPLE_RATE, SparseTimeline,
        SpoolManifest, capture_to_song_frame,
    },
};

const DEVICE_CAPTURE_FRAME_BYTES: usize = DEVICE_CAPTURE_CHANNELS as usize * size_of::<i32>();

// FLOW 8's first two USB capture channels are Mic1 and Mic2. Keep only those
// channels in the recording spool; the remaining channels are mixer returns.
fn select_mic_channels(bytes: &[u8]) -> Vec<i32> {
    bytes
        .chunks_exact(DEVICE_CAPTURE_FRAME_BYTES)
        .flat_map(|frame| {
            [
                i32::from_le_bytes(frame[0..4].try_into().unwrap()),
                i32::from_le_bytes(frame[4..8].try_into().unwrap()),
            ]
        })
        .collect()
}

struct CaptureSession {
    pipeline: gst::Pipeline,
    timeline: Arc<Mutex<SparseTimeline>>,
    cursor: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    manifest_path: PathBuf,
    manifest: SpoolManifest,
}

pub struct Flow8Device {
    config: Config,
    server_url: String,
    token: String,
    client: reqwest::Client,
    player: Option<gst::Element>,
    active_command_id: Option<String>,
    captures: HashMap<String, CaptureSession>,
    started_at: Instant,
}

impl Flow8Device {
    pub fn new(config: Config, server_url: String, token: String) -> Result<Self> {
        gst::init().context("initialize GStreamer")?;
        Ok(Self {
            config,
            server_url,
            token,
            client: reqwest::Client::new(),
            player: None,
            active_command_id: None,
            captures: HashMap::new(),
            started_at: Instant::now(),
        })
    }

    fn alsa_health(&self, direction: Direction, name: &str) -> HealthItem {
        match PCM::new(name, direction, true) {
            Ok(_) => HealthItem::ok(),
            Err(error) => HealthItem::failed(error.to_string()),
        }
    }

    fn midi_health(&self) -> HealthItem {
        match self.find_midi_port() {
            Ok(_) => HealthItem::ok(),
            Err(error) => HealthItem::failed(error.to_string()),
        }
    }

    fn find_midi_port(&self) -> Result<(MidiOutput, midir::MidiOutputPort)> {
        let output = MidiOutput::new("kma-audio-agent")?;
        let port = output
            .ports()
            .into_iter()
            .find(|port| {
                output
                    .port_name(port)
                    .map(|name| name.contains(&self.config.midi_device))
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("FLOW 8 MIDI output was not found"))?;
        Ok((output, port))
    }

    async fn cache_media(&self, command: &PlaybackCommand) -> Result<PathBuf> {
        let media_url = command
            .media_url
            .as_deref()
            .ok_or_else(|| anyhow!("play command is missing mediaUrl"))?;
        let url = url::Url::parse(&self.server_url)?.join(media_url)?;
        let extension = if media_url.ends_with(".mp3") {
            "mp3"
        } else {
            "audio"
        };
        let asset_id = command.asset_id.as_deref().unwrap_or(&command.command_id);
        let cache_dir = self.config.data_dir.join("cache");
        tokio::fs::create_dir_all(&cache_dir).await?;
        let target = cache_dir.join(format!("{asset_id}.{extension}"));
        if tokio::fs::try_exists(&target).await? {
            return Ok(target);
        }
        let temporary = target.with_extension("part");
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .header(reqwest::header::RANGE, "bytes=0-")
            .send()
            .await?
            .error_for_status()?;
        tokio::fs::write(&temporary, response.bytes().await?).await?;
        tokio::fs::rename(temporary, &target).await?;
        Ok(target)
    }

    fn seek_capture_cursors(&self, frame: u64) {
        for session in self.captures.values() {
            session.cursor.store(frame, Ordering::Release);
        }
    }

    fn pause_captures(&self, paused: bool) {
        for session in self.captures.values() {
            session.paused.store(paused, Ordering::Release);
        }
    }

    fn encode_flac(raw_path: &std::path::Path, flac_path: &std::path::Path) -> Result<()> {
        let description = format!(
            "filesrc location=\"{}\" ! rawaudioparse format=s32le sample-rate=48000 num-channels=2 interleaved=true ! audioconvert ! flacenc ! filesink location=\"{}\"",
            raw_path.display(),
            flac_path.display()
        );
        let pipeline = gst::parse::launch(&description)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("FLAC encoder did not create a pipeline"))?;
        pipeline.set_state(gst::State::Playing)?;
        let bus = pipeline
            .bus()
            .ok_or_else(|| anyhow!("FLAC encoder has no bus"))?;
        for message in bus.iter_timed(gst::ClockTime::NONE) {
            match message.view() {
                gst::MessageView::Eos(..) => break,
                gst::MessageView::Error(error) => {
                    pipeline.set_state(gst::State::Null)?;
                    return Err(anyhow!(error.error().to_string()));
                }
                _ => {}
            }
        }
        pipeline.set_state(gst::State::Null)?;
        Ok(())
    }
}

#[async_trait]
impl AudioDevice for Flow8Device {
    async fn probe(&mut self, config: &Config, calibrated: bool) -> ProbeReport {
        let playback = self.alsa_health(Direction::Playback, &config.alsa_playback_device);
        let capture = self.alsa_health(Direction::Capture, &config.alsa_capture_device);
        let midi = self.midi_health();
        let recording = if !calibrated {
            HealthItem::failed("calibration_missing")
        } else if capture.ok {
            HealthItem::ok()
        } else {
            HealthItem::failed("capture_unavailable")
        };
        ProbeReport {
            playback,
            capture,
            midi,
            recording,
            sample_rate: SAMPLE_RATE,
            playback_channels: 2,
            capture_channels: DEVICE_CAPTURE_CHANNELS,
            snapshot_slot: config.snapshot_slot,
        }
    }

    async fn calibrate(&mut self, config: &Config) -> Result<i64> {
        if let Ok(value) = std::env::var("KMA_CALIBRATION_OFFSET_FRAMES") {
            return value.parse().context("parse KMA_CALIBRATION_OFFSET_FRAMES");
        }
        let samples = Arc::new(Mutex::new(Vec::<i32>::new()));
        let capture_samples = Arc::clone(&samples);
        let description = format!(
            "audiotestsrc is-live=true wave=ticks samplesperbuffer=256 ! audioconvert ! audio/x-raw,format=S32LE,rate=48000,channels=2 ! alsasink device=\"{}\" sync=true alsasrc device=\"{}\" ! audio/x-raw,format=S32LE,rate=48000,channels=10 ! appsink name=calibration sync=false",
            config.alsa_playback_device, config.alsa_capture_device
        );
        let pipeline = gst::parse::launch(&description)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("calibration did not create a pipeline"))?;
        let sink = pipeline
            .by_name("calibration")
            .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| anyhow!("calibration appsink missing"))?;
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let mut output = capture_samples.lock().map_err(|_| gst::FlowError::Error)?;
                    output.extend(
                        map.as_slice()
                            .chunks_exact(DEVICE_CAPTURE_FRAME_BYTES)
                            .map(|frame| i32::from_le_bytes(frame[0..4].try_into().unwrap())),
                    );
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline.set_state(gst::State::Playing)?;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        pipeline.set_state(gst::State::Null)?;
        let samples = samples
            .lock()
            .map_err(|_| anyhow!("calibration buffer poisoned"))?;
        let (peak_frame, peak) = samples
            .iter()
            .enumerate()
            .skip(4_800)
            .max_by_key(|(_, sample)| sample.unsigned_abs())
            .ok_or_else(|| anyhow!("no calibration samples captured"))?;
        anyhow::ensure!(
            peak.unsigned_abs() > i32::MAX as u32 / 20,
            "calibration pulse not detected"
        );
        let expected = ((peak_frame as i64 + 24_000) / 48_000) * 48_000;
        Ok(peak_frame as i64 - expected)
    }

    async fn apply_mixer(&mut self, state: &MixerState, snapshot_slot: u8) -> Result<()> {
        let (output, port) = self.find_midi_port()?;
        let mut connection = output
            .connect(&port, "kma-flow8")
            .map_err(|error| anyhow!(error.to_string()))?;
        connection.send(&snapshot_program(snapshot_slot))?;
        for message in mixer_messages(state) {
            connection.send(&message)?;
        }
        Ok(())
    }

    async fn playback(&mut self, command: &PlaybackCommand) -> Result<()> {
        match command.action {
            PlaybackAction::Play | PlaybackAction::Switch => {
                let path = self.cache_media(command).await?;
                if let Some(player) = self.player.take() {
                    player.set_state(gst::State::Null)?;
                }
                let player = gst::ElementFactory::make("playbin3")
                    .build()
                    .context("create playbin3")?;
                let sink = gst::ElementFactory::make("alsasink")
                    .property("device", &self.config.alsa_playback_device)
                    .build()?;
                let uri = url::Url::from_file_path(&path)
                    .map_err(|_| anyhow!("invalid cache path"))?
                    .to_string();
                player.set_property("uri", uri);
                player.set_property("audio-sink", sink);
                player.set_state(gst::State::Paused)?;
                if let Some(position_ms) = command.position_ms {
                    player.seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                        gst::ClockTime::from_mseconds(position_ms),
                    )?;
                    self.seek_capture_cursors(position_ms * SAMPLE_RATE as u64 / 1_000);
                } else {
                    self.seek_capture_cursors(0);
                }
                player.set_state(gst::State::Playing)?;
                self.pause_captures(false);
                self.active_command_id = Some(command.command_id.clone());
                self.player = Some(player);
            }
            PlaybackAction::Pause => {
                if let Some(player) = &self.player {
                    player.set_state(gst::State::Paused)?;
                }
                self.pause_captures(true);
            }
            PlaybackAction::Resume => {
                if let Some(player) = &self.player {
                    player.set_state(gst::State::Playing)?;
                }
                self.pause_captures(false);
            }
            PlaybackAction::Seek => {
                let position_ms = command.position_ms.unwrap_or(0);
                if let Some(player) = &self.player {
                    player.seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                        gst::ClockTime::from_mseconds(position_ms),
                    )?;
                }
                self.seek_capture_cursors(position_ms * SAMPLE_RATE as u64 / 1_000);
            }
            PlaybackAction::Stop | PlaybackAction::Next => {
                if let Some(player) = self.player.take() {
                    player.set_state(gst::State::Null)?;
                }
                self.pause_captures(true);
            }
        }
        Ok(())
    }

    async fn start_recording(&mut self, request: RecordingStart) -> Result<()> {
        let raw_path = request
            .spool_dir
            .join(format!("{}.s32le", request.recording_session_id));
        let flac_path = request
            .spool_dir
            .join(format!("{}.flac", request.recording_session_id));
        let manifest_path = request
            .spool_dir
            .join(format!("{}.json", request.recording_session_id));
        let timeline = Arc::new(Mutex::new(SparseTimeline::create(&raw_path)?));
        let callback_timeline = Arc::clone(&timeline);
        let cursor = Arc::new(AtomicU64::new(0));
        let callback_cursor = Arc::clone(&cursor);
        let paused = Arc::new(AtomicBool::new(false));
        let callback_paused = Arc::clone(&paused);
        let offset = request.playback_to_capture_offset_frames;
        let description = format!(
            "alsasrc device=\"{}\" provide-clock=true ! audio/x-raw,format=S32LE,rate=48000,channels=10,layout=interleaved ! appsink name=capture sync=false max-buffers=16 drop=false",
            self.config.alsa_capture_device
        );
        let pipeline = gst::parse::launch(&description)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("capture did not create a pipeline"))?;
        let sink = pipeline
            .by_name("capture")
            .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| anyhow!("capture appsink missing"))?;
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    if callback_paused.load(Ordering::Acquire) {
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    let frames = (map.size() / DEVICE_CAPTURE_FRAME_BYTES) as u64;
                    let capture_frame = callback_cursor.fetch_add(frames, Ordering::AcqRel);
                    let destination = capture_to_song_frame(capture_frame, offset);
                    let samples = select_mic_channels(map.as_slice());
                    callback_timeline
                        .lock()
                        .map_err(|_| gst::FlowError::Error)?
                        .write_interleaved_at(destination, &samples)
                        .map_err(|_| gst::FlowError::Error)?;
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        let manifest = SpoolManifest {
            recording_session_id: request.recording_session_id.clone(),
            queue_item_id: request.queue_item_id,
            raw_path,
            flac_path,
            state: RecordingState::Recording,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            max_frame: 0,
            playback_to_capture_offset_frames: offset,
            interrupted: false,
        };
        manifest.store(&manifest_path)?;
        pipeline.set_state(gst::State::Playing)?;
        self.captures.insert(
            request.recording_session_id,
            CaptureSession {
                pipeline,
                timeline,
                cursor,
                paused,
                manifest_path,
                manifest,
            },
        );
        Ok(())
    }

    async fn cancel_recording(&mut self, recording_session_id: &str) -> Result<()> {
        if let Some(session) = self.captures.remove(recording_session_id) {
            session.pipeline.set_state(gst::State::Null)?;
            let _ = fs::remove_file(&session.manifest.raw_path);
            let _ = fs::remove_file(&session.manifest.flac_path);
            let _ = fs::remove_file(&session.manifest_path);
        }
        Ok(())
    }

    async fn finish_recording(
        &mut self,
        recording_session_id: &str,
        interrupted: bool,
    ) -> Result<Option<RecordingArtifact>> {
        let Some(mut session) = self.captures.remove(recording_session_id) else {
            return Ok(None);
        };
        session.pipeline.set_state(gst::State::Null)?;
        let max_frame = {
            let mut timeline = session
                .timeline
                .lock()
                .map_err(|_| anyhow!("timeline poisoned"))?;
            timeline.sync()?;
            timeline.max_frame()
        };
        session.manifest.max_frame = max_frame;
        session.manifest.state = if interrupted {
            RecordingState::Interrupted
        } else {
            RecordingState::Encoding
        };
        session.manifest.interrupted = interrupted;
        session.manifest.store(&session.manifest_path)?;
        Self::encode_flac(&session.manifest.raw_path, &session.manifest.flac_path)?;
        session.manifest.state = RecordingState::Uploading;
        session.manifest.store(&session.manifest_path)?;
        Ok(Some(RecordingArtifact {
            recording_session_id: recording_session_id.to_owned(),
            raw_path: session.manifest.raw_path,
            flac_path: session.manifest.flac_path,
            manifest_path: session.manifest_path,
            max_frame,
            interrupted,
        }))
    }

    async fn recover_recording(
        &mut self,
        manifest: &SpoolManifest,
    ) -> Result<Option<RecordingArtifact>> {
        if !manifest.flac_path.exists() {
            Self::encode_flac(&manifest.raw_path, &manifest.flac_path)?;
        }
        Ok(Some(RecordingArtifact {
            recording_session_id: manifest.recording_session_id.clone(),
            raw_path: manifest.raw_path.clone(),
            flac_path: manifest.flac_path.clone(),
            manifest_path: manifest.raw_path.with_extension("json"),
            max_frame: manifest.max_frame.max(
                fs::metadata(&manifest.raw_path)
                    .map(|metadata| metadata.len() / 8)
                    .unwrap_or(0),
            ),
            interrupted: manifest.interrupted,
        }))
    }

    fn anchor(&self) -> Option<(u64, u32, u64)> {
        let player = self.player.as_ref()?;
        let position = player.query_position::<gst::ClockTime>()?;
        let frame = position.nseconds() * SAMPLE_RATE as u64 / 1_000_000_000;
        Some((
            frame,
            SAMPLE_RATE,
            self.started_at.elapsed().as_millis() as u64,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_mic_one_and_two_from_ten_channel_frames() {
        let mut bytes = Vec::new();
        let frames: [[i32; 10]; 2] = [
            [11, 12, 13, 14, 15, 16, 17, 18, 19, 20],
            [21, 22, 23, 24, 25, 26, 27, 28, 29, 30],
        ];
        for frame in frames {
            for sample in frame {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
        }

        assert_eq!(select_mic_channels(&bytes), vec![11, 12, 21, 22]);
    }
}
