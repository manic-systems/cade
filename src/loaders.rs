use crate::{
    config,
    types::EnvSet,
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, bail};
use std::{
    collections::VecDeque,
    io::{IsTerminal, Read, Write},
    path::Path,
    process::{Command, Output, Stdio},
    sync::mpsc::RecvTimeoutError,
    time::{Duration, Instant},
};

const DEFAULT_LONG_RUNNING_WARNING_AFTER: Duration = Duration::from_secs(5);
const LONG_RUNNING_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECENT_OUTPUT_LINES: usize = 3;
const RECENT_OUTPUT_LINE_BYTES: usize = 4 * 1024;
const DISPLAY_LINE_CHARS: usize = 200;

fn long_running_warning_after() -> Duration {
    config::long_running_warning_ms()
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_LONG_RUNNING_WARNING_AFTER)
}

#[derive(Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

struct StreamEvent {
    kind: StreamKind,
    data: Vec<u8>,
}

struct RecentLines {
    lines: VecDeque<String>,
    current: Vec<u8>,
}

impl RecentLines {
    fn new() -> Self {
        Self {
            lines: VecDeque::with_capacity(RECENT_OUTPUT_LINES),
            current: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            match byte {
                b'\n' => self.finish_current_line(),
                b'\r' => {
                    self.current.clear();
                }
                _ => {
                    if self.current.len() < RECENT_OUTPUT_LINE_BYTES {
                        self.current.push(byte);
                    }
                }
            }
        }
    }

    fn finish_current_line(&mut self) {
        let line = sanitize_display_line(&self.current);
        self.current.clear();
        if line.is_empty() {
            return;
        }
        if self.lines.len() == RECENT_OUTPUT_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.lines.iter().cloned().collect();
        let current = sanitize_display_line(&self.current);
        if !current.is_empty() {
            lines.push(current);
        }
        let keep_from = lines.len().saturating_sub(RECENT_OUTPUT_LINES);
        lines.into_iter().skip(keep_from).collect()
    }
}

fn sanitize_display_line(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::new();
    for ch in text.chars() {
        if ch == '\t' {
            out.push_str("    ");
        } else if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
        if out.chars().count() >= DISPLAY_LINE_CHARS {
            out.push_str("...");
            break;
        }
    }
    out.trim().to_string()
}

struct LongRunningProgress<'a> {
    what: &'a str,
    enabled: bool,
    interactive: bool,
    shown: bool,
    visible_lines: usize,
    last_block: Vec<String>,
}

impl<'a> LongRunningProgress<'a> {
    fn new(what: &'a str) -> Self {
        Self {
            what,
            enabled: verbosity::enabled(Verbosity::Normal),
            interactive: std::io::stderr().is_terminal(),
            shown: false,
            visible_lines: 0,
            last_block: Vec::new(),
        }
    }

    fn show(&mut self, recent: &[String]) {
        self.shown = true;
        if !self.enabled {
            return;
        }
        if self.interactive {
            self.render(recent);
        } else {
            eprintln!(
                "cade: {} is taking a long time; press Ctrl-C to stop and inspect the command.",
                self.what
            );
        }
    }

    /// Whether a live render would actually draw anything.
    fn wants_live(&self) -> bool {
        self.shown && self.enabled && self.interactive
    }

    fn update(&mut self, recent: &[String]) {
        if self.wants_live() {
            self.render(recent);
        }
    }

    fn finish(&mut self, recent: &[String]) {
        if !self.shown || !self.enabled {
            return;
        }
        if self.interactive {
            self.clear();
        } else if !recent.is_empty() {
            eprintln!("cade: recent output from {}:", self.what);
            for line in recent {
                eprintln!("    {line}");
            }
        }
    }

    fn render(&mut self, recent: &[String]) {
        let block = self.block(recent);
        if block == self.last_block {
            return;
        }
        self.clear();
        for line in &block {
            eprintln!("{line}");
        }
        self.visible_lines = block.len();
        self.last_block = block;
        let _ = std::io::stderr().flush();
    }

    fn clear(&mut self) {
        if self.visible_lines == 0 {
            return;
        }
        eprint!("\x1b[{}F", self.visible_lines);
        for _ in 0..self.visible_lines {
            eprintln!("\x1b[2K");
        }
        eprint!("\x1b[{}F", self.visible_lines);
        let _ = std::io::stderr().flush();
        self.visible_lines = 0;
        self.last_block.clear();
    }

    fn block(&self, recent: &[String]) -> Vec<String> {
        let mut lines = vec![format!(
            "cade: {} is taking a long time; press Ctrl-C to stop and inspect the command.",
            self.what
        )];
        if !recent.is_empty() {
            lines.push("cade: recent output:".to_string());
            lines.extend(recent.iter().map(|line| format!("    {line}")));
        }
        lines
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    kind: StreamKind,
    tx: std::sync::mpsc::Sender<StreamEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx
                        .send(StreamEvent {
                            kind,
                            data: buf[..n].to_vec(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn handle_stream_event(
    event: StreamEvent,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    recent_stderr: &mut RecentLines,
    progress: &mut LongRunningProgress<'_>,
) {
    match event.kind {
        StreamKind::Stdout => stdout.extend(event.data),
        StreamKind::Stderr => {
            stderr.extend(&event.data);
            recent_stderr.push(&event.data);
            if progress.wants_live() {
                progress.update(&recent_stderr.lines());
            }
        }
    }
}

/// Run a command, returning stdout on success or an error carrying its stderr
fn run_checked(mut cmd: Command, what: &str) -> Result<Vec<u8>> {
    verbosity::log(Verbosity::Trace, format_args!("cade: running {what}."));

    let (tx, rx) = std::sync::mpsc::channel();
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {what}"))?;
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader(stdout, StreamKind::Stdout, tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader(stderr, StreamKind::Stderr, tx.clone()));
    }
    drop(tx);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut recent_stderr = RecentLines::new();
    let mut progress = LongRunningProgress::new(what);
    let start = Instant::now();
    let warn_after = long_running_warning_after();
    let status = loop {
        if let Some(status) = child.try_wait().context("checking command status")? {
            break status;
        }

        if !progress.shown && start.elapsed() >= warn_after {
            progress.show(&recent_stderr.lines());
        }

        let wait_for = if progress.shown {
            LONG_RUNNING_POLL_INTERVAL
        } else {
            warn_after
                .saturating_sub(start.elapsed())
                .min(LONG_RUNNING_POLL_INTERVAL)
        };

        match rx.recv_timeout(wait_for) {
            Ok(event) => handle_stream_event(
                event,
                &mut stdout,
                &mut stderr,
                &mut recent_stderr,
                &mut progress,
            ),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break child.wait().context("waiting for command status")?;
            }
        }
    };

    for reader in readers {
        let _ = reader.join();
    }
    while let Ok(event) = rx.try_recv() {
        handle_stream_event(
            event,
            &mut stdout,
            &mut stderr,
            &mut recent_stderr,
            &mut progress,
        );
    }
    progress.finish(&recent_stderr.lines());

    let out = Output {
        status,
        stdout,
        stderr,
    };
    verbosity::log(Verbosity::Trace, format_args!("cade: finished {what}."));

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        bail!(
            "{what} failed ({}){}",
            out.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(":\n{stderr}")
            }
        );
    }
    Ok(out.stdout)
}

pub fn load_flake(path: &Path, output: Option<String>) -> Result<EnvSet> {
    let mut proc = Command::new("nix");
    proc.args(["print-dev-env", "--json"]);
    // A named output is a flake installable
    if let Some(flake_output) = output.filter(|o| !o.is_empty()) {
        proc.arg(format!(".#{flake_output}"));
    }
    proc.current_dir(path);
    let stdout = run_checked(proc, &format!("nix print-dev-env at {}", path.display()))?;
    EnvSet::from_json(&stdout)
}

pub fn load_shell(path: &Path, filename: String) -> Result<EnvSet> {
    let file = if filename.is_empty() {
        "./shell.nix".to_string()
    } else {
        filename
    };
    let mut proc = Command::new("nix");
    proc.args(["print-dev-env", "--json", "-f", &file]);
    proc.current_dir(path);
    let stdout = run_checked(
        proc,
        &format!("nix print-dev-env -f {file} at {}", path.display()),
    )?;
    EnvSet::from_json(&stdout)
}

pub fn load_env(path: &Path, filename: String) -> Result<EnvSet> {
    let mut p = path.to_path_buf();
    if filename.is_empty() {
        p.push(".env");
    } else {
        p.push(filename);
    }
    let mut file = std::fs::File::open(p)
        .with_context(|| format!("opening env file at {}", path.display()))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).context("reading env file")?;
    EnvSet::from_envs(&buf)
}

pub fn call(path: &Path, argv: Vec<String>) -> Result<EnvSet> {
    let mut it = argv.iter();
    // safety: parser rejects an empty argv
    let mut process = Command::new(it.next().unwrap());
    process.current_dir(path);
    process.args(it);
    let cmdline = argv.join(" ");
    let stdout = run_checked(process, &format!("call `{cmdline}`"))?;

    let text = String::from_utf8(stdout)
        .with_context(|| format!("call `{cmdline}` output must be valid UTF-8"))?;
    EnvSet::from_envs(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_output_must_be_utf8() {
        let dir = std::env::temp_dir();
        let err = call(
            &dir,
            vec!["sh".into(), "-c".into(), "printf 'BAD=\\377\\n'".into()],
        )
        .expect_err("invalid UTF-8 call output must fail");
        assert!(
            format!("{err:#}").contains("must be valid UTF-8"),
            "{err:#}"
        );
    }
}
