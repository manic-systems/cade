//! Animated activation spinner rendered on stderr.
//!
//! One spinner is active per process while an environment activates. A
//! background thread repaints a bracketed `/ | \ -` frame in blue; `run_checked`
//! feeds it recent command output and flips it into the yellow long-running
//! state; `do_activation` resolves it to a green tick or red cross next to the
//! `cade: loaded ...` message. Every other stderr writer goes through
//! [`log_line`] (via `verbosity::log`) so its output never collides with the
//! live spinner.

use crate::verbosity::{self, Verbosity};
use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

const FRAMES: [char; 4] = ['/', '-', '\\', '|'];
const LOADED: char = '\u{2192}'; // → layer applied
const EVICTED: char = '\u{2190}'; // ← layer peeled off
const CROSS: char = '\u{2717}'; // ✗ ballot x
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const RECENT_LINES: usize = 5;

// Standard ANSI colours only.
const BLUE: &str = "\x1b[34m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

static STATE: Mutex<Option<State>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);

struct State {
    message: String,
    frame: usize,
    long_running: bool,
    recent: Vec<String>,
    nix_bar: Option<String>,
    visible_rows: usize,
}

impl State {
    fn block(&self) -> Vec<String> {
        let colour = if self.long_running { YELLOW } else { BLUE };
        let frame = FRAMES[self.frame % FRAMES.len()];
        let mut lines = vec![format!("[{colour}{frame}{RESET}] {}", self.message)];
        if self.long_running {
            lines.extend(self.recent.iter().map(|line| format!("    {line}")));
            if let Some(bar) = &self.nix_bar {
                lines.push(bar.clone());
            }
        }
        lines
    }

    fn render(&mut self) {
        let block = self.block();
        let mut err = std::io::stderr().lock();
        rewind(&mut err, self.visible_rows);
        self.visible_rows = render_block(&mut err, &block);
        let _ = err.flush();
    }
}

/// Move the cursor back to the top of a `n`-row block, clearing it on the way.
pub(crate) fn rewind(err: &mut impl Write, n: usize) {
    if n == 0 {
        return;
    }
    let _ = write!(err, "\x1b[{n}F");
    for _ in 0..n {
        let _ = writeln!(err, "\x1b[2K");
    }
    let _ = write!(err, "\x1b[{n}F");
}

pub(crate) fn render_block(err: &mut impl Write, lines: &[String]) -> usize {
    let width = terminal_width().unwrap_or(80).max(1);
    for line in lines {
        let line = fit_terminal_line(line, width);
        let _ = write!(err, "{line}\x1b[K\r\n");
    }
    lines.len()
}

fn fit_terminal_line(line: &str, width: usize) -> String {
    let max_columns = width.saturating_sub(1).max(1);
    if visible_columns(line) <= max_columns {
        return line.to_string();
    }

    let suffix = if max_columns >= 3 {
        "..."
    } else if max_columns == 2 {
        ".."
    } else {
        "."
    };
    let keep_columns = max_columns.saturating_sub(suffix.len());
    let mut out = take_visible_columns(line, keep_columns);
    out.push_str(RESET);
    out.push_str(suffix);
    out
}

fn visible_columns(line: &str) -> usize {
    let mut columns = 0;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        } else {
            columns += 1;
        }
    }
    columns
}

fn take_visible_columns(line: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }

    let mut out = String::new();
    let mut visible = 0;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            out.push(ch);
            out.push(chars.next().unwrap());
            for ch in chars.by_ref() {
                out.push(ch);
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        } else {
            if visible == columns {
                break;
            }
            out.push(ch);
            visible += 1;
        }
    }
    out
}

#[cfg(unix)]
fn terminal_width() -> Option<usize> {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: ioctl writes a winsize into the valid out pointer when stderr is a tty.
    let result = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) };
    if result == 0 {
        // SAFETY: ioctl returned success, so the winsize has been initialized.
        let size = unsafe { size.assume_init() };
        (size.ws_col > 0).then_some(size.ws_col as usize)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn terminal_width() -> Option<usize> {
    None
}

/// True while a live spinner owns the terminal.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// A bracketed yellow `[←]` eviction marker for unload notices, mirroring the
/// green `[→]` load arrow. Empty when stderr is not a terminal, so piped output
/// stays plain.
pub fn eviction_marker() -> String {
    if std::io::stderr().is_terminal() {
        format!("[{YELLOW}{EVICTED}{RESET}] ")
    } else {
        String::new()
    }
}

/// green `[→]` marker for layer notices emitted outside the spinner; empty off-terminal
pub fn load_marker() -> String {
    if std::io::stderr().is_terminal() {
        format!("[{GREEN}{LOADED}{RESET}] ")
    } else {
        String::new()
    }
}

/// Replace the spinner's recent-output tail (shown once long-running).
pub fn set_recent(lines: Vec<String>) {
    if !is_active() {
        return;
    }
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        let start = lines.len().saturating_sub(RECENT_LINES);
        state.recent = lines[start..].to_vec();
        if state.long_running {
            state.render();
        }
    }
}

/// Replace the reconstructed nix progress bar shown below the recent output.
pub fn set_nix_bar(bar: Option<String>) {
    if !is_active() {
        return;
    }
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        state.nix_bar = bar;
        if state.long_running {
            state.render();
        }
    }
}

/// Flip the spinner into its yellow long-running state with a new message.
pub fn mark_long_running(message: String) {
    if !is_active() {
        return;
    }
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        state.long_running = true;
        state.message = message;
        state.render();
    }
}

fn durable_recent_block(state: &State) -> Vec<String> {
    if !state.long_running || state.recent.is_empty() {
        return Vec::new();
    }

    let mut lines = vec!["cade: recent output:".to_string()];
    lines.extend(state.recent.iter().map(|line| format!("    {line}")));
    lines
}

/// Emit a stderr line, stepping around the live spinner if one is drawing.
pub fn log_line(line: &str) {
    if !is_active() {
        eprintln!("{line}");
        return;
    }
    let mut guard = STATE.lock().unwrap();
    match guard.as_mut() {
        Some(state) => {
            let mut err = std::io::stderr().lock();
            rewind(&mut err, state.visible_rows);
            state.visible_rows = 0;
            let _ = writeln!(err, "{line}");
            let _ = err.flush();
        }
        None => eprintln!("{line}"),
    }
}

/// Start a spinner labelled for `subject` (a path). Inert (a no-op handle) when
/// stderr is not a terminal, output is quiet, or one is already running.
pub fn start(subject: &str) -> Spinner {
    if !verbosity::enabled(Verbosity::Normal)
        || !std::io::stderr().is_terminal()
        || ACTIVE.swap(true, Ordering::AcqRel)
    {
        return Spinner {
            active: false,
            resolved: false,
            thread: None,
        };
    }

    *STATE.lock().unwrap() = Some(State {
        message: format!("cade: loading {subject}"),
        frame: 0,
        long_running: false,
        recent: Vec::new(),
        nix_bar: None,
        visible_rows: 0,
    });

    let thread = std::thread::spawn(run_loop);
    Spinner {
        active: true,
        resolved: false,
        thread: Some(thread),
    }
}

fn run_loop() {
    while ACTIVE.load(Ordering::Acquire) {
        if let Some(state) = STATE.lock().unwrap().as_mut() {
            state.frame = state.frame.wrapping_add(1);
            state.render();
        }
        std::thread::park_timeout(FRAME_INTERVAL);
    }
}

/// Handle to the running spinner. Resolves to a green tick on [`Spinner::success`]
/// and, if dropped without one (an error unwound past it), to a red cross.
pub struct Spinner {
    active: bool,
    resolved: bool,
    thread: Option<JoinHandle<()>>,
}

impl Spinner {
    /// Resolve to a green tick beside `message`.
    pub fn success(mut self, message: &str) {
        self.resolved = true;
        if self.active {
            self.finish(GREEN, LOADED, message.to_string());
        } else {
            verbosity::log(Verbosity::Normal, format_args!("{message}"));
        }
    }

    /// resolve with no message; for a silent recompose where another notice carries the news
    pub fn done(mut self) {
        self.resolved = true;
        if !self.active {
            return;
        }
        ACTIVE.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        let (visible, recent) = STATE
            .lock()
            .unwrap()
            .take()
            .map(|state| (state.visible_rows, durable_recent_block(&state)))
            .unwrap_or((0, Vec::new()));
        let mut err = std::io::stderr().lock();
        rewind(&mut err, visible);
        for line in recent {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
    }

    fn finish(&mut self, colour: &str, symbol: char, message: String) {
        ACTIVE.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        let (visible, recent) = STATE
            .lock()
            .unwrap()
            .take()
            .map(|state| (state.visible_rows, durable_recent_block(&state)))
            .unwrap_or((0, Vec::new()));
        let mut err = std::io::stderr().lock();
        rewind(&mut err, visible);
        let _ = writeln!(err, "[{colour}{symbol}{RESET}] {message}");
        for line in recent {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if self.active && !self.resolved {
            self.finish(RED, CROSS, "cade: environment failed to load.".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_columns_ignores_spinner_colour_sequences() {
        let line = format!("[{YELLOW}/{RESET}] cade: loading");
        assert_eq!(visible_columns(&line), 17);
    }

    #[test]
    fn fit_terminal_line_prevents_autowrap() {
        let fitted = fit_terminal_line("1234567890", 10);
        assert!(fitted.ends_with("..."), "{fitted:?}");
        assert_eq!(visible_columns(&fitted), 9);
    }

    #[test]
    fn fit_terminal_line_preserves_ansi_reset_when_truncated() {
        let line = format!("[{YELLOW}/{RESET}] cade: loading a very long path");
        let fitted = fit_terminal_line(&line, 12);
        assert!(fitted.contains(RESET), "{fitted:?}");
        assert_eq!(visible_columns(&fitted), 11);
    }

    #[test]
    fn durable_recent_block_keeps_long_running_command_output() {
        let state = State {
            message: "cade: call `slow` is taking a long time".to_string(),
            frame: 0,
            long_running: true,
            recent: vec!["line2".to_string(), "line3".to_string()],
            nix_bar: None,
            visible_rows: 3,
        };

        assert_eq!(
            durable_recent_block(&state),
            vec![
                "cade: recent output:".to_string(),
                "    line2".to_string(),
                "    line3".to_string()
            ]
        );
    }

    #[test]
    fn durable_recent_block_stays_empty_before_long_running_warning() {
        let state = State {
            message: "cade: loading /project".to_string(),
            frame: 0,
            long_running: false,
            recent: vec!["line".to_string()],
            nix_bar: None,
            visible_rows: 1,
        };

        assert!(durable_recent_block(&state).is_empty());
    }
}
