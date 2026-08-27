use crate::protocol::{MixerState, ReverbPreset};

const MIC1_CHANNEL: u8 = 0;
const MIC2_CHANNEL: u8 = 1;
const MUSIC_CHANNEL: u8 = 6;
const MAIN_BUS_CHANNEL: u8 = 7;
const MON1_BUS_CHANNEL: u8 = 8;
const MON2_BUS_CHANNEL: u8 = 9;
const FX1_BUS_CHANNEL: u8 = 10;
const FX1_CONTROL_CHANNEL: u8 = 13;
const GLOBAL_CHANNEL: u8 = 15;

pub fn db_to_cc(db: f32) -> u8 {
    (1.0 + ((db.clamp(-70.0, 10.0) + 70.0) / 80.0) * 126.0).round() as u8
}

pub fn percent_to_cc(percent: f32) -> u8 {
    (percent.clamp(0.0, 100.0) * 1.27).round() as u8
}

pub fn snapshot_program(slot: u8) -> [u8; 2] {
    [0xC0 | GLOBAL_CHANNEL, slot.clamp(1, 15)]
}

pub fn mixer_messages(state: &MixerState) -> Vec<Vec<u8>> {
    let preset = match state.reverb.preset {
        ReverbPreset::Default => 1,
        ReverbPreset::MaleVocal => 2,
        ReverbPreset::FemaleVocal => 3,
        ReverbPreset::Chorus => 14,
    };
    let mut messages = vec![
        cc(MIC1_CHANNEL, 7, db_to_cc(state.mic1.level_db)),
        cc(MIC1_CHANNEL, 5, bool_cc(state.mic1.muted)),
        cc(
            MIC1_CHANNEL,
            11,
            percent_to_cc(state.mic1.compressor_percent),
        ),
        cc(
            MIC1_CHANNEL,
            16,
            percent_to_cc(state.mic1.reverb_send_percent),
        ),
        cc(MIC2_CHANNEL, 7, db_to_cc(state.mic2.level_db)),
        cc(MIC2_CHANNEL, 5, bool_cc(state.mic2.muted)),
        cc(
            MIC2_CHANNEL,
            11,
            percent_to_cc(state.mic2.compressor_percent),
        ),
        cc(
            MIC2_CHANNEL,
            16,
            percent_to_cc(state.mic2.reverb_send_percent),
        ),
        cc(MUSIC_CHANNEL, 7, db_to_cc(state.music.level_db)),
        cc(MUSIC_CHANNEL, 5, bool_cc(state.music.muted)),
        cc(
            MAIN_BUS_CHANNEL,
            7,
            bus_level_cc(state.main.level_db, state.main.muted),
        ),
        program_change(FX1_CONTROL_CHANNEL, preset),
        cc(
            FX1_CONTROL_CHANNEL,
            1,
            percent_to_cc(state.reverb.decay_percent),
        ),
        cc(FX1_BUS_CHANNEL, 7, db_to_cc(state.reverb.return_level_db)),
    ];
    if let Some(mon1) = &state.mon1 {
        messages.push(cc(
            MON1_BUS_CHANNEL,
            7,
            bus_level_cc(mon1.level_db, mon1.muted),
        ));
    }
    if let Some(mon2) = &state.mon2 {
        messages.push(cc(
            MON2_BUS_CHANNEL,
            7,
            bus_level_cc(mon2.level_db, mon2.muted),
        ));
    }
    messages
}

fn cc(channel: u8, controller: u8, value: u8) -> Vec<u8> {
    vec![0xB0 | channel, controller, value]
}

fn program_change(channel: u8, program: u8) -> Vec<u8> {
    vec![0xC0 | channel, program]
}

fn bus_level_cc(level_db: f32, muted: bool) -> u8 {
    if muted { 0 } else { db_to_cc(level_db) }
}

fn bool_cc(value: bool) -> u8 {
    if value { 127 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_domain_values() {
        assert_eq!(db_to_cc(-100.0), 1);
        assert_eq!(db_to_cc(-70.0), 1);
        assert_eq!(db_to_cc(100.0), 127);
        assert_eq!(percent_to_cc(-1.0), 0);
        assert_eq!(percent_to_cc(101.0), 127);
        assert_eq!(snapshot_program(15), [0xCF, 15]);
    }

    #[test]
    fn maps_outputs_and_fx_to_flow8_channels() {
        let state: MixerState = serde_json::from_value(serde_json::json!({
            "mic1": { "levelDb": -18, "muted": false, "compressorPercent": 35, "reverbSendPercent": 25 },
            "mic2": { "levelDb": -18, "muted": false, "compressorPercent": 35, "reverbSendPercent": 25 },
            "music": { "levelDb": -6, "muted": false },
            "main": { "levelDb": -3, "muted": false },
            "mon1": { "levelDb": -12, "muted": false },
            "mon2": { "levelDb": -20, "muted": true },
            "reverb": { "preset": "chorus", "decayPercent": 50, "returnLevelDb": -12 }
        }))
        .unwrap();

        let messages = mixer_messages(&state);
        assert!(messages.contains(&vec![0xB7, 7, db_to_cc(-3.0)]));
        assert!(messages.contains(&vec![0xB8, 7, db_to_cc(-12.0)]));
        assert!(messages.contains(&vec![0xB9, 7, 0]));
        assert!(messages.contains(&vec![0xCD, 14]));
        assert!(messages.contains(&vec![0xBA, 7, db_to_cc(-12.0)]));
    }

    #[test]
    fn old_server_state_leaves_monitor_buses_untouched() {
        let state: MixerState = serde_json::from_value(serde_json::json!({
            "mic1": { "levelDb": -18, "muted": false, "compressorPercent": 35, "reverbSendPercent": 25 },
            "mic2": { "levelDb": -18, "muted": false, "compressorPercent": 35, "reverbSendPercent": 25 },
            "music": { "levelDb": -6, "muted": false },
            "main": { "levelDb": -3, "muted": false },
            "reverb": { "preset": "default", "decayPercent": 50, "returnLevelDb": -12 }
        }))
        .unwrap();

        assert_eq!(state.mon1, None);
        assert_eq!(state.mon2, None);
        let messages = mixer_messages(&state);
        assert!(!messages.iter().any(|message| message[0] == 0xB8));
        assert!(!messages.iter().any(|message| message[0] == 0xB9));
    }
}
