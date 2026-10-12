//! The tool calls in a transcript, for `varde eval`: what a session read,
//! searched, ran and edited, and when. Adapters differ only in how a line
//! reads.

use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

/// One tool call a session made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// Unix milliseconds, when the transcript says.
    pub ts: Option<i64>,
    pub name: String,
    /// Its input as JSON text: paths, patterns and commands are in it.
    pub input: String,
    /// Whether it changed a file.
    pub edit: bool,
}

/// Every tool call in `transcript`, in order, as `calls` reads each line
/// that may hold one (it holds `marker`).
pub fn read(
    transcript: &Path,
    marker: &str,
    calls: &dyn Fn(&Value) -> Vec<ToolCall>,
) -> std::io::Result<Vec<ToolCall>> {
    let mut out = Vec::new();
    for line in BufReader::new(std::fs::File::open(transcript)?).lines() {
        let line = line?;
        if !line.contains(marker) {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            out.extend(calls(&v));
        }
    }
    Ok(out)
}

/// A transcript line's time: an RFC 3339 string in UTC
/// (`2026-09-25T09:52:10.229Z`), or unix milliseconds.
pub fn when(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => iso_ms(s),
        _ => None,
    }
}

/// `2026-09-25T09:52:10.229Z` as unix milliseconds.
pub fn iso_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || !s.ends_with('Z') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let ms = match s.get(19..s.len() - 1) {
        Some(f) if f.starts_with('.') => {
            let digits: String = f[1..].chars().chain("000".chars()).take(3).collect();
            digits.parse::<i64>().ok()?
        }
        _ => 0,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_read_as_unix_milliseconds() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_ms("2026-09-25T09:52:10.229Z"), Some(1_790_329_930_229));
        assert_eq!(iso_ms("2024-02-29T12:00:00.5Z"), Some(1_709_208_000_500));
        assert_eq!(iso_ms("yesterday"), None);
        assert_eq!(when(&Value::from(5)), Some(5));
    }
}
