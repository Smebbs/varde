//! The board's keys: what each does, and the ones the user binds in the
//! config's `[keys]`, by the action's name (`park = "P"`). A key bound
//! there does that action in place of what it did; every other key keeps
//! its default. The footer and `?` show the keys as bound.

use std::collections::BTreeMap;

use ratatui::crossterm::event::KeyCode;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Act {
    Start,
    Background,
    Fresh,
    Fork,
    NextNeeds,
    PrevNeeds,
    Steward,
    Trust,
    New,
    Edit,
    Done,
    Park,
    Drop,
    Reopen,
    Accept,
    Move,
    Mark,
    View,
    Cairn,
    Editor,
    Diff,
    Recall,
    Down,
    Up,
    First,
    Last,
    NextGroup,
    PrevGroup,
    HalfDown,
    HalfUp,
    Fold,
    Scope,
    Refresh,
    Quit,
    Help,
}

use Act::*;
use KeyCode::Char;

/// Each action: its name in `[keys]`, its default keys, and the key `?`
/// shows when it has none of those: a Ctrl key, which `[keys]` cannot
/// bind. `g` as `first`'s key is pressed twice, as in vi.
const ACTS: &[(Act, &str, &[KeyCode], &str)] = &[
    (Start, "start", &[KeyCode::Enter], ""),
    (Background, "background", &[Char('s')], ""),
    (Fresh, "fresh", &[Char('R')], ""),
    (Fork, "fork", &[Char('F')], ""),
    (NextNeeds, "next-needs", &[KeyCode::Tab], ""),
    (PrevNeeds, "prev-needs", &[KeyCode::BackTab], ""),
    (Steward, "steward", &[Char('S')], ""),
    (Trust, "trust", &[Char('T')], ""),
    (New, "new", &[Char('n')], ""),
    (Edit, "edit", &[Char('e')], ""),
    (Done, "done", &[Char('d')], ""),
    (Park, "park", &[Char('P')], ""),
    (Drop, "drop", &[Char('x')], ""),
    (Reopen, "reopen", &[Char('u')], ""),
    (Accept, "accept", &[Char('a')], ""),
    (Move, "move", &[Char('m')], ""),
    (Mark, "mark", &[Char(' ')], ""),
    (View, "view", &[Char('v')], ""),
    // No key of its own: the view's Project tab is `v` then `]`.
    (Cairn, "cairn", &[], ""),
    (Editor, "editor", &[Char('E')], ""),
    (Diff, "diff", &[Char('=')], ""),
    (Recall, "recall", &[Char('/')], ""),
    (Down, "down", &[Char('j'), KeyCode::Down], ""),
    (Up, "up", &[Char('k'), KeyCode::Up], ""),
    (First, "first", &[Char('g'), KeyCode::Home], ""),
    (Last, "last", &[Char('G'), KeyCode::End], ""),
    (NextGroup, "next-group", &[Char('}')], ""),
    (PrevGroup, "prev-group", &[Char('{')], ""),
    (HalfDown, "half-down", &[KeyCode::PageDown], "C-d"),
    (HalfUp, "half-up", &[KeyCode::PageUp], "C-u"),
    (Fold, "fold", &[Char('z')], ""),
    (Scope, "scope", &[Char('.')], ""),
    (Refresh, "refresh", &[], "C-l"),
    (Quit, "quit", &[Char('q')], ""),
    (Help, "help", &[Char('?')], ""),
];

/// Names `[keys]` took before the board's keys were made one with the
/// Emacs board's, still read.
const OLD: &[(&str, &str)] =
    &[("detail", "view"), ("project", "cairn"), ("edit-project", "editor"), ("parked", "fold")];

/// A group of what `?` shows: its name, and lines, each the keys of its
/// actions and what they do.
pub(super) type HelpGroup = (&'static str, &'static [(&'static [Act], &'static str)]);

pub(super) const HELP: [HelpGroup; 4] = [
    (
        "Go",
        &[
            (&[Start], "go to it, starting it"),
            (&[NextNeeds, PrevNeeds], "cycle what needs you"),
            (&[Background], "start it, stay here"),
            (&[Fresh, Fork], "fresh start / fork"),
        ],
    ),
    (
        "Cairns",
        &[
            (&[View], "view task and project"),
            (&[Editor], "whole cairn in $EDITOR"),
            (&[Recall], "recall: search cairns"),
            (&[Diff], "diff of its work"),
        ],
    ),
    (
        "Task",
        &[
            (&[New], "new task"),
            (&[Edit], "edit title and context"),
            (&[Done, Park, Drop], "done / park / drop"),
            (&[Reopen], "reopen what d/P/x closed"),
            (&[Accept], "accept an Inbox task"),
            (&[Move], "move to another project"),
            (&[Mark], "mark for a, d, P, x"),
        ],
    ),
    (
        "Board",
        &[
            (&[Down, Up], "next / previous task"),
            (&[First, Last], "first / last task"),
            (&[PrevGroup, NextGroup], "previous / next group"),
            (&[HalfDown, HalfUp], "half a page down / up"),
            (&[Fold], "fold or open the group"),
            (&[Scope], "this project / every one"),
            (&[Steward, Trust], "steward / hand it over"),
            (&[Refresh], "redraw and refresh"),
            (&[Quit], "close the pane; q quits"),
        ],
    ),
];

#[derive(Default)]
pub(super) struct Keys {
    /// The user's keys, each with its action.
    bound: Vec<(KeyCode, Act)>,
}

impl Keys {
    /// The keys `[keys]` binds; those it cannot use were refused when
    /// the config was read.
    pub(super) fn of(names: &BTreeMap<String, String>) -> Keys {
        let bound = names.iter().filter_map(|(name, key)| Some((parse(key)?, named(name)?))).collect();
        Keys { bound }
    }

    /// What `code` does on the board.
    pub(super) fn act(&self, code: KeyCode) -> Option<Act> {
        if let Some((_, act)) = self.bound.iter().find(|(k, _)| *k == code) {
            return Some(*act);
        }
        ACTS.iter().find(|a| a.2.contains(&code)).map(|a| a.0)
    }

    /// The key to show for `act`: the user's, else its first default
    /// that still does it, else its Ctrl key.
    pub(super) fn label(&self, act: Act) -> String {
        let (_, _, defaults, ctrl) = ACTS.iter().find(|a| a.0 == act).expect("every action is listed");
        let user = self.bound.iter().find(|(_, a)| *a == act).map(|(k, _)| *k);
        let default = defaults.iter().find(|k| self.act(**k) == Some(act)).copied();
        match (user, default) {
            (Some(Char('g')), _) | (None, Some(Char('g'))) if act == First => "gg".into(),
            (Some(k), _) => name(k),
            _ if !ctrl.is_empty() => ctrl.to_string(),
            (None, Some(k)) => name(k),
            (None, None) => "-".into(),
        }
    }

    /// The keys of `acts`, as one: `j/k`.
    pub(super) fn labels(&self, acts: &[Act]) -> String {
        acts.iter().map(|a| self.label(*a)).collect::<Vec<_>>().join("/")
    }

    /// The footer for what is selected, in `width` columns: the keys in
    /// a fixed order, the least needed left out until it fits; `?` has
    /// the rest. `state` is the selected task's, `folded` for a folded
    /// group, and `None` for nothing. An Inbox row offers accepting it, a
    /// closed one reopening it, and Tab is offered while something on
    /// the board needs you; `u` while there is a task closed here to
    /// reopen. With tasks marked, it says how many, and the keys that act
    /// on them all.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn footer(
        &self,
        state: Option<&str>,
        inbox: bool,
        needs: bool,
        reopen: bool,
        marked: usize,
        width: usize,
    ) -> Vec<(String, &'static str)> {
        if marked > 0 {
            let all = [
                (Mark, "mark", 1, true),
                (Accept, "accept", 2, inbox),
                (Done, "done", 1, true),
                (Park, "park", 2, true),
                (Drop, "drop", 2, true),
                (Help, "help", 3, true),
            ];
            let mut keys = vec![(marked.to_string(), "marked", 0)];
            keys.extend(
                all.iter().filter(|a| a.3).map(|&(act, what, need, _)| (self.label(act), what, need)),
            );
            keys.push(("Esc".into(), "unmark all", 1));
            return fit(keys, width);
        }
        let go = match state {
            Some("done" | "parked" | "dropped") => "reopen",
            Some("folded") => "open",
            _ => "go",
        };
        let task = state.is_some_and(|s| s != "folded");
        // What would do nothing to the row is left out: `d` on a done task.
        let not = |s: &str| task && state != Some(s);
        // Each with how much it is needed: 1 most.
        let all = [
            (Start, go, 1, state.is_some()),
            (NextNeeds, "needs you", 2, needs),
            (New, "new", 2, true),
            (Accept, "accept", 2, inbox),
            (View, "view", 3, task),
            (Done, "done", 3, not("done")),
            (Park, "park", 5, not("parked")),
            (Drop, "drop", 6, not("dropped")),
            (Reopen, "reopen last", 3, reopen),
            (Recall, "recall", 4, true),
            (Help, "help", 1, true),
            (Quit, "quit", 4, true),
        ];
        fit(
            all.iter().filter(|a| a.3).map(|&(act, what, need, _)| (self.label(act), what, need)).collect(),
            width,
        )
    }
}

/// Keys and what they do in `width` columns, in their order, the least
/// needed (the highest number; the later of two alike) left out until
/// they fit.
pub(super) fn fit(mut keys: Vec<(String, &'static str, u8)>, width: usize) -> Vec<(String, &'static str)> {
    let wide = |k: &[(String, &str, u8)]| {
        k.iter().map(|(k, w, _)| k.chars().count() + w.chars().count() + 3).sum::<usize>()
    };
    while wide(&keys) > width + 2 {
        let Some(least) = keys.iter().enumerate().max_by_key(|(i, k)| (k.2, *i)).map(|(i, _)| i) else {
            break;
        };
        keys.remove(least);
    }
    keys.into_iter().map(|(k, w, _)| (k, w)).collect()
}

/// The action `[keys]` names, by its name or the one it had.
fn named(name: &str) -> Option<Act> {
    let name = OLD.iter().find(|o| o.0 == name).map_or(name, |o| o.1);
    ACTS.iter().find(|a| a.1 == name).map(|a| a.0)
}

/// A key as `[keys]` writes it: one character, or `space`, `tab`,
/// `backtab`, `enter`, `esc`, `home`, `end`, `up` or `down`.
fn parse(s: &str) -> Option<KeyCode> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => return Some(Char(c)),
        _ => {}
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "space" => Char(' '),
        "tab" => KeyCode::Tab,
        "backtab" | "s-tab" => KeyCode::BackTab,
        "enter" | "ret" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        _ => return None,
    })
}

fn name(code: KeyCode) -> String {
    match code {
        Char(' ') => "space".into(),
        Char(c) => c.to_string(),
        KeyCode::Enter => "RET".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "S-Tab".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageDown => "PgDn".into(),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        _ => "?".into(),
    }
}

/// What is wrong with `[keys]`, for the config's reader: an action that
/// does not exist, a key it cannot read, or one key bound twice.
pub(crate) fn check(names: &BTreeMap<String, String>) -> Result<(), String> {
    let mut seen: Vec<(KeyCode, &str)> = Vec::new();
    for (action, key) in names {
        if named(action).is_none() {
            let known: Vec<&str> = ACTS.iter().map(|a| a.1).collect();
            return Err(format!("keys.{action} is no action of the board's; they are {}", known.join(", ")));
        }
        let Some(code) = parse(key) else {
            return Err(format!(
                "keys.{action} = {key:?}: a key is one character, or space, tab, backtab, enter, esc, home, end, up or down"
            ));
        };
        // Esc unmarks and closes a pane first, whatever else it does.
        if code == KeyCode::Esc && named(action) != Some(Quit) {
            return Err(format!("keys.{action} = {key:?}: Esc can only quit"));
        }
        if let Some((_, other)) = seen.iter().find(|(k, _)| *k == code) {
            return Err(format!("keys.{action} and keys.{other} are both {key:?}"));
        }
        seen.push((code, action));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pairs: &[(&str, &str)]) -> Keys {
        let map = pairs.iter().map(|(a, k)| (a.to_string(), k.to_string())).collect();
        check(&map).unwrap();
        Keys::of(&map)
    }

    fn text(f: Vec<(String, &str)>) -> String {
        f.iter().map(|(k, w)| format!("{k} {w}")).collect::<Vec<_>>().join("  ")
    }

    #[test]
    fn a_bound_key_takes_over_and_the_rest_keep_their_defaults() {
        let k = keys(&[("park", "Z"), ("done", "D")]);
        assert_eq!(k.act(Char('Z')), Some(Park));
        assert_eq!(k.act(Char('P')), Some(Park), "a default stays");
        assert_eq!(k.act(Char('D')), Some(Done));
        assert_eq!(k.act(Char('d')), Some(Done));
        assert_eq!(k.label(Park), "Z");
        let k = keys(&[("down", "n"), ("new", "N")]);
        assert_eq!(k.act(Char('n')), Some(Down), "n now moves");
        assert_eq!(k.label(New), "N");
        assert_eq!(k.label(Down), "n");
        assert!(text(k.footer(Some("open"), false, false, false, 0, 80)).starts_with("RET go  N new  v view"));
    }

    #[test]
    fn a_default_key_another_action_took_is_not_shown_for_its_old_one() {
        let k = keys(&[("refresh", "j")]);
        assert_eq!(k.act(Char('j')), Some(Refresh));
        assert_eq!((k.label(Down), k.label(Refresh)), ("↓".into(), "j".into()));
        assert_eq!(Keys::default().label(Refresh), "C-l");
        assert_eq!(Keys::default().labels(&[First, Last]), "gg/G");
        assert_eq!(keys(&[("first", "H")]).label(First), "H");
    }

    #[test]
    fn the_old_names_still_bind() {
        let k = keys(&[("detail", "o"), ("parked", "Z")]);
        assert_eq!((k.act(Char('o')), k.act(Char('Z'))), (Some(View), Some(Fold)));
    }

    #[test]
    fn what_keys_cannot_use_is_said() {
        let bad = |pairs: &[(&str, &str)]| {
            check(&pairs.iter().map(|(a, k)| (a.to_string(), k.to_string())).collect()).unwrap_err()
        };
        assert!(bad(&[("parc", "z")]).contains("no action"));
        assert!(bad(&[("park", "ctrl-x")]).contains("one character"));
        assert!(bad(&[("park", "Z"), ("done", "Z")]).contains("both"));
        assert_eq!(keys(&[("mark", "space"), ("start", "o")]).act(Char(' ')), Some(Mark));
        assert!(bad(&[("help", "esc")]).contains("Esc can only quit"));
        let k = keys(&[("quit", "Esc")]);
        assert_eq!((k.act(KeyCode::Esc), k.label(Quit)), (Some(Quit), "Esc".into()));
    }

    #[test]
    fn the_footer_fits_its_width_leaving_out_the_least_needed() {
        let k = Keys::default();
        assert_eq!(
            text(k.footer(Some("open"), false, false, false, 0, 80)),
            "RET go  n new  v view  d done  P park  x drop  / recall  ? help  q quit"
        );
        for (state, inbox, needs) in
            [(Some("open"), true, true), (Some("folded"), false, true), (Some("done"), true, false)]
        {
            let f = text(k.footer(state, inbox, needs, false, 0, 80));
            assert!(f.chars().count() <= 80, "{f}");
            assert!(f.starts_with("RET ") && f.contains("? help"), "{f}");
        }
        let f = text(k.footer(Some("open"), true, true, false, 0, 80));
        assert!(f.contains("Tab needs you") && f.contains("a accept") && f.contains("d done"), "{f}");
        assert!(text(k.footer(Some("done"), false, false, false, 0, 80)).starts_with("RET reopen"));
        assert!(text(k.footer(Some("open"), true, true, false, 0, 40)).chars().count() <= 40);
        let none = text(k.footer(None, false, true, false, 0, 80));
        assert_eq!(none, "Tab needs you  n new  / recall  ? help  q quit", "nothing selected");
        assert!(text(k.footer(Some("folded"), false, false, false, 0, 80))
            .starts_with("RET open  n new  / recall"));
        // What would do nothing to the row is left out.
        let f = text(k.footer(Some("done"), false, false, false, 0, 80));
        assert!(!f.contains("d done") && f.contains("P park") && f.contains("x drop"), "{f}");
        let f = text(k.footer(Some("parked"), false, false, false, 0, 80));
        assert!(f.contains("d done") && !f.contains("P park"), "{f}");
        let f = text(k.footer(Some("open"), false, false, true, 0, 80));
        assert!(f.contains("u reopen last") && f.chars().count() <= 80, "{f}");
        let f = text(k.footer(Some("open"), true, false, false, 2, 80));
        assert_eq!(f, "2 marked  space mark  a accept  d done  P park  x drop  ? help  Esc unmark all");
    }
}
