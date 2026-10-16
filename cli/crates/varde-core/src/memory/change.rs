//! A person's change to one line of a cairn: set its Status or hand it
//! back to the updates, add a bullet, change or remove one, or bring back
//! one they removed. Each is
//! one transaction on the document as it is now: bullets keep their ids,
//! so no version needs to travel with it. The caps hold, the line written
//! is the user's, and what leaves goes to history, which recall does not
//! search: the user took it back.

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{document, write_doc, Owner, Write};
use crate::cairn::{normalized, Cairn, Item, Section, MAX_BULLET_WORDS, MAX_STATUS_WORDS};
use crate::store::Store;

/// Why a bullet the user removed left its cairn.
pub const REMOVED: &str = "removed by you";
/// Why the words of a bullet the user changed left it.
pub const EDITED_BY_YOU: &str = "edited by you";
/// Why a bullet left when the user took it out or rewrote it, in a line's
/// edit or the whole document's: what it said is taken back, so recall
/// does not find it. It stays in history, and `--restore` brings it back.
pub const TAKEN_BACK: [&str; 3] = [REMOVED, EDITED_BY_YOU, "removed in varde edit"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change<'a> {
    /// Replace the Status; a task's cairn only.
    Status(&'a str),
    /// Hand a Status the user set back to the updates: its words stay,
    /// and the next update may change them.
    HandBack,
    Add(Section, &'a str),
    Set(i64, &'a str),
    Remove(i64),
    /// Bring back a bullet removed by you, as it was.
    Restore(i64),
}

/// The document after a change, its version, and the bullet the change
/// was about (`None` for the Status). `changed` is false when there was
/// nothing to write, such as a bullet set to the words it has.
#[derive(Debug)]
pub struct Changed {
    pub cairn: Cairn,
    pub version: i64,
    pub id: Option<i64>,
    pub changed: bool,
}

/// One line as typed: its words, one space between them.
pub fn one_line(text: &str) -> String {
    crate::cairn::clean(text).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Make `change` to `owner`'s document, or refuse it, writing nothing,
/// with a message that says why.
pub fn change(store: &mut Store, owner: Owner, change: Change, now: i64) -> Result<Changed> {
    let tx = store.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let shape = owner.shape();
    let (before, version) =
        document(&tx, owner)?.unwrap_or_else(|| (Cairn { shape, ..Default::default() }, 0));
    let mut c = before.clone();
    let find = |c: &Cairn, id: i64| {
        shape.sections().iter().find_map(|s| c.section(*s).iter().position(|i| i.id == id).map(|p| (*s, p)))
    };
    // The words of a bullet, checked as an update's are.
    let words = |text: &str, c: &Cairn, id: i64| -> Result<String> {
        let text = one_line(text);
        let n = text.split_whitespace().count();
        if text.is_empty() {
            bail!("a line needs words; to take one out, remove it");
        }
        if n > MAX_BULLET_WORDS {
            bail!("a line is at most {MAX_BULLET_WORDS} words, and this has {n}");
        }
        let key = normalized(&text);
        if let Some((_, same)) = c.items().find(|(_, i)| i.id != id && normalized(&i.text) == key) {
            bail!("a line already says this (line #{})", same.id);
        }
        Ok(text)
    };
    let full = |c: &Cairn, s: Section| -> Result<()> {
        let n = c.section(s).len();
        if n >= s.max() {
            bail!("{} is full ({n} of {}): change or remove one of its lines first", s.heading(), s.max());
        }
        Ok(())
    };
    let mut gone = Vec::new();
    let (id, reason) = match change {
        Change::Status(_) if matches!(owner, Owner::Project(_)) => bail!("a project cairn has no Status"),
        Change::Status(text) => {
            let text = one_line(text);
            let n = text.split_whitespace().count();
            if text.is_empty() {
                bail!("a Status needs words");
            }
            if n > MAX_STATUS_WORDS {
                bail!("the Status is at most {MAX_STATUS_WORDS} words, and this has {n}");
            }
            c.status = text;
            c.status_mine = true;
            (None, format!("set status: {}", c.status))
        }
        Change::HandBack if matches!(owner, Owner::Project(_)) => bail!("a project cairn has no Status"),
        Change::HandBack if !c.status_mine => bail!("the Status is already the updates'"),
        Change::HandBack => {
            c.status_mine = false;
            (None, "hand back status".to_string())
        }
        Change::Add(s, _) if !shape.sections().contains(&s) => {
            let names: Vec<&str> = shape.sections().iter().map(|s| s.name()).collect();
            bail!("no section {} here: it has {}", s.name(), names.join(", "))
        }
        Change::Add(s, text) => {
            let text = words(text, &c, 0)?;
            full(&c, s)?;
            c.section_mut(s).push(Item { mine: true, confirmed: now, ..Item::new(&text) });
            (None, format!("add to {}: {text}", s.name()))
        }
        Change::Set(id, text) => {
            let Some((s, p)) = find(&c, id) else { return Err(missing(&tx, owner, id)?) };
            let text = words(text, &c, id)?;
            let item = &mut c.section_mut(s)[p];
            // The same words make the line yours, and change nothing else.
            if item.text == text {
                item.mine = true;
            } else {
                *item = Item { id, mine: true, confirmed: now, ..Item::new(&text) };
            }
            (Some(id), format!("edit {id}"))
        }
        Change::Remove(id) => {
            let Some((s, p)) = find(&c, id) else {
                let removed: bool = tx.query_row(
                    &format!(
                        "SELECT EXISTS (SELECT 1 FROM cairn_item
                         WHERE id = ?1 AND {} = ?2 AND state = 'history' AND reason = ?3)",
                        owner.column()
                    ),
                    params![id, owner.key(), REMOVED],
                    |r| r.get(0),
                )?;
                if removed {
                    bail!("line #{id} is already removed; --restore {id} brings it back")
                }
                return Err(missing(&tx, owner, id)?);
            };
            c.section_mut(s).remove(p);
            gone.push((id, REMOVED.to_string()));
            (Some(id), format!("remove {id}"))
        }
        Change::Restore(id) => {
            let row: Option<(String, usize, Item)> = tx
                .query_row(
                    &format!(
                        "SELECT section, pos, text, anchor, mine, confirmed FROM cairn_item
                         WHERE id = ?1 AND {} = ?2 AND state = 'history' AND reason = ?3",
                        owner.column()
                    ),
                    params![id, owner.key(), REMOVED],
                    |r| {
                        let item = Item {
                            id,
                            text: r.get(2)?,
                            anchor: r.get(3)?,
                            mine: r.get(4)?,
                            confirmed: r.get(5)?,
                            stale: false,
                        };
                        Ok((r.get(0)?, r.get(1)?, item))
                    },
                )
                .optional()?;
            let Some((s, pos, item)) = row.and_then(|(s, p, i)| Some((Section::parse(&s)?, p, i))) else {
                return Err(match replaced_by(&tx, owner, id)? {
                    Some(now) => anyhow!("line #{id} was changed since: it is #{now} now"),
                    None => anyhow!("no line #{id} removed by you to bring back"),
                });
            };
            words(&item.text, &c, id)?;
            full(&c, s)?;
            // Where it was, or as near as the section now allows.
            let items = c.section_mut(s);
            items.insert(pos.min(items.len()), item);
            (Some(id), format!("restore {id}"))
        }
    };
    if c == before {
        return Ok(Changed { cairn: before, version, id, changed: false });
    }
    let max = shape.max_words();
    if c.words() > max && c.words() > before.words() {
        bail!(
            "the cairn would have {} words, and holds at most {max}: shorten or remove a line first",
            c.words()
        );
    }
    let w = Write {
        author: "user",
        run: None,
        reason: Some(&reason),
        gone: &gone,
        default_gone: REMOVED,
        capped: &[],
        now,
    };
    if !write_doc(&tx, owner, version, &c, &w)? {
        bail!("the cairn changed while this was written: try again");
    }
    let (cairn, version) = document(&tx, owner)?.unwrap_or_default();
    tx.commit()?;
    // A new bullet's id is the one it was given.
    let id = match change {
        Change::Add(s, _) => cairn.section(s).last().map(|i| i.id),
        _ => id,
    };
    Ok(Changed { cairn, version, id, changed: true })
}

/// Why line `id` is not in the cairn to change: a whole-document edit
/// changed it into another line, or there was never such a line.
fn missing(conn: &Connection, owner: Owner, id: i64) -> Result<anyhow::Error> {
    Ok(match replaced_by(conn, owner, id)? {
        Some(now) => anyhow!("line #{id} was changed since: it is #{now} now"),
        None => anyhow!("no line #{id} in this cairn: varde show lists the current ids"),
    })
}

/// The current line that line `id` became, when whole-document edits
/// changed it: each such edit takes the old words to history and writes
/// the new as a new line, in the same section at the same moment, so
/// the line written then in its place, or the only one in its section,
/// is what it became. Followed through later edits of that one.
fn replaced_by(conn: &Connection, owner: Owner, id: i64) -> Result<Option<i64>> {
    let col = owner.column();
    let mut at = id;
    // A bound, as each step is an edit someone made.
    for _ in 0..50 {
        // Its state, why it left, its section and place, and when.
        type Line = (String, Option<String>, String, i64, Option<i64>);
        let row: Option<Line> = conn
            .query_row(
                &format!("SELECT state, reason, section, pos, left_at FROM cairn_item WHERE id = ?1 AND {col} = ?2"),
                params![at, owner.key()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((state, reason, section, pos, left)) = row else { return Ok(None) };
        if state == "current" {
            return Ok((at != id).then_some(at));
        }
        if !matches!(reason.as_deref(), Some("removed in varde edit" | "removed by an edit")) {
            return Ok(None);
        }
        let written: Vec<(i64, i64)> = conn
            .prepare(&format!(
                "SELECT id, pos FROM cairn_item
                 WHERE {col} = ?1 AND section = ?2 AND created = ?3 AND author = 'user' AND of IS NULL AND id != ?4"
            ))?
            .query_map(params![owner.key(), section, left, at], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        at = match written.as_slice() {
            [(one, _)] => *one,
            many => match many.iter().find(|(_, p)| *p == pos) {
                Some((one, _)) => *one,
                None => return Ok(None),
            },
        };
    }
    Ok(None)
}
