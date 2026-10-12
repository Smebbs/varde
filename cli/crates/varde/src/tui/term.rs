//! The terminal under the board: waiting for input, and taking and
//! giving back the screen.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;

/// Whether the board still has a terminal. One that hung up reads as
/// always ready with nothing to read, and crossterm's poll then spins
/// forever, so a board ignoring SIGHUP (say, started under nohup) would
/// burn a core long after its terminal closed.
fn terminal_alive() -> bool {
    unsafe { libc::isatty(0) == 1 }
}

/// Whether the board has a terminal to read keys from and draw on.
pub(super) fn have_terminal() -> bool {
    unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
}

static TERMINATED: AtomicBool = AtomicBool::new(false);

/// SIGTERM ends the board as `q` does, so it gives the terminal back
/// first rather than leaving it in the alternate screen. It interrupts
/// the wait for input.
pub(super) fn quit_on_sigterm() {
    extern "C" fn on_term(_: libc::c_int) {
        TERMINATED.store(true, Ordering::SeqCst);
    }
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t) };
}

pub(super) fn terminated() -> bool {
    TERMINATED.load(Ordering::SeqCst)
}

/// Wait up to `timeout` for input. `None` once the terminal is gone.
pub(super) fn input(timeout: Duration) -> Option<bool> {
    let wait = |ms: libc::c_int| {
        let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut fd, 1, ms) };
        let gone = fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 || !terminal_alive();
        (!gone).then_some(ready > 0)
    };
    wait(0)?;
    // Keys crossterm has already read wait in its own buffer, not on the fd.
    if event::poll(Duration::ZERO).unwrap_or(false) {
        return Some(true);
    }
    // A resize interrupts the wait; crossterm reports it as an event.
    Some(wait(timeout.as_millis() as libc::c_int)? && event::poll(Duration::ZERO).unwrap_or(false))
}

/// Wait for a key press, with the screen left as it is.
pub(super) fn any_key() {
    if ratatui::crossterm::terminal::enable_raw_mode().is_err() {
        return;
    }
    while let Some(ready) = input(Duration::from_secs(1)) {
        if !ready {
            continue;
        }
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
}

/// Take the screen, with pasted text marked as such.
pub(super) fn enter() -> std::io::Result<DefaultTerminal> {
    let term = ratatui::try_init()?;
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    // A panic leaves the terminal as it found it, ratatui's own hook
    // doing the rest.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        previous(info);
    }));
    Ok(term)
}

pub(super) fn leave() {
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    let _ = std::panic::take_hook();
}
