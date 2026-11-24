//! The board's keys for loops, checks and trails (design §6.7), as the
//! Emacs board has them: `L` loops, `c` sets the check and `C` runs it,
//! `A` approves, `M` picks the profile, `t` shows the task's loop and
//! trail, `O` what its last check printed, and `r` makes tasks from a
//! recipe. Each runs varde's own command, so the board decides nothing
//! and says what varde said.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::field::{Field, Words};
use super::{App, Mode};

/// What a one-line prompt asks for.
#[derive(Clone)]
pub(super) enum Ask {
    /// The command a task loops until it passes.
    Loop(String),
    /// The command that checks a task.
    Check(String),
    /// The profile a task runs on.
    Profile(String),
    /// The recipe to make tasks from.
    Recipe,
    /// The title of the tasks made from this recipe.
    Title(String),
}

/// A prompt on the bottom line: what it asks, its label, and the line typed.
pub(super) struct Prompt {
    pub(super) ask: Ask,
    pub(super) label: String,
    pub(super) field: Field,
}

/// Text over the board, as `?` shows the keys: a task's loop and trail,
/// or what a check printed. Any key closes it.
pub(super) struct Text {
    pub(super) title: String,
    pub(super) lines: Vec<String>,
}

/// `varde <args>`, as a subprocess, so what it prints never reaches the
/// screen the board owns: its output, or what it said went wrong.
fn varde(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(crate::exe())
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.trim().trim_start_matches("varde: ").to_string())
    }
}

/// What varde said, on the message line: its first line.
fn said(r: Result<String, String>) -> String {
    match r {
        Ok(s) => s.lines().find(|l| !l.trim().is_empty()).unwrap_or("done").to_string(),
        Err(e) => format!("error: {e}"),
    }
}

impl App<'_> {
    /// A key of loops, checks and trails, on the selected task.
    pub(super) fn flow_act(&mut self, act: super::keys::Act) {
        use super::keys::Act;
        if act == Act::Recipes {
            let names = varde(&["recipe", "list", "--json"])
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|v| {
                    v["recipes"]
                        .as_array()
                        .map(|a| {
                            a.iter().filter_map(|r| r["name"].as_str().map(String::from)).collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            self.mode = Mode::Ask(Prompt {
                ask: Ask::Recipe,
                label: format!("Recipe ({}): ", names.join(" ")),
                field: Field::default(),
            });
            return;
        }
        let Some(row) = self.current() else {
            self.message = "no task here".into();
            return;
        };
        let (id, short) = (row.task.clone(), row.short.clone());
        let state = row.flow.as_ref().and_then(|f| f.state);
        let flow = || -> serde_json::Value {
            varde(&["show", &id, "--json", "--no-git"])
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|v| v["flow"].clone())
                .unwrap_or_default()
        };
        match act {
            Act::Loop => match state {
                Some("paused") => self.message = said(varde(&["loop", &id])),
                Some("working" | "checking" | "due" | "attended") => {
                    self.message = format!("{}; L again resumes it", said(varde(&["loop", &id, "--pause"])))
                }
                // None yet, or it stopped: what it loops until, to set it going.
                _ => {
                    let cmd = flow()["check"]["cmd"].as_str().unwrap_or("").to_string();
                    self.mode = Mode::Ask(Prompt {
                        ask: Ask::Loop(id),
                        label: format!("Loop {short} until this passes (3 rounds, then deep): "),
                        field: Field::new(&cmd),
                    });
                }
            },
            Act::Check => {
                let cmd = flow()["check"]["cmd"].as_str().unwrap_or("").to_string();
                self.mode = Mode::Ask(Prompt {
                    ask: Ask::Check(id),
                    label: format!("Check {short} with (exits 0 to pass): "),
                    field: Field::new(&cmd),
                });
            }
            Act::CheckNow => {
                // Detached: the row says what it found when it is done.
                self.message = match crate::detach(&["verify", &id], "a check") {
                    Ok(()) => format!("checking {short}; its row says what it found, O its output"),
                    Err(e) => format!("error: {e:#}"),
                };
            }
            Act::Approve => {
                self.message = said(varde(&["approve", &id]));
                self.stale = true;
            }
            Act::Profile => {
                let names = varde(&["profiles", "--json"])
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .map(|v| {
                        v["profiles"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|p| {
                                        Some(format!(
                                            "{} ({})",
                                            p["name"].as_str()?,
                                            p["model"].as_str().unwrap_or("default")
                                        ))
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                let now = flow()["profile"].as_str().unwrap_or("").to_string();
                self.mode = Mode::Ask(Prompt {
                    ask: Ask::Profile(id),
                    label: format!("Profile for {short}, empty for the work role's ({}): ", names.join(", ")),
                    field: Field::new(&now),
                });
            }
            Act::Trail => {
                let mut lines: Vec<String> = varde(&["show", &id, "--no-git"])
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| {
                        // Its trail drawn below, not listed.
                        ["Next ", "Runs ", "Check ", "Rounds ", "        #", "        last ", "        …"]
                            .iter()
                            .any(|p| l.starts_with(p))
                    })
                    .map(String::from)
                    .collect();
                if let Ok(t) = varde(&["trail", &id]) {
                    if t.lines().count() > 1 {
                        lines.push(String::new());
                        lines.extend(t.lines().map(String::from));
                    }
                }
                if lines.is_empty() {
                    lines.push(format!("{short} has no check, loop or trail: L loops it, c sets its check"));
                }
                self.mode = Mode::Text(Text { title: format!("{short}: loop and trail"), lines });
            }
            Act::Output => {
                let file = flow()["last_check"]["output"].as_str().map(String::from);
                match file {
                    None => self.message = format!("no check of {short} has run yet: C runs it"),
                    Some(f) => {
                        let text = crate::flow::tail(std::path::Path::new(&f), 6000);
                        let mut lines: Vec<String> = text.lines().map(String::from).collect();
                        if lines.len() > 200 {
                            lines.drain(..lines.len() - 200);
                        }
                        self.mode =
                            Mode::Text(Text { title: format!("{short}: its last check, {f}"), lines });
                    }
                }
            }
            _ => {}
        }
    }

    /// A key while a prompt is open: RET answers it, Esc leaves it.
    pub(super) fn ask_key(&mut self, mut p: Prompt, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => self.answer_prompt(p),
            KeyCode::Esc => self.message = "cancelled".into(),
            KeyCode::Char('g') if ctrl => self.message = "cancelled".into(),
            _ => {
                p.field.key(key, Words::Prose);
                self.mode = Mode::Ask(p);
            }
        }
    }

    fn answer_prompt(&mut self, p: Prompt) {
        let text = p.field.text().trim().to_string();
        self.message = match p.ask {
            Ask::Loop(id) => {
                let mut args = vec!["loop", &id, "--escalate", "deep"];
                if !text.is_empty() {
                    args.extend(["--check", &text]);
                }
                said(varde(&args))
            }
            Ask::Check(id) if text.is_empty() => said(varde(&["check", &id, "--off"])),
            Ask::Check(id) => said(varde(&["check", &id, "--", &text])),
            Ask::Profile(id) => said(varde(&["set", &id, "--profile", &text])),
            Ask::Recipe if text.is_empty() => "cancelled".into(),
            Ask::Recipe => {
                self.mode = Mode::Ask(Prompt {
                    label: format!("Title of the {text} tasks: "),
                    ask: Ask::Title(text),
                    field: Field::default(),
                });
                return;
            }
            Ask::Title(_) if text.is_empty() => "a task made from a recipe needs a title".into(),
            Ask::Title(recipe) => {
                let project = self.new_task_project();
                let mut args = vec!["new", "--json", "--recipe", &recipe, "--start"];
                if let Some(p) = &project {
                    args.extend(["--project", p.as_str()]);
                }
                args.extend(["--", &text]);
                match varde(&args) {
                    Ok(out) => {
                        let made: serde_json::Value =
                            serde_json::from_str(out.lines().next().unwrap_or("{}")).unwrap_or_default();
                        let n = made["tasks"].as_array().map_or(0, Vec::len);
                        if let Some(first) = made["tasks"][0]["id"].as_str() {
                            self.selected = Some(first.to_string());
                        }
                        format!(
                            "made {n} task{} from {recipe}, going; t shows its trail",
                            if n == 1 { "" } else { "s" }
                        )
                    }
                    Err(e) => format!("error: {e}"),
                }
            }
        };
        self.stale = true;
    }
}

/// The short id at the end of a trail's line, for tests.
#[cfg(test)]
pub(super) fn short_of(line: &str) -> Option<&str> {
    line.split_whitespace().rev().find(|w| w.len() == 6 && w.chars().all(|c| c.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_trail_line_ends_in_its_task() {
        assert_eq!(super::short_of("├─ T Store · fast · ↺ 1/3  bbb222"), Some("bbb222"));
        assert_eq!(varde_core::task::short("T0000000000000000000BBB222"), "bbb222");
    }
}
