use std::path::{Path, PathBuf};

use anyhow::Result;
#[cfg(not(all(target_os = "linux", feature = "linux-audio")))]
use anyhow::anyhow;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    config::Config,
    protocol::{MixerState, PlaybackCommand},
    recording::SpoolManifest,
};

#[cfg(any(test, all(target_os = "linux", feature = "linux-audio")))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CalibrationPeak {
    pub frame: usize,
    pub channel: usize,
    pub sample: i32,
    pub channel_peaks: Vec<u32>,
}

#[cfg(any(test, all(target_os = "linux", feature = "linux-audio")))]
pub(crate) fn find_calibration_peak(
    interleaved_samples: &[i32],
    channels: usize,
    skip_frames: usize,
) -> Option<CalibrationPeak> {
    if channels == 0 {
        return None;
    }

    let mut channel_peaks = vec![0; channels];
    let mut strongest: Option<(usize, usize, i32)> = None;
    for (frame, samples) in interleaved_samples
        .chunks_exact(channels)
        .enumerate()
        .skip(skip_frames)
    {
        for (channel, sample) in samples.iter().copied().enumerate() {
            let amplitude = sample.unsigned_abs();
            channel_peaks[channel] = channel_peaks[channel].max(amplitude);
            if strongest
                .as_ref()
                .is_none_or(|(_, _, strongest_sample)| amplitude > strongest_sample.unsigned_abs())
            {
                strongest = Some((frame, channel, sample));
            }
        }
    }

    strongest.map(|(frame, channel, sample)| CalibrationPeak {
        frame,
        channel,
        sample,
        channel_peaks,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthItem {
    pub ok: bool,
    pub reason: Option<String>,
}

impl HealthItem {
    pub fn ok() -> Self {
        Self {
            ok: true,
            reason: None,
        }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    pub playback: HealthItem,
    pub capture: HealthItem,
    pub midi: HealthItem,
    pub recording: HealthItem,
    pub sample_rate: u32,
    pub playback_channels: u16,
    pub capture_channels: u16,
    pub snapshot_slot: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    pub playback_to_capture_offset_frames: i64,
    pub sample_rate: u32,
    pub measured_at: String,
}

impl Calibration {
    pub async fn load(path: &Path) -> Result<Option<Self>> {
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn store(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temporary = path.with_extension("json.part");
        tokio::fs::write(&temporary, serde_json::to_vec_pretty(self)?).await?;
        tokio::fs::rename(temporary, path).await?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RecordingStart {
    pub recording_session_id: String,
    pub queue_item_id: String,
    pub spool_dir: PathBuf,
    pub playback_to_capture_offset_frames: i64,
}

#[derive(Debug, Clone)]
pub struct RecordingArtifact {
    pub recording_session_id: String,
    pub raw_path: PathBuf,
    pub flac_path: PathBuf,
    pub manifest_path: PathBuf,
    pub max_frame: u64,
    pub interrupted: bool,
}

#[async_trait]
pub trait AudioDevice: Send {
    async fn probe(&mut self, config: &Config, calibrated: bool) -> ProbeReport;
    async fn calibrate(&mut self, config: &Config) -> Result<i64>;
    async fn apply_mixer(&mut self, state: &MixerState, snapshot_slot: u8) -> Result<()>;
    async fn playback(&mut self, command: &PlaybackCommand) -> Result<()>;
    async fn start_recording(&mut self, request: RecordingStart) -> Result<()>;
    async fn cancel_recording(&mut self, recording_session_id: &str) -> Result<()>;
    async fn finish_recording(
        &mut self,
        recording_session_id: &str,
        interrupted: bool,
    ) -> Result<Option<RecordingArtifact>>;
    async fn recover_recording(
        &mut self,
        manifest: &SpoolManifest,
    ) -> Result<Option<RecordingArtifact>>;
    fn anchor(&mut self) -> Option<(u64, u32, u64)>;

    fn playback_state(&self) -> &'static str {
        "playing"
    }
}

pub fn create_device(
    config: &Config,
    server_url: String,
    token: String,
) -> Result<Box<dyn AudioDevice>> {
    #[cfg(all(target_os = "linux", feature = "linux-audio"))]
    let device: Box<dyn AudioDevice> =
        Box::new(linux::Flow8Device::new(config.clone(), server_url, token)?);
    #[cfg(not(all(target_os = "linux", feature = "linux-audio")))]
    let device: Box<dyn AudioDevice> = {
        let _ = (config, server_url, token);
        Box::new(UnsupportedDevice)
    };
    Ok(device)
}

#[cfg(not(all(target_os = "linux", feature = "linux-audio")))]
struct UnsupportedDevice;

#[cfg(not(all(target_os = "linux", feature = "linux-audio")))]
#[async_trait]
impl AudioDevice for UnsupportedDevice {
    async fn probe(&mut self, config: &Config, calibrated: bool) -> ProbeReport {
        let reason = "binary was built without the linux-audio feature";
        ProbeReport {
            playback: HealthItem::failed(reason),
            capture: HealthItem::failed(reason),
            midi: HealthItem::failed(reason),
            recording: if calibrated {
                HealthItem::failed(reason)
            } else {
                HealthItem::failed("calibration_missing")
            },
            sample_rate: 48_000,
            playback_channels: 0,
            capture_channels: 0,
            snapshot_slot: config.snapshot_slot,
        }
    }

    async fn calibrate(&mut self, _config: &Config) -> Result<i64> {
        Err(anyhow!("calibration requires a linux-audio build"))
    }

    async fn apply_mixer(&mut self, _state: &MixerState, _snapshot_slot: u8) -> Result<()> {
        Err(anyhow!("MIDI is unavailable"))
    }

    async fn playback(&mut self, _command: &PlaybackCommand) -> Result<()> {
        Err(anyhow!("audio playback is unavailable"))
    }

    async fn start_recording(&mut self, _request: RecordingStart) -> Result<()> {
        Err(anyhow!("audio capture is unavailable"))
    }

    async fn cancel_recording(&mut self, _recording_session_id: &str) -> Result<()> {
        Ok(())
    }

    async fn finish_recording(
        &mut self,
        _recording_session_id: &str,
        _interrupted: bool,
    ) -> Result<Option<RecordingArtifact>> {
        Ok(None)
    }

    async fn recover_recording(
        &mut self,
        _manifest: &SpoolManifest,
    ) -> Result<Option<RecordingArtifact>> {
        Ok(None)
    }

    fn anchor(&mut self) -> Option<(u64, u32, u64)> {
        None
    }
}

#[cfg(all(target_os = "linux", feature = "linux-audio"))]
mod linux;

#[cfg(test)]
mod tests {
    use super::find_calibration_peak;

    #[test]
    fn calibration_peak_scans_every_capture_channel() {
        let mut samples = vec![0_i32; 8 * 10];
        samples[2 * 10] = i32::MAX;
        samples[6 * 10 + 8] = -1_500_000_000;

        let peak = find_calibration_peak(&samples, 10, 4).expect("peak after skipped frames");

        assert_eq!(peak.frame, 6);
        assert_eq!(peak.channel, 8);
        assert_eq!(peak.sample, -1_500_000_000);
        assert_eq!(peak.channel_peaks[0], 0);
        assert_eq!(peak.channel_peaks[8], 1_500_000_000);
    }

    #[test]
    fn calibration_peak_ignores_incomplete_capture_frame() {
        let mut samples = vec![0_i32; 10];
        samples[4] = 123;
        samples.extend([i32::MAX, i32::MAX]);

        let peak = find_calibration_peak(&samples, 10, 0).expect("complete capture frame");

        assert_eq!(peak.frame, 0);
        assert_eq!(peak.channel, 4);
        assert_eq!(peak.sample, 123);
    }

    #[test]
    fn calibration_peak_rejects_zero_channel_layout() {
        assert!(find_calibration_peak(&[1, 2, 3], 0, 0).is_none());
    }
}
