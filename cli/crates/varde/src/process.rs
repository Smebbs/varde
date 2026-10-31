//! Is an agent's process still the one running? A pid alone is not
//! enough, since pids are reused, so the process's start time is recorded
//! beside it and both must match.

/// An opaque start-time token for `pid`, or `None` if it is not running
/// or the platform cannot say.
pub fn start_time(pid: u32) -> Option<i64> {
    imp::start_time(pid)
}

pub fn alive(pid: i64, start: i64) -> bool {
    // 0 marks a run claimed but not yet started.
    if pid <= 0 {
        return false;
    }
    match u32::try_from(pid) {
        Ok(p) => match start_time(p) {
            Some(t) => t == start,
            // No start time on this platform: fall back to existence.
            None => start == 0 && unsafe { libc::kill(p as libc::pid_t, 0) } == 0,
        },
        Err(_) => false,
    }
}

/// The agent process that called this hook.
pub struct Agent {
    /// `None` when no ancestor goes by the agent's name: a wrong pid would
    /// end a live session the moment that process exits.
    pub pid: Option<u32>,
    /// Its terminal, such as `/dev/pts/3`, so a client can find the
    /// window it is in; `None` when it has none.
    pub tty: Option<String>,
}

/// The nearest ancestor whose command name is one of `names`, past a
/// shell and any wrapper between it and the hook. `None` when the
/// platform cannot say.
pub fn agent(names: &[&str]) -> Option<Agent> {
    let mut pid = std::os::unix::process::parent_id();
    let mut found = Agent { pid: None, tty: None };
    for depth in 0..8 {
        let Some((name, parent, tty)) = imp::info(pid) else {
            return (depth > 0).then_some(found);
        };
        found.tty = found.tty.or(tty);
        if names.contains(&name.as_str()) {
            found.pid = Some(pid);
            break;
        }
        if parent <= 1 {
            break;
        }
        pid = parent;
    }
    Some(found)
}

/// Whether `pid` runs under the process `ancestor` (pid and start time),
/// as an agent does under a wrapper that did not `exec` it.
/// This process's ancestors, nearest first: each one's pid and command
/// name, as far as the platform can say.
pub fn ancestors() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..16 {
        let Some((name, parent, _)) = imp::info(pid) else { break };
        out.push((pid, name));
        if parent <= 1 {
            break;
        }
        pid = parent;
    }
    out
}

pub fn descends(pid: u32, ancestor: (i64, i64)) -> bool {
    let mut pid = pid;
    for _ in 0..8 {
        let Some((_, parent, _)) = imp::info(pid) else { return false };
        if i64::from(parent) == ancestor.0 {
            return start_time(parent).unwrap_or(0) == ancestor.1;
        }
        if parent <= 1 {
            return false;
        }
        pid = parent;
    }
    false
}

#[cfg(target_os = "linux")]
mod imp {
    /// A process's command name, parent and terminal.
    pub fn info(pid: u32) -> Option<(String, u32, Option<String>)> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let comm = &stat[stat.find('(')? + 1..stat.rfind(')')?];
        // Fields after the command name: state, ppid, pgrp, session, tty_nr.
        let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
        Some((comm.to_string(), fields.get(1)?.parse().ok()?, pts(fields.get(4)?.parse().ok()?)))
    }

    /// The name of a pseudo-terminal from its device number.
    fn pts(dev: i64) -> Option<String> {
        let major = (dev >> 8) & 0xfff;
        let minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
        (136..=143).contains(&major).then(|| format!("/dev/pts/{}", (major - 136) * 256 + minor))
    }

    pub fn start_time(pid: u32) -> Option<i64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name may contain spaces and parentheses; fields are
        // counted from after its closing parenthesis. Start time is field 22.
        let rest = &stat[stat.rfind(')')? + 1..];
        rest.split_whitespace().nth(19)?.parse().ok()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    fn bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        (n == size).then_some(info)
    }

    /// A process's command name, parent and terminal, in one call.
    pub fn info(pid: u32) -> Option<(String, u32, Option<String>)> {
        let info = bsdinfo(pid)?;
        let comm: Vec<u8> = info.pbi_comm.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        // No terminal is NODEV, all ones.
        let tty = (info.e_tdev != u32::MAX).then(|| tty_name(info.e_tdev)).flatten();
        Some((String::from_utf8_lossy(&comm).into_owned(), info.pbi_ppid, tty))
    }

    fn tty_name(dev: u32) -> Option<String> {
        let name = unsafe { libc::devname(dev as libc::dev_t, libc::S_IFCHR) };
        if name.is_null() {
            return None;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_str().ok()?;
        (!name.is_empty() && !name.starts_with('?')).then(|| format!("/dev/{name}"))
    }

    pub fn start_time(pid: u32) -> Option<i64> {
        let info = bsdinfo(pid)?;
        Some(info.pbi_start_tvsec as i64 * 1_000_000 + info.pbi_start_tvusec as i64)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    pub fn info(_pid: u32) -> Option<(String, u32, Option<String>)> {
        None
    }

    pub fn start_time(_pid: u32) -> Option<i64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_a_reused_pid_is_not() {
        let me = std::process::id();
        let t = start_time(me).expect("start time");
        assert!(alive(me as i64, t));
        assert!(!alive(me as i64, t + 1));
    }

    #[test]
    fn a_finished_process_is_not_alive() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let t = start_time(pid).unwrap_or(0);
        child.wait().unwrap();
        assert!(!alive(pid as i64, t));
    }

    #[test]
    fn a_child_descends_from_this_process_and_not_the_other_way() {
        let me = std::process::id();
        let mine = (me as i64, start_time(me).unwrap_or(0));
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        assert!(descends(child.id(), mine));
        assert!(!descends(child.id(), (mine.0, mine.1 + 1)), "another process that had the pid");
        let theirs = (child.id() as i64, start_time(child.id()).unwrap_or(0));
        assert!(!descends(me, theirs));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_process_knows_its_parent() {
        let (_, parent, _) = imp::info(std::process::id()).expect("process info");
        assert_eq!(parent, std::os::unix::process::parent_id());
    }
}
