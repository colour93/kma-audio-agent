use crate::protocol::{MixerState, ReverbPreset};

const MIDI_CHANNEL: u8 = 0;

pub fn db_to_cc(db: f32) -> u8 {
    (((db.clamp(-60.0, 10.0) + 60.0) / 70.0) * 127.0).round() as u8
}

pub fn percent_to_cc(percent: f32) -> u8 {
    (percent.clamp(0.0, 100.0) * 1.27).round() as u8
}

pub fn snapshot_program(slot: u8) -> [u8; 2] {
    [0xC0 | MIDI_CHANNEL, slot.clamp(1, 15) - 1]
}

pub fn mixer_messages(state: &MixerState) -> Vec<[u8; 3]> {
    let preset = match state.reverb.preset {
        ReverbPreset::Default => 0,
        ReverbPreset::MaleVocal => 1,
        ReverbPreset::FemaleVocal => 2,
        ReverbPreset::Chorus => 3,
    };
    vec![
        cc(20, db_to_cc(state.mic1.level_db)),
        cc(21, bool_cc(state.mic1.muted)),
        cc(22, percent_to_cc(state.mic1.compressor_percent)),
        cc(23, percent_to_cc(state.mic1.reverb_send_percent)),
        cc(24, db_to_cc(state.mic2.level_db)),
        cc(25, bool_cc(state.mic2.muted)),
        cc(26, percent_to_cc(state.mic2.compressor_percent)),
        cc(27, percent_to_cc(state.mic2.reverb_send_percent)),
        cc(28, db_to_cc(state.music.level_db)),
        cc(29, bool_cc(state.music.muted)),
        cc(30, db_to_cc(state.main.level_db)),
        cc(31, bool_cc(state.main.muted)),
        cc(32, preset),
        cc(33, percent_to_cc(state.reverb.decay_percent)),
        cc(34, db_to_cc(state.reverb.return_level_db)),
    ]
}

fn cc(controller: u8, value: u8) -> [u8; 3] {
    [0xB0 | MIDI_CHANNEL, controller, value]
}

fn bool_cc(value: bool) -> u8 {
    if value { 127 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_domain_values() {
        assert_eq!(db_to_cc(-100.0), 0);
        assert_eq!(db_to_cc(100.0), 127);
        assert_eq!(percent_to_cc(-1.0), 0);
        assert_eq!(percent_to_cc(101.0), 127);
        assert_eq!(snapshot_program(15), [0xC0, 14]);
    }
}
