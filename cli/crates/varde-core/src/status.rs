//! Status is derived from a run's events and never stored, so it cannot
//! drift from them.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    /// The turn ended with sub-agents still running in the background.
    Delegated,
    Waiting,
    Blocked,
    Exited,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Delegated => "delegated",
            Status::Waiting => "waiting",
            Status::Blocked => "blocked",
            Status::Exited => "exited",
        }
    }

    pub fn needs_you(self) -> bool {
        matches!(self, Status::Waiting | Status::Blocked)
    }

    /// The session is in a turn: closing its task stops it mid-work, or
    /// while it waits for an answer.
    pub fn mid_turn(self) -> bool {
        matches!(self, Status::Working | Status::Delegated | Status::Blocked)
    }
}

/// Event kinds, by their stored names, that bear on status.
pub const STATUS_KINDS: &[&str] = &[
    "started",
    "prompt",
    "unblocked",
    "turn_end",
    "delegated",
    "interrupted",
    "answered",
    "blocked",
    "ended",
    "idle",
];

/// Apply one stored event kind to a status.
///
/// A session that starts sits at its prompt until it is given work, and
/// a turn you interrupt ends with no `turn_end`, so both are `waiting`,
/// as is one whose dialog was answered while no turn was running.
/// `idle` only moves `working` to `waiting`: a session blocked on a
/// dialog is idle too but already says why, and one that handed work to
/// sub-agents sits idle until they report back.
pub fn step(status: Option<Status>, kind: &str) -> Option<Status> {
    match kind {
        "prompt" | "unblocked" => Some(Status::Working),
        "started" | "turn_end" | "interrupted" | "answered" => Some(Status::Waiting),
        "delegated" => Some(Status::Delegated),
        "blocked" => Some(Status::Blocked),
        "ended" => Some(Status::Exited),
        "idle" if status == Some(Status::Working) => Some(Status::Waiting),
        _ => status,
    }
}

/// Fold a run's events, oldest first. `None` means no status-bearing
/// event yet.
pub fn fold<'a>(kinds: impl IntoIterator<Item = &'a str>) -> Option<Status> {
    kinds.into_iter().fold(None, step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The rule the store's status query relies on: only events from the
    /// last non-`idle` one on matter, because every other kind sets the
    /// status outright.
    fn tail_needed(kinds: &[&str]) -> usize {
        match kinds.iter().rposition(|k| *k != "idle") {
            Some(i) => kinds.len() - i,
            None => kinds.len(),
        }
    }

    #[test]
    fn a_turn_goes_working_then_waiting() {
        assert_eq!(fold(["started", "prompt"]), Some(Status::Working));
        assert_eq!(fold(["started", "prompt", "turn_end"]), Some(Status::Waiting));
    }

    #[test]
    fn answering_a_dialog_leaves_needs_you() {
        let s = fold(["prompt", "blocked"]);
        assert!(s.unwrap().needs_you());
        assert_eq!(fold(["prompt", "blocked", "unblocked"]), Some(Status::Working));
    }

    #[test]
    fn idle_only_moves_working() {
        assert_eq!(fold(["prompt", "idle"]), Some(Status::Waiting));
        assert_eq!(fold(["prompt", "blocked", "idle"]), Some(Status::Blocked));
        assert_eq!(fold(["ended", "idle"]), Some(Status::Exited));
    }

    #[test]
    fn a_session_waits_at_its_prompt_until_given_work() {
        assert_eq!(fold(["started"]), Some(Status::Waiting));
        assert_eq!(fold(["prompt", "ended", "started"]), Some(Status::Waiting));
        assert_eq!(fold(["prompt", "ended", "started", "prompt"]), Some(Status::Working));
    }

    #[test]
    fn a_dialog_answered_between_turns_leaves_the_session_waiting() {
        assert_eq!(fold(["turn_end", "blocked", "answered"]), Some(Status::Waiting));
    }

    #[test]
    fn an_interrupted_turn_waits() {
        assert_eq!(fold(["prompt", "interrupted"]), Some(Status::Waiting));
        assert_eq!(fold(["prompt", "blocked", "interrupted"]), Some(Status::Waiting));
    }

    #[test]
    fn a_turn_that_leaves_sub_agents_running_does_not_need_you() {
        let s = fold(["prompt", "delegated"]);
        assert_eq!(s, Some(Status::Delegated));
        assert!(!s.unwrap().needs_you());
        assert_eq!(fold(["prompt", "delegated", "idle"]), Some(Status::Delegated));
        // Each sub-agent reports back as a prompt of its own.
        assert_eq!(fold(["prompt", "delegated", "prompt"]), Some(Status::Working));
        assert_eq!(fold(["prompt", "delegated", "prompt", "turn_end"]), Some(Status::Waiting));
        assert_eq!(fold(["prompt", "delegated", "interrupted"]), Some(Status::Waiting));
    }

    #[test]
    fn unrelated_kinds_change_nothing() {
        assert_eq!(fold(["prompt", "turn_end", "compacting"]), Some(Status::Waiting));
        assert_eq!(fold(["compacting"]), None);
    }

    fn kind() -> impl Strategy<Value = &'static str> {
        prop::sample::select([STATUS_KINDS, &["compacting"]].concat())
    }

    proptest! {
        /// The store only reads the tail of a run's events; the tail must
        /// give the same status as the whole history.
        #[test]
        fn the_tail_agrees_with_the_whole(kinds in prop::collection::vec(kind(), 0..60)) {
            let status: Vec<&str> =
                kinds.iter().copied().filter(|k| STATUS_KINDS.contains(k)).collect();
            let n = tail_needed(&status);
            prop_assert_eq!(fold(status[status.len() - n..].iter().copied()),
                            fold(kinds.iter().copied()));
        }
    }
}
