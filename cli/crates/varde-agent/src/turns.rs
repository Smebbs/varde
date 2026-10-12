//! The digest every adapter gives a cairn update: each prompt with the
//! report that answered it, within a budget. Adapters differ only in how
//! a transcript line reads.

use std::io::Read;
use std::path::Path;

/// Transcript lines longer than this are mostly tool output, which is not
/// conversation and not worth parsing. One is still read unless it is a
/// tool result, so a long pasted prompt or report keeps its turn.
const LONG_LINE: usize = 20_000;
/// How the agents' transcripts mark a tool result.
const TOOL_RESULT: &[&str] = &["\"tool_result\"", "\"toolResult\""];
/// A pasted prompt is kept whole up to here.
const PROMPT_CHARS: usize = 2_000;
/// What a turn's report gets at the least before older turns are left out.
const MIN_REPORT: usize = 200;

/// Split `budget` characters across reports of these lengths: none gets
/// more than it needs, and what a short one leaves goes to the longer
/// ones. Reports lead with their conclusion, so a clip loses detail.
fn shares(lens: &[usize], budget: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lens.len()).collect();
    order.sort_by_key(|&i| lens[i]);
    let mut out = vec![0; lens.len()];
    let mut left = budget;
    for (k, &i) in order.iter().enumerate() {
        let fair = left / (lens.len() - k);
        out[i] = lens[i].min(fair);
        left -= out[i];
    }
    out
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).chain(['…']).collect()
    }
}

/// What happened in `transcript` after byte `from`, as turns of what was
/// asked and what was reported back, within about `budget` characters.
/// `line_text` gives (role, text) for a line that is conversation, with
/// role `user` or `assistant`.
pub fn digest(
    transcript: &Path,
    from: u64,
    budget: usize,
    line_text: impl Fn(&str) -> Option<(String, String)>,
) -> std::io::Result<(String, u64)> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(transcript)?;
    f.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    // Only whole lines: the agent may be writing the last one now.
    let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let text = String::from_utf8_lossy(&bytes[..whole]);

    // Each prompt with the last assistant message before the next
    // one: that turn's report. Earlier messages in a turn are
    // narration between tool calls.
    let mut turns: Vec<(String, Option<String>)> = Vec::new();
    let tool_output = |l: &str| l.len() >= LONG_LINE && TOOL_RESULT.iter().any(|m| l.contains(m));
    for line in text.lines().filter(|l| !tool_output(l)) {
        match line_text(line) {
            Some((role, t)) if role == "user" => turns.push((clip(&t, PROMPT_CHARS), None)),
            Some((_, t)) => match turns.last_mut() {
                Some(turn) => turn.1 = Some(t),
                // The turn began before `from`.
                None => turns.push(("(continued from before)".into(), Some(t))),
            },
            None => {}
        }
    }
    let end = from + whole as u64;
    if turns.is_empty() {
        return Ok((String::new(), end));
    }
    // The budget bounds the whole digest: prompts count too, and when
    // there are too many turns the oldest are left out, not clipped to
    // nothing.
    let mut asked_total = 0;
    let mut keep = 0;
    for (asked, _) in turns.iter().rev() {
        let cost = asked.chars().count() + MIN_REPORT;
        if keep > 0 && asked_total + cost > budget {
            break;
        }
        asked_total += cost;
        keep += 1;
    }
    let omitted = turns.len() - keep;
    let turns = &turns[omitted..];
    let budget = budget.saturating_sub(asked_total - keep * MIN_REPORT);
    let lens: Vec<usize> = turns.iter().map(|(_, r)| r.as_deref().map_or(0, |r| r.chars().count())).collect();
    let shares = shares(&lens, budget);
    let n = turns.len();
    let mut out = String::new();
    if omitted > 0 {
        out.push_str(&format!("({omitted} earlier turns left out)\n\n"));
    }
    for (i, (asked, reported)) in turns.iter().enumerate() {
        out.push_str(&format!("--- Turn {} of {n} ---\nAsked:\n{asked}\n\nReported back:\n", i + 1));
        match reported {
            Some(r) => out.push_str(&clip(r, shares[i])),
            None => out.push_str("(nothing yet: interrupted or still running)"),
        }
        out.push_str("\n\n");
    }
    Ok((out, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_reports_leave_their_share_to_long_ones() {
        assert_eq!(shares(&[10, 1000, 20], 300), [10, 270, 20]);
        assert_eq!(shares(&[500, 500], 300), [150, 150]);
        assert_eq!(shares(&[], 300), Vec::<usize>::new());
    }
}
