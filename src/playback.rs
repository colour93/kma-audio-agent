use std::collections::{HashSet, VecDeque};

use crate::protocol::PlaybackCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyResult {
    Apply,
    Duplicate,
    StaleEpoch,
}

#[derive(Debug, Default)]
pub struct PlaybackCoordinator {
    route_epoch: u64,
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl PlaybackCoordinator {
    pub fn apply(&mut self, command: &PlaybackCommand) -> ApplyResult {
        if command.route_epoch < self.route_epoch {
            return ApplyResult::StaleEpoch;
        }
        if command.route_epoch > self.route_epoch {
            self.route_epoch = command.route_epoch;
            self.seen.clear();
            self.order.clear();
        }
        if self.seen.contains(&command.command_id) {
            return ApplyResult::Duplicate;
        }
        self.seen.insert(command.command_id.clone());
        self.order.push_back(command.command_id.clone());
        if self.order.len() > 256
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        ApplyResult::Apply
    }

    pub fn route_epoch(&self) -> u64 {
        self.route_epoch
    }
}

#[derive(Debug, Default)]
pub struct MixerRevision {
    applied: u64,
}

impl MixerRevision {
    pub fn apply(&mut self, revision: u64) -> ApplyResult {
        if revision < self.applied {
            ApplyResult::StaleEpoch
        } else if revision == self.applied && revision != 0 {
            ApplyResult::Duplicate
        } else {
            self.applied = revision;
            ApplyResult::Apply
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::protocol::{PlaybackAction, PlaybackCommand};

    use super::*;

    fn command(id: &str, epoch: u64) -> PlaybackCommand {
        PlaybackCommand {
            schema_version: 1,
            command_id: id.to_owned(),
            route_epoch: epoch,
            action: PlaybackAction::Play,
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
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn rejects_stale_epochs_and_deduplicates_commands() {
        let mut state = PlaybackCoordinator::default();
        assert_eq!(state.apply(&command("a", 4)), ApplyResult::Apply);
        assert_eq!(state.apply(&command("a", 4)), ApplyResult::Duplicate);
        assert_eq!(state.apply(&command("b", 3)), ApplyResult::StaleEpoch);
        assert_eq!(state.apply(&command("b", 5)), ApplyResult::Apply);
    }

    #[test]
    fn rejects_old_mixer_revisions() {
        let mut revision = MixerRevision::default();
        assert_eq!(revision.apply(3), ApplyResult::Apply);
        assert_eq!(revision.apply(3), ApplyResult::Duplicate);
        assert_eq!(revision.apply(2), ApplyResult::StaleEpoch);
    }
}
