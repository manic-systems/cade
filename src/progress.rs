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

const FRAMES: [char; 4] = ['/', '|', '\\', '-'];
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
    visible_lines: usize,
}

impl State {
    fn block(&self) -> Vec<String> {
        let colour = if self.long_running { YELLOW } else { BLUE };
        let frame = FRAMES[self.frame % FRAMES.len()];
        let mut lines = vec![format!("[{colour}{frame}{RESET}] {}", self.message)];
        if self.long_running {
            lines.extend(self.recent.iter().map(|line| format!("    {line}")));
        }
        lines
    }

    fn render(&mut self) {
        let block = self.block();
        let mut err = std::io::stderr().lock();
        rewind(&mut err, self.visible_lines);
        for line in &block {
            let _ = writeln!(err, "{line}");
        }
        self.visible_lines = block.len();
        let _ = err.flush();
    }
}

/// Move the cursor back to the top of a `n`-line block, clearing it on the way.
/// Avoid `writeln!`/`eprintln!` so stray `\n` won't produce visible scrollback.
fn rewind(err: &mut impl Write, n: usize) {
    if n == 0 {
        return;
    }
    let _ = write!(err, "\x1b[{n}F");
    for _ in 0..n {
        let _ = write!(err, "\x1b[2K\x1b[1B");
    }
    let _ = write!(err, "\x1b[{n}F");
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

/// Replace the spinner's recent-output tail (shown once long-running).
pub fn set_recent(lines: Vec<String>) {
    if !is_active() {
        return;
    }
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        let start = lines.len().saturating_sub(RECENT_LINES);
        state.recent = lines[start..].to_vec();
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
    }
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
            rewind(&mut err, state.visible_lines);
            state.visible_lines = 0;
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
        visible_lines: 0,
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

    fn finish(&mut self, colour: &str, symbol: char, message: String) {
        ACTIVE.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        let visible = STATE
            .lock()
            .unwrap()
            .take()
            .map(|state| state.visible_lines)
            .unwrap_or(0);
        let mut err = std::io::stderr().lock();
        rewind(&mut err, visible);
        let _ = writeln!(err, "[{colour}{symbol}{RESET}] {message}");
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
