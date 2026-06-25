use crate::verbosity::{self, Verbosity};
use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

const FRAMES: [char; 4] = ['/', '-', '\\', '|'];
const LOADED: char = '\u{2192}';
const EVICTED: char = '\u{2190}';
const CROSS: char = '\u{2717}';
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const RECENT_LINES: usize = 5;

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

pub fn rewind(err: &mut impl Write, n: usize) {
    if n == 0 {
        return;
    }
    let _ = write!(err, "\x1b[{n}F");
    for _ in 0..n {
        let _ = writeln!(err, "\x1b[2K");
    }
    let _ = write!(err, "\x1b[{n}F");
}

pub fn render_block(err: &mut impl Write, lines: &[String]) -> usize {
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

    let result = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) };
    if result == 0 {
        let size = unsafe { size.assume_init() };
        (size.ws_col > 0).then_some(size.ws_col as usize)
    } else {
        None
    }
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

pub fn eviction_marker() -> String {
    if std::io::stderr().is_terminal() {
        format!("[{YELLOW}{EVICTED}{RESET}] ")
    } else {
        String::new()
    }
}

pub fn load_marker() -> String {
    if std::io::stderr().is_terminal() {
        format!("[{GREEN}{LOADED}{RESET}] ")
    } else {
        String::new()
    }
}

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

pub struct Spinner {
    active: bool,
    resolved: bool,
    thread: Option<JoinHandle<()>>,
}

impl Spinner {
    pub fn success(mut self, message: &str) {
        self.resolved = true;
        if self.active {
            self.finish(GREEN, LOADED, message.to_string());
        } else {
            verbosity::log(Verbosity::Normal, format_args!("{message}"));
        }
    }

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
