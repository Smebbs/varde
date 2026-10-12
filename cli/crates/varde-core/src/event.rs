//! The agent-neutral event vocabulary. Adapters translate their agent's
//! hooks into these; nothing past the adapter knows any agent's names.

/// How a session came to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A new conversation.
    Startup,
    /// The same conversation, continued under the same native id.
    Resume,
    /// A new native id whose history is a copy of another conversation.
    Fork,
    /// The conversation's history was cleared.
    Clear,
    /// The context was compacted; the session carries on.
    Compact,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Started(Source),
    /// The user gave the session work.
    Prompt,
    /// The turn finished. With no sub-agents left running it is the
    /// user's move; with some, the session wakes itself as each reports
    /// back. Background shells are counted but hold nothing up: a server
    /// left running never finishes.
    TurnEnd {
        agents: u32,
        shells: u32,
    },
    /// The session is waiting on a permission decision.
    Blocked,
    /// A tool ran. Only meaningful right after `Blocked`, as the proof
    /// that the decision was made, since agents send no event for that,
    /// and right after a turn end, as a turn that started with no prompt.
    ToolRan,
    /// A dialog was answered, for an agent that says so. During a turn
    /// the turn goes on; otherwise the session is back at its prompt.
    Answered,
    /// The agent has sat waiting for input for a while.
    Idle,
    /// The user stopped the turn, for an agent that says so; for one that
    /// does not, `varde run` records it from the transcript.
    Interrupted,
    Ended,
    /// The context is about to be compacted.
    Compacting,
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            // Compaction happens inside a session, often mid-turn: it
            // leaves the status as it was.
            Kind::Started(Source::Compact) => "compacted",
            Kind::Started(_) => "started",
            Kind::Prompt => "prompt",
            Kind::TurnEnd { agents: 0, .. } => "turn_end",
            Kind::TurnEnd { .. } => "delegated",
            Kind::Blocked => "blocked",
            Kind::ToolRan => "unblocked",
            Kind::Answered => "answered",
            Kind::Idle => "idle",
            Kind::Interrupted => "interrupted",
            Kind::Ended => "ended",
            Kind::Compacting => "compacting",
        }
    }
}

/// One normalised hook event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Norm {
    pub native_id: String,
    pub kind: Kind,
    pub cwd: Option<String>,
    pub transcript: Option<String>,
    pub prompt: Option<String>,
}
