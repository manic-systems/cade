use std::{
    io::{
        IsTerminal as _,
        Read,
        Write as _,
        stderr as io_stderr,
    },
    process::{
        Command,
        Output,
        Stdio,
    },
    sync::mpsc::{
        RecvTimeoutError,
        Sender,
        channel,
    },
    thread::{
        JoinHandle,
        spawn,
    },
    time::{
        Duration,
        Instant,
    },
};

use anyhow::{
    Context as _,
    Result,
    bail,
};

use crate::{
    config,
    nix::progress::NixProgress,
    progress::{
        LiveRenderer,
        is_active,
        mark_long_running,
        set_command_progress,
    },
    verbosity::{
        self,
        Verbosity,
    },
};

const DEFAULT_LONG_RUNNING_WARNING_AFTER: Duration = Duration::from_secs(5);
const LONG_RUNNING_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn long_running_warning_after() -> Duration {
    config::long_running_warning_ms()
        .map_or(DEFAULT_LONG_RUNNING_WARNING_AFTER, Duration::from_millis)
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

struct LongRunningProgress<'task> {
    what:        &'task str,
    enabled:     bool,
    interactive: bool,
    shown:       bool,
    renderer:    LiveRenderer,
    last_render: Option<Instant>,
}

impl<'task> LongRunningProgress<'task> {
    fn new(what: &'task str) -> Self {
        Self {
            what,
            enabled: verbosity::enabled(Verbosity::Normal),
            interactive: io_stderr().is_terminal(),
            shown: false,
            renderer: LiveRenderer::default(),
            last_render: None,
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

    const fn wants_live(&self) -> bool {
        self.shown && self.enabled && self.interactive
    }

    fn update(&mut self, recent: &[String], bar: Option<&str>) {
        if self.wants_live()
            && self
                .last_render
                .is_none_or(|last| last.elapsed() >= LONG_RUNNING_POLL_INTERVAL)
        {
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
        let mut err = io_stderr().lock();
        self.renderer.render(&mut err, &block);
        let _ = err.flush();
        self.last_render = Some(Instant::now());
    }

    fn clear(&mut self) {
        let mut err = io_stderr().lock();
        self.renderer.clear(&mut err);
        let _ = err.flush();
    }

    fn block(&self, recent: &[String], bar: Option<&str>) -> Vec<String> {
        let mut lines = vec![format!(
            "cade: {} is taking a long time; press Ctrl-C to stop and inspect the command.",
            self.what
        )];
        if !recent.is_empty() {
            lines.push("cade: recent output:".to_owned());
            lines.extend(recent.iter().map(|line| format!("    {line}")));
        }
        if let Some(bar_text) = bar {
            lines.push(bar_text.to_owned());
        }
        lines
    }
}

fn spawn_reader<Reader: Read + Send + 'static>(
    mut reader: Reader,
    kind: StreamKind,
    tx: Sender<StreamEvent>,
) -> JoinHandle<()> {
    spawn(move || {
        let mut buf = [0; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if tx
                        .send(StreamEvent {
                            kind,
                            data: buf[..count].to_vec(),
                        })
                        .is_err()
                    {
                        break;
                    }
                },
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
                Some(tracker) if tracker.wants_live() => tracker.update(&recent, bar.as_deref()),
                Some(_) => {},
                None => set_command_progress(recent, bar),
            }
        },
    }
}

pub fn run_checked_output(mut cmd: Command, what: &str) -> Result<Output> {
    verbosity::log(Verbosity::Trace, format_args!("cade: running {what}."));

    let (tx, rx) = channel();
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
    let mut progress = (!is_active()).then(|| LongRunningProgress::new(what));
    let mut warned = false;
    let start = Instant::now();
    let warn_after = long_running_warning_after();
    let status = loop {
        if let Some(status) = child.try_wait().context("checking command status")? {
            break status;
        }

        if !warned && start.elapsed() >= warn_after {
            warned = true;
            match progress.as_mut() {
                Some(tracker) => tracker.show(&nix.recent_lines(), nix.bar_line().as_deref()),
                None => {
                    mark_long_running(format!(
                        "cade: {what} is taking a long time; press Ctrl-C to stop and inspect the \
                         command."
                    ));
                },
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
                handle_stream_event(event, &mut stdout, &mut stderr, &mut nix, progress.as_mut());
            },
            Err(RecvTimeoutError::Timeout) => {
                if let Some(tracker) = progress.as_mut().filter(|candidate| candidate.wants_live())
                {
                    tracker.update(&nix.recent_lines(), nix.bar_line().as_deref());
                }
            },
            Err(RecvTimeoutError::Disconnected) => {
                break child.wait().context("waiting for command status")?;
            },
        }
    };

    for reader in readers {
        let _ = reader.join();
    }
    while let Ok(event) = rx.try_recv() {
        handle_stream_event(event, &mut stdout, &mut stderr, &mut nix, progress.as_mut());
    }
    nix.finish();
    if let Some(mut tracker) = progress {
        tracker.finish(&nix.recent_lines());
    }

    let out = Output {
        status,
        stdout,
        stderr,
    };
    verbosity::log(Verbosity::Trace, format_args!("cade: finished {what}."));

    if !out.status.success() {
        let summary = if nix.saw_nix() {
            nix.error_text()
        } else {
            String::from_utf8_lossy(&out.stderr).into_owned()
        };
        let trimmed = summary.trim();
        bail!(
            "{what} failed ({}){}",
            out.status,
            if trimmed.is_empty() {
                String::new()
            } else {
                format!(":\n{trimmed}")
            }
        );
    }
    Ok(out)
}

pub fn run_checked(cmd: Command, what: &str) -> Result<Vec<u8>> {
    Ok(run_checked_output(cmd, what)?.stdout)
}
