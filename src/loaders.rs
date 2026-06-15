use crate::{
    config,
    env::EnvSet,
    nix::NixProgress,
    verbosity::{self, Verbosity},
};
use anyhow::{Context, Result, bail};
use std::{
    io::{IsTerminal, Read, Write},
    path::Path,
    process::{Command, Output, Stdio},
    sync::mpsc::RecvTimeoutError,
    time::{Duration, Instant},
};

pub use crate::nix::{load_flake, load_shell};

const DEFAULT_LONG_RUNNING_WARNING_AFTER: Duration = Duration::from_secs(5);
const LONG_RUNNING_POLL_INTERVAL: Duration = Duration::from_millis(100);

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

struct LongRunningProgress<'a> {
    what: &'a str,
    enabled: bool,
    interactive: bool,
    shown: bool,
    visible_rows: usize,
    last_block: Vec<String>,
}

impl<'a> LongRunningProgress<'a> {
    fn new(what: &'a str) -> Self {
        Self {
            what,
            enabled: verbosity::enabled(Verbosity::Normal),
            interactive: std::io::stderr().is_terminal(),
            shown: false,
            visible_rows: 0,
            last_block: Vec::new(),
        }
    }

    fn show(&mut self, recent: &[String], bar: Option<&str>) {
        self.shown = true;
        if !self.enabled {
            return;
        }
        if self.interactive {
            self.render(recent, bar);
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

    fn update(&mut self, recent: &[String], bar: Option<&str>) {
        if self.wants_live() {
            self.render(recent, bar);
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

    fn render(&mut self, recent: &[String], bar: Option<&str>) {
        let block = self.block(recent, bar);
        if block == self.last_block {
            return;
        }
        self.clear();
        let mut err = std::io::stderr().lock();
        self.visible_rows = crate::progress::render_block(&mut err, &block);
        self.last_block = block;
        let _ = err.flush();
    }

    fn clear(&mut self) {
        if self.visible_rows == 0 {
            return;
        }
        let mut err = std::io::stderr().lock();
        crate::progress::rewind(&mut err, self.visible_rows);
        let _ = err.flush();
        self.visible_rows = 0;
        self.last_block.clear();
    }

    fn block(&self, recent: &[String], bar: Option<&str>) -> Vec<String> {
        let mut lines = vec![format!(
            "cade: {} is taking a long time; press Ctrl-C to stop and inspect the command.",
            self.what
        )];
        if !recent.is_empty() {
            lines.push("cade: recent output:".to_string());
            lines.extend(recent.iter().map(|line| format!("    {line}")));
        }
        if let Some(bar) = bar {
            lines.push(bar.to_string());
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
    nix: &mut NixProgress,
    progress: Option<&mut LongRunningProgress<'_>>,
) {
    match event.kind {
        StreamKind::Stdout => stdout.extend(event.data),
        StreamKind::Stderr => {
            stderr.extend(&event.data);
            nix.push(&event.data);
            let recent = nix.recent_lines();
            let bar = nix.bar_line();
            match progress {
                // No spinner owns the terminal: drive the standalone widget.
                Some(progress) if progress.wants_live() => progress.update(&recent, bar.as_deref()),
                Some(_) => {}
                // Feed the active activation spinner instead.
                None => {
                    crate::progress::set_recent(recent);
                    crate::progress::set_nix_bar(bar);
                }
            }
        }
    }
}

/// Run a command, returning stdout on success or an error carrying its stderr
pub fn run_checked(mut cmd: Command, what: &str) -> Result<Vec<u8>> {
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
    let mut nix = NixProgress::new();
    // The activation spinner, when present, subsumes the standalone widget.
    let mut progress = (!crate::progress::is_active()).then(|| LongRunningProgress::new(what));
    let mut warned = false;
    let start = Instant::now();
    let warn_after = long_running_warning_after();
    let status = loop {
        if let Some(status) = child.try_wait().context("checking command status")? {
            break status;
        }

        if !warned && start.elapsed() >= warn_after {
            warned = true;
            match &mut progress {
                Some(progress) => progress.show(&nix.recent_lines(), nix.bar_line().as_deref()),
                None => {
                    crate::progress::mark_long_running(format!(
                        "cade: {what} is taking a long time; press Ctrl-C to stop and inspect the command."
                    ));
                    crate::progress::set_nix_bar(nix.bar_line());
                }
            }
        }

        let wait_for = if warned {
            LONG_RUNNING_POLL_INTERVAL
        } else {
            warn_after
                .saturating_sub(start.elapsed())
                .min(LONG_RUNNING_POLL_INTERVAL)
        };

        match rx.recv_timeout(wait_for) {
            Ok(event) => {
                handle_stream_event(event, &mut stdout, &mut stderr, &mut nix, progress.as_mut())
            }
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
        handle_stream_event(event, &mut stdout, &mut stderr, &mut nix, progress.as_mut());
    }
    if let Some(mut progress) = progress {
        progress.finish(&nix.recent_lines());
    }

    let out = Output {
        status,
        stdout,
        stderr,
    };
    verbosity::log(Verbosity::Trace, format_args!("cade: finished {what}."));

    if !out.status.success() {
        // prefer the de-jsonified nix messages; fall back to raw stderr for
        // non-nix commands (whose stderr is plain text already).
        let summary = if nix.saw_nix() {
            nix.error_text()
        } else {
            String::from_utf8_lossy(&out.stderr).into_owned()
        };
        let summary = summary.trim();
        bail!(
            "{what} failed ({}){}",
            out.status,
            if summary.is_empty() {
                String::new()
            } else {
                format!(":\n{summary}")
            }
        );
    }
    Ok(out.stdout)
}

pub fn load_env(path: &Path) -> Result<EnvSet> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening env file at {}", path.display()))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).context("reading env file")?;
    EnvSet::from_envs(&buf)
}

pub fn call(path: &Path, argv: Vec<String>) -> Result<EnvSet> {
    let mut it = argv.iter();
    // expansion can empty the argv (e.g. `call ${UNSET}`)
    let program = it.next().context("call has no command")?;
    let mut process = Command::new(program);
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
