use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
const BYTES_PER_FRAME: u64 = CHANNELS as u64 * size_of::<i32>() as u64;

pub fn capture_to_song_frame(capture_frame: u64, offset_frames: i64) -> u64 {
    if offset_frames >= 0 {
        capture_frame.saturating_sub(offset_frames as u64)
    } else {
        capture_frame.saturating_add(offset_frames.unsigned_abs())
    }
}

pub struct SparseTimeline {
    file: File,
    path: PathBuf,
    max_frame: u64,
}

impl SparseTimeline {
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        let max_frame = file.metadata()?.len() / BYTES_PER_FRAME;
        Ok(Self {
            file,
            path,
            max_frame,
        })
    }

    pub fn write_interleaved_at(&mut self, song_frame: u64, samples: &[i32]) -> Result<u64> {
        anyhow::ensure!(
            samples.len().is_multiple_of(CHANNELS as usize),
            "capture buffer must contain interleaved stereo samples"
        );
        self.file
            .seek(SeekFrom::Start(song_frame * BYTES_PER_FRAME))?;
        for sample in samples {
            self.file.write_all(&sample.to_le_bytes())?;
        }
        let frames = samples.len() as u64 / CHANNELS as u64;
        self.max_frame = self.max_frame.max(song_frame + frames);
        Ok(self.max_frame)
    }

    pub fn sync(&mut self) -> Result<()> {
        self.file.set_len(self.max_frame * BYTES_PER_FRAME)?;
        self.file.sync_data()?;
        Ok(())
    }

    pub fn max_frame(&self) -> u64 {
        self.max_frame
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecordingState {
    Starting,
    Recording,
    Paused,
    Encoding,
    Uploading,
    Interrupted,
    Discarded,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingEvent {
    Started,
    Pause,
    Resume,
    NaturalEnd,
    Next,
    Stop,
    Disable,
    DisconnectTimeout,
    Encoded,
    Uploaded,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingEffect {
    None,
    Save,
    Discard,
    PreserveInterrupted,
}

pub fn transition(
    state: RecordingState,
    event: RecordingEvent,
) -> (RecordingState, RecordingEffect) {
    use RecordingEffect::*;
    use RecordingEvent::*;
    use RecordingState::*;
    match (state, event) {
        (Starting, Started) => (Recording, None),
        (Recording, Pause) => (Paused, None),
        (Paused, Resume) => (Recording, None),
        (Recording | Paused, NaturalEnd | Next) => (Encoding, Save),
        (Starting | Recording | Paused, Stop | Disable) => (Discarded, Discard),
        (Starting | Recording | Paused, DisconnectTimeout) => (Interrupted, PreserveInterrupted),
        (Encoding | Interrupted, Encoded) => (Uploading, Save),
        (Uploading, Uploaded) => (Completed, None),
        (_, Fail) => (Failed, None),
        _ => (state, None),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpoolManifest {
    pub recording_session_id: String,
    pub queue_item_id: String,
    pub raw_path: PathBuf,
    pub flac_path: PathBuf,
    pub state: RecordingState,
    pub sample_rate: u32,
    pub channels: u16,
    pub max_frame: u64,
    pub playback_to_capture_offset_frames: i64,
    #[serde(default)]
    pub interrupted: bool,
}

impl SpoolManifest {
    pub fn store(&self, path: &Path) -> Result<()> {
        let temporary = path.with_extension("json.part");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

pub fn recover_spool(spool_dir: &Path) -> Result<Vec<(PathBuf, SpoolManifest)>> {
    if !spool_dir.exists() {
        return Ok(Vec::new());
    }
    let mut recovered = Vec::new();
    for entry in fs::read_dir(spool_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let mut bytes = Vec::new();
        File::open(&path)?.read_to_end(&mut bytes)?;
        let mut manifest: SpoolManifest =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        if matches!(
            manifest.state,
            RecordingState::Starting | RecordingState::Recording | RecordingState::Paused
        ) {
            manifest.state = RecordingState::Interrupted;
            manifest.interrupted = true;
            manifest.store(&path)?;
        }
        if !matches!(
            manifest.state,
            RecordingState::Completed | RecordingState::Discarded
        ) {
            recovered.push((path, manifest));
        }
    }
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_seek_leaves_silence_and_backward_seek_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("take.s32le");
        let mut timeline = SparseTimeline::create(&path).unwrap();
        timeline.write_interleaved_at(0, &[1, 2, 3, 4]).unwrap();
        timeline.write_interleaved_at(4, &[9, 10]).unwrap();
        timeline.write_interleaved_at(1, &[7, 8]).unwrap();
        timeline.sync().unwrap();
        let bytes = fs::read(path).unwrap();
        let frames = bytes
            .as_chunks::<{ size_of::<i32>() }>()
            .0
            .iter()
            .map(|chunk| i32::from_le_bytes(*chunk))
            .collect::<Vec<_>>();
        assert_eq!(frames, vec![1, 2, 7, 8, 0, 0, 0, 0, 9, 10]);
        assert_eq!(timeline.max_frame(), 5);
    }

    #[test]
    fn signed_capture_offset_maps_to_song_frames() {
        assert_eq!(capture_to_song_frame(1_000, 256), 744);
        assert_eq!(capture_to_song_frame(1_000, -256), 1_256);
        assert_eq!(capture_to_song_frame(100, 256), 0);
    }

    #[test]
    fn stop_discards_but_next_and_end_save() {
        assert_eq!(
            transition(RecordingState::Recording, RecordingEvent::Stop),
            (RecordingState::Discarded, RecordingEffect::Discard)
        );
        assert_eq!(
            transition(RecordingState::Recording, RecordingEvent::Next),
            (RecordingState::Encoding, RecordingEffect::Save)
        );
        assert_eq!(
            transition(RecordingState::Paused, RecordingEvent::NaturalEnd),
            (RecordingState::Encoding, RecordingEffect::Save)
        );
        assert_eq!(
            transition(RecordingState::Recording, RecordingEvent::Disable),
            (RecordingState::Discarded, RecordingEffect::Discard)
        );
    }

    #[test]
    fn crash_recovery_marks_live_take_interrupted() {
        let directory = tempfile::tempdir().unwrap();
        let manifest_path = directory.path().join("take.json");
        SpoolManifest {
            recording_session_id: "take".to_owned(),
            queue_item_id: "queue".to_owned(),
            raw_path: directory.path().join("take.s32le"),
            flac_path: directory.path().join("take.flac"),
            state: RecordingState::Recording,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            max_frame: 42,
            playback_to_capture_offset_frames: 0,
            interrupted: false,
        }
        .store(&manifest_path)
        .unwrap();
        let recovered = recover_spool(directory.path()).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.state, RecordingState::Interrupted);
    }
}
