//! `varde list`: the board, as a table or as JSON for clients.

use std::time::Duration;

use anyhow::Result;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use varde_core::board::{self, Board};

use crate::{now_ms, Paths};

pub fn run(paths: &Paths, json: bool, all: bool) -> Result<()> {
    let store = paths.open(Duration::from_secs(2))?;
    let now = now_ms();
    let board = board::build(&store, now, local_midnight_ms(), all, &crate::process::alive)?;
    if !all {
        crate::sweep::kick(paths, &board, now)?;
    }
    if json {
        println!("{}", serde_json::to_string(&board)?);
    } else {
        print!("{}", table(&board, now, all, columns()));
    }
    Ok(())
}

pub fn local_midnight_ms() -> i64 {
    use chrono::{Local, TimeZone};
    let today = Local::now().date_naive().and_hms_opt(0, 0, 0).unwrap_or_default();
    Local.from_local_datetime(&today).earliest().map(|t| t.timestamp_millis()).unwrap_or(0)
}

/// The terminal's width, or 80 when the output goes elsewhere.
fn columns() -> usize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0;
    if ok && size.ws_col > 0 {
        size.ws_col as usize
    } else {
        80
    }
}

/// The columns of a row other than its title.
const FIXED: usize = 2 + 6 + 2 + 8 + 1 + 10 + 1 + 1 + PROJECT + 1 + 4;
const PROJECT: usize = 12;

/// The board as the TUI draws it, in `width` columns: `●` marks an
/// unread row, and `← k2f0g3` the task deferred work came from. A
/// collapsed group shows only its count, unless `all` asks for everything.
fn table(board: &Board, now: i64, all: bool, width: usize) -> String {
    let title_w = width.saturating_sub(FIXED).max(20);
    let mut out = String::new();
    for g in &board.groups {
        let hidden = g.collapsed && !all && !g.rows.is_empty();
        out.push_str(&format!(
            "{} ({}){}\n",
            g.label,
            g.rows.len(),
            if hidden { " ▸ --all lists them" } else { "" }
        ));
        for r in g.rows.iter().filter(|_| !hidden) {
            let project = project_name(r.project.as_deref());
            let origin = r.origin.as_ref().map(|o| format!("  ← {}", o.short)).unwrap_or_default();
            out.push_str(&format!(
                " {}{}  {:<8} {} {} {} {:>4}\n",
                if r.unread { "●" } else { " " },
                r.short,
                r.status,
                pad(r.git.as_ref().map_or("", |g| g.label.as_str()), 10),
                pad(
                    &(clip(&varde_core::title::clean(&r.title), title_w.saturating_sub(origin.width()))
                        + &origin),
                    title_w
                ),
                pad(project, PROJECT),
                age(now - r.last)
            ));
        }
    }
    out
}

/// `s` in at most `n` columns, cut with `…`. Columns, not characters: a
/// CJK character or an emoji takes two.
pub fn clip(s: &str, n: usize) -> String {
    if s.width() <= n {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > n {
            break;
        }
        out.push(c);
        w += cw;
    }
    if n > 0 {
        out.push('…');
    }
    out
}

/// `s` in exactly `n` columns: clipped, then padded with spaces.
pub fn pad(s: &str, n: usize) -> String {
    let mut out = clip(s, n);
    out.push_str(&" ".repeat(n.saturating_sub(out.width())));
    out
}

/// The last component of a project path, which is what people call it.
pub fn project_name(project: Option<&str>) -> &str {
    project.and_then(|p| p.rsplit('/').find(|s| !s.is_empty())).unwrap_or("-")
}

pub fn age(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_marks_unread_rows_and_origins_and_folds_parked() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            varde_core::store::Store::open(&dir.path().join("varde.db"), Duration::from_secs(1)).unwrap();
        let origin = varde_core::task::create(&store, "Big job", None, Some("/p"), None, 1).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO run (id, task, agent, native_id, relation, started) VALUES ('R1', ?1, 'claude', 'n', 'fresh', 1)",
                [&origin],
            )
            .unwrap();
        varde_core::task::defer(&store, "Later bit", None, "R1", None, 2).unwrap();
        let parked = varde_core::task::create(&store, "Someday", None, Some("/p"), None, 3).unwrap();
        store.conn.execute("UPDATE task SET state = 'parked' WHERE id = ?1", [&parked]).unwrap();
        let board = board::build(&store, 10, 0, false, &|_, _| true).unwrap();
        let t = table(&board, 10, false, 80);
        assert!(t.contains(&format!("Later bit  ← {}", varde_core::task::short(&origin))), "{t}");
        assert!(t.contains("Parked (1) ▸ --all lists them") && !t.contains("Someday"), "{t}");
        let mut board = board;
        board.groups[2].rows[0].unread = true;
        assert!(table(&board, 10, false, 80).contains(&format!(" ●{}", board.groups[2].rows[0].short)));
    }

    #[test]
    fn the_table_fits_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            varde_core::store::Store::open(&dir.path().join("varde.db"), Duration::from_secs(1)).unwrap();
        let long = "A rather long title for a task, longer than any column would hold, 日本語 too";
        varde_core::task::create(&store, long, None, Some("/src/a-project-with-a-long-name"), None, 1)
            .unwrap();
        let board = board::build(&store, 10, 0, false, &|_, _| true).unwrap();
        for width in [80, 120] {
            let t = table(&board, 10, false, width);
            assert!(t.lines().all(|l| l.width() <= width), "{width}:\n{t}");
            assert!(t.lines().any(|l| l.width() == width), "the title takes the room:\n{t}");
        }
    }

    #[test]
    fn a_title_is_printed_without_its_control_characters() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            varde_core::store::Store::open(&dir.path().join("varde.db"), Duration::from_secs(1)).unwrap();
        store
            .conn
            .execute("INSERT INTO task (id, title, created) VALUES ('T', ?1, 1)", ["\x1b[2JEvil\ttitle"])
            .unwrap();
        let board = board::build(&store, 10, 0, false, &|_, _| true).unwrap();
        let t = table(&board, 10, false, 80);
        assert!(!t.contains(|c: char| c.is_control() && c != '\n') && t.contains("[2JEvil title"), "{t:?}");
    }

    #[test]
    fn wide_characters_take_two_columns() {
        assert_eq!(pad("日本語のタイトル", 9), "日本語の…");
        assert_eq!(pad("日本語のタイトル", 10), "日本語の… ");
        assert_eq!(pad("🚀 go", 6), "🚀 go ");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(pad("ab", 4), "ab  ");
    }
}
