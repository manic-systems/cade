use crate::verbosity::{self, Verbosity};
use std::io::{IsTerminal as _, Write, stderr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{JoinHandle, park_timeout, spawn};
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
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";

static STATE: Mutex<Option<State>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);

struct State {
    message: String,
    frame: usize,
    long_running: bool,
    recent: Vec<String>,
    nix_bar: Option<String>,
    renderer: LiveRenderer,
}

impl State {
    fn block(&self) -> Vec<String> {
        let colour = if self.long_running { YELLOW } else { BLUE };
        let frame = FRAMES[self.frame % FRAMES.len()];
        let mut lines = vec![format!("[{colour}{frame}{RESET}] {}", self.message)];
        if self.long_running {
            lines.extend(self.recent.iter().map(|line| format!("    {line}")));
            if let Some(bar) = self.nix_bar.as_ref() {
                lines.push(bar.clone());
            }
        }
        lines
    }

    fn render(&mut self) {
        let block = self.block();
        let mut err = stderr().lock();
        self.renderer.render(&mut err, &block);
        let _ = err.flush();
    }
}

#[derive(Default)]
pub struct LiveRenderer {
    lines: Vec<String>,
}

impl LiveRenderer {
    pub(crate) fn render(&mut self, err: &mut impl Write, lines: &[String]) {
        let width = terminal_width().unwrap_or(80).max(1);
        self.render_at_width(err, lines, width);
    }

    pub(crate) fn clear(&mut self, err: &mut impl Write) {
        self.update(err, Vec::new());
    }

    fn render_at_width(&mut self, err: &mut impl Write, lines: &[String], width: usize) {
        let fitted: Vec<_> = lines
            .iter()
            .map(|line| fit_terminal_line(line, width))
            .collect();
        self.update(err, fitted);
    }

    fn update(&mut self, err: &mut impl Write, lines: Vec<String>) {
        if self.lines == lines {
            return;
        }
        let _ = write!(err, "{HIDE_CURSOR}");
        update_block(err, &self.lines, &lines);
        let _ = write!(err, "{SHOW_CURSOR}");
        self.lines = lines;
    }
}

fn update_block(err: &mut impl Write, previous: &[String], next: &[String]) {
    let extent = previous.len().max(next.len());
    let mut cursor_row = previous.len();
    for index in 0..extent {
        if previous.get(index) == next.get(index) {
            continue;
        }
        move_to_row(err, cursor_row, index);
        if let Some(line) = next.get(index) {
            let _ = write!(err, "{line}\x1b[K\r\n");
        } else {
            let _ = write!(err, "\x1b[2K\r\n");
        }
        cursor_row = index + 1;
    }
    move_to_row(err, cursor_row, next.len());
}

fn move_to_row(err: &mut impl Write, from: usize, to: usize) {
    if from > to {
        let _ = write!(err, "\x1b[{}F", from - to);
    } else if from < to {
        let _ = write!(err, "\x1b[{}E", to - from);
    }
}

fn fit_terminal_line(line: &str, width: usize) -> String {
    let max_columns = width.saturating_sub(1).max(1);
    if visible_columns(line) <= max_columns {
        return line.to_owned();
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
            for control in chars.by_ref() {
                if ('@'..='~').contains(&control) {
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
            for control in chars.by_ref() {
                out.push(control);
                if ('@'..='~').contains(&control) {
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
#[expect(
    clippy::undocumented_unsafe_blocks,
    reason = "TIOCGWINSZ writes only to the initialized winsize buffer passed by pointer"
)]
fn terminal_width() -> Option<usize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &raw mut size) };
    if result == 0 {
        (size.ws_col > 0).then_some(usize::from(size.ws_col))
    } else {
        None
    }
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

pub fn eviction_marker() -> String {
    if stderr().is_terminal() {
        format!("[{YELLOW}{EVICTED}{RESET}] ")
    } else {
        String::new()
    }
}

pub fn load_marker() -> String {
    if stderr().is_terminal() {
        format!("[{GREEN}{LOADED}{RESET}] ")
    } else {
        String::new()
    }
}

pub fn set_command_progress(mut lines: Vec<String>, nix_bar: Option<String>) {
    if !is_active() {
        return;
    }
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        let start = lines.len().saturating_sub(RECENT_LINES);
        lines.drain(..start);
        state.recent = lines;
        state.nix_bar = nix_bar;
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

    let mut lines = vec!["cade: recent output:".to_owned()];
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
            let mut err = stderr().lock();
            state.renderer.clear(&mut err);
            let _ = writeln!(err, "{line}");
            let _ = err.flush();
        }
        None => eprintln!("{line}"),
    }
}

pub fn start(subject: &str) -> Spinner {
    if !verbosity::enabled(Verbosity::Normal)
        || !stderr().is_terminal()
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
        renderer: LiveRenderer::default(),
    });

    let thread = spawn(run_loop);
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
        park_timeout(FRAME_INTERVAL);
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
            self.finish(GREEN, LOADED, message);
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
        let active_state = STATE.lock().unwrap().take();
        let mut err = stderr().lock();
        if let Some(mut state) = active_state {
            let recent = durable_recent_block(&state);
            state.renderer.clear(&mut err);
            for line in recent {
                let _ = writeln!(err, "{line}");
            }
        }
        let _ = err.flush();
    }

    fn finish(&mut self, colour: &str, symbol: char, message: &str) {
        ACTIVE.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        let active_state = STATE.lock().unwrap().take();
        let mut err = stderr().lock();
        let recent = if let Some(mut state) = active_state {
            let recent = durable_recent_block(&state);
            state.renderer.clear(&mut err);
            recent
        } else {
            Vec::new()
        };
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
            self.finish(RED, CROSS, "cade: environment failed to load.");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::progress::LiveRenderer;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn unchanged_block_emits_nothing() {
        let mut renderer = LiveRenderer::default();
        let mut output = Vec::new();
        let block = lines(&["spinner", "detail", "progress"]);

        renderer.render_at_width(&mut output, &block, 80);
        output.clear();
        renderer.render_at_width(&mut output, &block, 80);

        assert!(output.is_empty());
    }

    #[test]
    fn changing_first_row_does_not_repaint_unchanged_rows() {
        let mut renderer = LiveRenderer::default();
        let mut output = Vec::new();
        renderer.render_at_width(
            &mut output,
            &lines(&["frame one", "detail", "progress"]),
            80,
        );

        output.clear();
        renderer.render_at_width(
            &mut output,
            &lines(&["frame two", "detail", "progress"]),
            80,
        );

        assert_eq!(
            output,
            b"\x1b[?25l\x1b[3Fframe two\x1b[K\r\n\x1b[2E\x1b[?25h"
        );
    }

    #[test]
    fn changing_last_row_only_repaints_last_row() {
        let mut renderer = LiveRenderer::default();
        let mut output = Vec::new();
        renderer.render_at_width(&mut output, &lines(&["spinner", "detail", "10%"]), 80);

        output.clear();
        renderer.render_at_width(&mut output, &lines(&["spinner", "detail", "20%"]), 80);

        assert_eq!(output, b"\x1b[?25l\x1b[1F20%\x1b[K\r\n\x1b[?25h");
    }

    #[test]
    fn separated_changes_do_not_repaint_rows_between_them() {
        let mut renderer = LiveRenderer::default();
        let mut output = Vec::new();
        renderer.render_at_width(&mut output, &lines(&["frame one", "detail", "10%"]), 80);

        output.clear();
        renderer.render_at_width(&mut output, &lines(&["frame two", "detail", "20%"]), 80);

        assert_eq!(
            output,
            b"\x1b[?25l\x1b[3Fframe two\x1b[K\r\n\x1b[1E20%\x1b[K\r\n\x1b[?25h"
        );
    }

    #[test]
    fn clear_restores_the_cursor() {
        let mut renderer = LiveRenderer::default();
        let mut output = Vec::new();
        renderer.render_at_width(&mut output, &lines(&["spinner", "detail"]), 80);

        output.clear();
        renderer.clear(&mut output);

        assert_eq!(
            output,
            b"\x1b[?25l\x1b[2F\x1b[2K\r\n\x1b[2K\r\n\x1b[2F\x1b[?25h"
        );
    }
}
