//! The board inside tmux: each task's session gets a window of its own,
//! and the board stays where it is. A window the board opens is the
//! task's (`@varde_task`) and varde's (`@varde_owned`), of the board's
//! store (`@varde_home`), and has the board's environment: starting the
//! task again types into its idle shell, and once the task is closed and
//! the shell is idle the window goes. `varde run` in a pane of your own tags
//! it only while the session runs, so the board can go to it; varde never
//! closes it.
//! Outside tmux the board runs sessions in its own terminal instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Result};
use varde_core::task;

/// What separates the fields `panes` asks tmux for.
const SEP: &str = "|~|";

/// Shells, as tmux names a pane's command. With `$SHELL`, a pane running
/// one of these is back at its prompt.
const SHELLS: &[&str] = &[
    "sh", "bash", "dash", "zsh", "fish", "ksh", "mksh", "oksh", "tcsh", "csh", "nu", "elvish", "xonsh",
    "yash", "ion", "osh", "ysh",
];

/// Whether a pane running `command` is idle at a shell prompt.
pub fn idle(command: &str) -> bool {
    let command = command.trim_start_matches('-');
    SHELLS.contains(&command)
        || std::env::var_os("SHELL").is_some_and(|s| Path::new(&s).file_name().is_some_and(|n| n == command))
}

/// The tmux server to talk to: the one we run in, or a socket of its own.
pub struct Tmux {
    socket: Option<PathBuf>,
}

/// The tmux we run inside, if any.
pub fn here() -> Option<Tmux> {
    std::env::var_os("TMUX").filter(|t| !t.is_empty()).map(|_| Tmux { socket: None })
}

/// The user's tmux: the one we run in, else the default server.
pub fn server() -> Tmux {
    Tmux { socket: None }
}

/// The tmux server on `socket`: a test's own.
#[cfg(test)]
pub fn at(socket: PathBuf) -> Tmux {
    Tmux { socket: Some(socket) }
}

/// The pane we run in, and its `@varde_task`: the one on our terminal.
/// Not `TMUX_PANE`, which a program started from a pane, such as an
/// Emacs, passes on to terminals of its own.
pub fn this_pane() -> Option<(String, String)> {
    let ours = unsafe {
        let name = libc::ttyname(0);
        (!name.is_null()).then(|| std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned())
    }?;
    let p = here()?.panes().ok()?.into_iter().find(|p| p.tty == ours)?;
    Some((p.id, p.task))
}

pub struct Pane {
    pub id: String,
    pub task: String,
    /// Opened by the board for its task, rather than the user's own.
    pub owned: bool,
    /// The store of the board that opened it.
    pub home: String,
    /// On a client's screen: the active pane of the active window of an
    /// attached session.
    pub shown: bool,
    pub tty: String,
    /// Its window, as tmux shows it: `session:index name`.
    pub window: String,
    path: String,
    command: String,
}

impl Pane {
    /// Back at its shell's prompt.
    pub fn idle(&self) -> bool {
        idle(&self.command)
    }

    /// Ring the pane's bell: tmux flags its window in the status line
    /// (`monitor-bell`, on by default) until you go to it.
    pub fn bell(&self) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tty = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(&self.tty);
        if let Ok(mut tty) = tty {
            let _ = tty.write_all(b"\x07");
        }
    }
}

impl Tmux {
    fn tmux(&self, args: &[&str]) -> Result<String> {
        let bin = std::env::var_os("VARDE_TMUX").unwrap_or_else(|| "tmux".into());
        let mut c = Command::new(bin);
        if let Some(s) = &self.socket {
            c.arg("-S").arg(s);
        }
        let out = c.args(args).output()?;
        if !out.status.success() {
            bail!("tmux {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    }

    /// Every pane, a line each. Fields are split by `SEP`, not a tab:
    /// tmux before 3.4 prints a tab in a format as `_`. The window name,
    /// which can hold anything, comes last.
    pub fn panes(&self) -> Result<Vec<Pane>> {
        let format = [
            "#{pane_id}",
            "#{@varde_task}",
            "#{@varde_owned}",
            "#{@varde_home}",
            "#{pane_active}#{window_active}#{session_attached}",
            "#{pane_tty}",
            "#{pane_current_path}",
            "#{pane_current_command}",
            "#{session_name}:#{window_index} #{window_name}",
        ]
        .join(SEP);
        let out = self.tmux(&["list-panes", "-a", "-F", &format])?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let mut f = l.splitn(9, SEP).map(String::from);
                Some(Pane {
                    id: f.next()?,
                    task: f.next()?,
                    owned: !f.next()?.is_empty(),
                    home: f.next()?,
                    shown: f
                        .next()
                        .is_some_and(|s| s.starts_with("11") && s.get(2..).is_some_and(|n| n != "0")),
                    tty: f.next()?,
                    path: f.next()?,
                    command: f.next()?,
                    window: f.next()?,
                })
            })
            .collect())
    }

    /// Go to the pane of `task`'s live session: the one tagged for it, or
    /// the one on `tty`. Whether there was one.
    pub fn focus(&self, task: &str, tty: Option<&str>) -> Result<bool> {
        match find(&self.panes()?, task, tty) {
            Some(p) => {
                self.go(&p.id)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Type `command` into `task`'s window if its shell is idle in `cwd`,
    /// else into a new window there; then go to it, if `go`. `home` is the
    /// store the command is for: a new window gets it, and the rest of
    /// varde's environment and `PATH`, which a window would otherwise take
    /// from the tmux server, and only a window of that store is reused.
    pub fn open(
        &self,
        task: &str,
        title: &str,
        cwd: &Path,
        home: &str,
        command: &str,
        go: bool,
    ) -> Result<()> {
        // tmux reports a pane's directory with links resolved.
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let cwd_s = cwd.to_string_lossy();
        let idle = self
            .panes()?
            .into_iter()
            .find(|p| p.task == task && p.owned && p.home == home && p.path == cwd_s && p.idle());
        let pane = match idle {
            Some(p) => {
                // Whatever was left typed at the prompt goes first.
                self.tmux(&["send-keys", "-t", &p.id, "C-u"])?;
                p.id
            }
            None => {
                // tmux expands formats in both: `#(cmd)` in a title would run.
                let name =
                    literal(&format!("{} {}", task::short(task), title.chars().take(20).collect::<String>()));
                let dir = literal(&cwd_s);
                let mut args = vec!["new-window", "-d", "-P", "-F", "#{pane_id}", "-c", &dir, "-n", &name];
                let env = environment(home, std::env::vars_os());
                for e in &env {
                    args.extend(["-e", e]);
                }
                let pane = self.tmux(&args)?;
                self.tag(&pane, task)?;
                self.tmux(&["set-option", "-p", "-t", &pane, "@varde_owned", "1"])?;
                self.tmux(&["set-option", "-p", "-t", &pane, "@varde_home", home])?;
                // Keys typed before the shell draws its prompt are echoed by
                // the terminal, then again after the prompt: wait for it,
                // briefly. A prompt moves the cursor.
                for _ in 0..50 {
                    if self.tmux(&["display", "-p", "-t", &pane, "#{cursor_x},#{cursor_y}"])? != "0,0" {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                pane
            }
        };
        self.tmux(&["send-keys", "-t", &pane, "-l", command])?;
        self.tmux(&["send-keys", "-t", &pane, "Enter"])?;
        if go {
            self.go(&pane)?;
        }
        Ok(())
    }

    /// Mark `pane` as `task`'s, or as nobody's when `task` is empty.
    pub fn tag(&self, pane: &str, task: &str) -> Result<()> {
        if task.is_empty() {
            self.tmux(&["set-option", "-p", "-u", "-t", pane, "@varde_task"])?;
        } else {
            self.tmux(&["set-option", "-p", "-t", pane, "@varde_task", task])?;
        }
        Ok(())
    }

    /// Close the windows the board opened for `task` that are back at an
    /// idle shell. Returns how many are still busy, and so were left.
    pub fn close(&self, task: &str) -> Result<usize> {
        let mut busy = 0;
        for p in self.panes()?.into_iter().filter(|p| p.task == task && p.owned) {
            if p.idle() {
                self.kill(&p.id)?;
            } else {
                busy += 1;
            }
        }
        Ok(busy)
    }

    pub fn kill(&self, pane: &str) -> Result<()> {
        self.tmux(&["kill-pane", "-t", pane])?;
        Ok(())
    }

    pub fn go(&self, pane: &str) -> Result<()> {
        self.tmux(&["switch-client", "-t", pane])?;
        Ok(())
    }
}

/// `s` as tmux takes it literally where it expands formats.
fn literal(s: &str) -> String {
    s.replace('#', "##")
}

/// What a session the board starts needs of `vars`, the board's
/// environment: the store, varde's other settings (`VARDE_AGENT`, the
/// agent of a new conversation, among them), and `PATH`. Not the markers
/// of a session the board itself may run in.
fn environment(
    home: &str,
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<String> {
    let mut env = vec![format!("VARDE_HOME={home}")];
    for (k, v) in vars {
        let (Some(k), Some(v)) = (k.to_str(), v.to_str()) else { continue };
        let session = ["VARDE_HOME", "VARDE_RUN", "VARDE_INTERNAL"].contains(&k);
        if k == "PATH" || (k.starts_with("VARDE_") && !session) {
            env.push(format!("{k}={v}"));
        }
    }
    env
}

/// `task`'s pane: the one on `tty`, where its session runs, or else the
/// one tagged for it. A tag alone may be an old window back at its shell.
pub fn find<'a>(panes: &'a [Pane], task: &str, tty: Option<&str>) -> Option<&'a Pane> {
    tty.and_then(|t| panes.iter().find(|p| p.tty == t)).or_else(|| panes.iter().find(|p| p.task == task))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_the_board_starts_gets_the_settings_not_the_markers() {
        let vars =
            [("VARDE_RUN", "r"), ("VARDE_INTERNAL", "1"), ("VARDE_HOME", "/other"), ("VARDE_AGENT", "pi")]
                .into_iter()
                .chain([("PATH", "/bin"), ("HOME", "/h")])
                .map(|(k, v)| (k.into(), v.into()));
        assert_eq!(environment("/store", vars), ["VARDE_HOME=/store", "VARDE_AGENT=pi", "PATH=/bin"]);
    }

    /// A tmux server of the test's own, or `None` where tmux is missing.
    fn server(dir: &Path) -> Option<Tmux> {
        let t = Tmux { socket: Some(dir.join("sock")) };
        t.tmux(&["-f", "/dev/null", "new-session", "-d", "-s", "board", "-c", "/"]).ok()?;
        // Not the user's shell, whose start-up can be slow and anything.
        t.tmux(&["set-option", "-g", "default-shell", "/bin/sh"]).ok()?;
        Some(t)
    }

    fn wait_for(t: &Tmux, pane: &str, text: &str) -> String {
        for _ in 0..300 {
            let shown = t.tmux(&["capture-pane", "-p", "-t", pane]).unwrap();
            if shown.contains(text) {
                return shown;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("{text:?} never appeared in {pane}");
    }

    #[test]
    fn a_title_or_directory_with_a_format_is_taken_literally() {
        let dir = tempfile::tempdir().unwrap();
        let Some(t) = server(dir.path()) else { return eprintln!("no tmux; skipped") };
        let cwd = dir.path().canonicalize().unwrap().join("a#{pane_id}");
        std::fs::create_dir(&cwd).unwrap();
        t.open("01TASKBBBBBB", "#(touch pwn) #{host}", &cwd, "/s", "true", false).unwrap();
        let ours: Vec<Pane> = t.panes().unwrap().into_iter().filter(|p| p.task == "01TASKBBBBBB").collect();
        assert_eq!(ours.len(), 1);
        let name = t.tmux(&["display", "-p", "-t", &ours[0].id, "#{window_name}"]).unwrap();
        assert_eq!(name, "bbbbbb #(touch pwn) #{host}");
        assert_eq!(ours[0].path, cwd.to_string_lossy());
        let _ = t.tmux(&["kill-server"]);
    }

    #[test]
    fn a_task_gets_a_window_that_is_reused_and_closed() {
        let dir = tempfile::tempdir().unwrap();
        let Some(t) = server(dir.path()) else { return eprintln!("no tmux; skipped") };
        let cwd = dir.path().canonicalize().unwrap();
        // No client is attached, so there is nowhere to switch; the window is still made.
        t.open("01TASKAAAAAA", "Fix it", &cwd, "/s", "echo first-run", false).unwrap();
        let ours: Vec<Pane> = t.panes().unwrap().into_iter().filter(|p| p.task == "01TASKAAAAAA").collect();
        assert_eq!(ours.len(), 1);
        let shown = wait_for(&t, &ours[0].id, "\nfirst-run");
        assert_eq!(shown.matches("echo first-run").count(), 1, "typed once, after the prompt: {shown}");
        assert_eq!(t.tmux(&["display", "-p", "-t", &ours[0].id, "#{window_name}"]).unwrap(), "aaaaaa Fix it");

        // Back at the shell, starting again types into the same pane.
        t.open("01TASKAAAAAA", "Fix it", &cwd, "/s", "echo second-run", false).unwrap();
        wait_for(&t, &ours[0].id, "second-run");
        assert_eq!(t.panes().unwrap().iter().filter(|p| p.task == "01TASKAAAAAA").count(), 1);

        // Somewhere else, it is a new window.
        t.open("01TASKAAAAAA", "Fix it", Path::new("/"), "/s", "true", false).unwrap();
        assert_eq!(t.panes().unwrap().iter().filter(|p| p.task == "01TASKAAAAAA").count(), 2);

        // A pane busy with something else is left.
        let busy = t.tmux(&["new-window", "-d", "-P", "-F", "#{pane_id}", "exec sleep 300"]).unwrap();
        t.tag(&busy, "01TASKAAAAAA").unwrap();
        t.tmux(&["set-option", "-p", "-t", &busy, "@varde_owned", "1"]).unwrap();
        // Until the shell running it has exec'd, the pane is tmux's fork,
        // then the shell, which looks idle. (GNU's is `gsleep` on macOS.)
        let running = || t.panes().unwrap().iter().any(|p| p.id == busy && p.command.ends_with("sleep"));
        // Generous: a loaded machine can take seconds to get there.
        for _ in 0..500 {
            if running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let seen: Vec<String> =
            t.panes().unwrap().into_iter().filter(|p| p.id == busy).map(|p| p.command).collect();
        assert!(running(), "the busy pane never ran its command: tmux says {seen:?}");
        assert_eq!(t.close("01TASKAAAAAA").unwrap(), 1);
        let left: Vec<Pane> = t.panes().unwrap().into_iter().filter(|p| p.task == "01TASKAAAAAA").collect();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, busy);
        let _ = t.tmux(&["kill-server"]);
    }

    #[test]
    fn a_window_has_the_boards_store_and_is_reused_only_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let Some(t) = server(dir.path()) else { return eprintln!("no tmux; skipped") };
        let cwd = dir.path().canonicalize().unwrap();
        // The server's own environment has no store: the window's comes from the board.
        t.open("01TASKDDDDDD", "Env", &cwd, "/a store", "echo \"[$VARDE_HOME]\"", false).unwrap();
        let ours = |t: &Tmux| -> Vec<Pane> {
            t.panes().unwrap().into_iter().filter(|p| p.task == "01TASKDDDDDD").collect()
        };
        let pane = ours(&t)[0].id.clone();
        wait_for(&t, &pane, "\n[/a store]");
        assert_eq!(ours(&t)[0].home, "/a store");
        // Another store's board opens a window of its own.
        t.open("01TASKDDDDDD", "Env", &cwd, "/b", "true", false).unwrap();
        assert_eq!(ours(&t).len(), 2);
        let _ = t.tmux(&["kill-server"]);
    }

    #[test]
    fn a_pane_of_your_own_is_never_closed_or_reused() {
        let dir = tempfile::tempdir().unwrap();
        let Some(t) = server(dir.path()) else { return eprintln!("no tmux; skipped") };
        let mine = t.panes().unwrap()[0].id.clone();
        t.tag(&mine, "01TASKCCCCCC").unwrap();
        assert_eq!(t.close("01TASKCCCCCC").unwrap(), 0);
        assert!(t.panes().unwrap().iter().any(|p| p.id == mine), "still there");
        t.open("01TASKCCCCCC", "Mine", Path::new("/"), "/s", "true", false).unwrap();
        assert_eq!(t.panes().unwrap().len(), 2, "a window of its own, not typed into mine");
        t.tag(&mine, "").unwrap();
        assert!(t.panes().unwrap().iter().any(|p| p.id == mine && p.task.is_empty()), "untagged");
        let _ = t.tmux(&["kill-server"]);
    }

    #[test]
    fn a_session_is_found_by_its_tag_or_its_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let Some(t) = server(dir.path()) else { return eprintln!("no tmux; skipped") };
        let board = &t.panes().unwrap()[0];
        // Nothing switches without a client, but finding it is what counts.
        assert!(!t.focus("01TASKBBBBBB", None).unwrap());
        assert!(!t.focus("01TASKBBBBBB", Some("/dev/pts/9999")).unwrap());
        assert!(t.focus("01TASKBBBBBB", Some(&board.tty)).is_err_and(|e| e.to_string().contains("client")));
        t.tag(&board.id, "01TASKBBBBBB").unwrap();
        assert!(t.focus("01TASKBBBBBB", None).is_err_and(|e| e.to_string().contains("client")));
        let _ = t.tmux(&["kill-server"]);
    }

    #[test]
    fn the_pane_a_session_runs_on_comes_before_one_only_tagged() {
        let pane = |id: &str, task: &str, tty: &str| Pane {
            id: id.into(),
            task: task.into(),
            owned: true,
            home: String::new(),
            shown: false,
            tty: tty.into(),
            window: String::new(),
            path: String::new(),
            command: String::new(),
        };
        let panes = [pane("%1", "T", "/dev/pts/1"), pane("%2", "", "/dev/pts/2")];
        assert_eq!(find(&panes, "T", Some("/dev/pts/2")).unwrap().id, "%2");
        assert_eq!(find(&panes, "T", Some("/dev/pts/9")).unwrap().id, "%1");
        assert_eq!(find(&panes, "T", None).unwrap().id, "%1");
    }

    #[test]
    fn shells_are_idle_whatever_their_kind() {
        for s in ["bash", "-zsh", "fish", "nu", "elvish", "tcsh"] {
            assert!(idle(s), "{s}");
        }
        assert!(!idle("claude") && !idle("vim") && !idle("varde"));
    }
}
