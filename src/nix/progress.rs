use cognos::internal::json::{Actions, Activities, ResultType, Verbosity, parse_line};
use std::collections::{HashMap, VecDeque};

const RECENT_LINES: usize = 5;
const LINE_BYTES: usize = 4 * 1024;
const DISPLAY_CHARS: usize = 200;
const TRANSCRIPT_CAP: usize = 200;
const BAR_CELLS: usize = 24;

const BAR: &str = "\x1b[34m";
const RESET: &str = "\x1b[0m";

#[derive(Default, Clone, Copy)]
struct Count {
    done: u64,
    expected: u64,
}

#[derive(Default)]
pub struct NixProgress {
    carry: Vec<u8>,
    recent: VecDeque<String>,
    transcript: VecDeque<String>,
    saw_nix: bool,

    builds: Count,
    copies: Count,
    builds_id: Option<u64>,
    copies_id: Option<u64>,
    transfers: HashMap<u64, (u64, u64)>,
}

impl NixProgress {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            match byte {
                b'\n' => {
                    let line = std::mem::take(&mut self.carry);
                    self.line(&line);
                }
                b'\r' => self.carry.clear(),
                _ => {
                    if self.carry.len() < LINE_BYTES {
                        self.carry.push(byte);
                    }
                }
            }
        }
    }

    fn line(&mut self, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        if let Some(action) = parse_line(&text) {
            self.saw_nix = true;
            self.observe(action);
        } else {
            let line = sanitize(raw);
            if !line.is_empty() {
                self.push_recent(line);
            }
        }
    }

    fn observe(&mut self, action: Actions) {
        match action {
            Actions::Start {
                id,
                level,
                text,
                activity,
                ..
            } => {
                match activity {
                    Activities::Builds => self.builds_id = Some(id),
                    Activities::CopyPaths => self.copies_id = Some(id),
                    Activities::FileTransfer => {
                        self.transfers.entry(id).or_insert((0, 0));
                    }
                    _ => {}
                }
                let lively = matches!(
                    activity,
                    Activities::Build
                        | Activities::Substitute
                        | Activities::CopyPath
                        | Activities::FileTransfer
                );
                if lively && level <= Verbosity::Talkative && !text.is_empty() {
                    self.push_recent(sanitize(text.as_bytes()));
                }
            }
            Actions::Result {
                id,
                result_type,
                fields,
            } => match result_type {
                ResultType::Progress => {
                    let done = fields.first().and_then(|v| v.as_u64()).unwrap_or(0);
                    let expected = fields.get(1).and_then(|v| v.as_u64()).unwrap_or(0);
                    let count = Count { done, expected };
                    if self.builds_id == Some(id) {
                        self.builds = count;
                    } else if self.copies_id == Some(id) {
                        self.copies = count;
                    } else if let Some(bytes) = self.transfers.get_mut(&id) {
                        *bytes = (done, expected);
                    }
                }
                ResultType::BuildLogLine | ResultType::PostBuildLogLine => {
                    if let Some(text) = fields.first().and_then(|v| v.as_str()) {
                        let line = sanitize(text.as_bytes());
                        if !line.is_empty() {
                            self.push_recent(line.clone());
                            self.push_transcript(line);
                        }
                    }
                }
                _ => {}
            },
            Actions::Message { level, msg, .. } => {
                let line = sanitize(msg.as_bytes());
                if line.is_empty() {
                    return;
                }
                self.push_transcript(line.clone());
                if level <= Verbosity::Notice {
                    self.push_recent(line);
                }
            }
            Actions::Stop { .. } => {}
        }
    }

    fn push_recent(&mut self, line: String) {
        if self.recent.len() == RECENT_LINES {
            self.recent.pop_front();
        }
        self.recent.push_back(line);
    }

    fn push_transcript(&mut self, line: String) {
        if self.transcript.len() == TRANSCRIPT_CAP {
            self.transcript.pop_front();
        }
        self.transcript.push_back(line);
    }

    pub fn recent_lines(&self) -> Vec<String> {
        self.recent.iter().cloned().collect()
    }

    pub fn saw_nix(&self) -> bool {
        self.saw_nix
    }

    pub fn error_text(&self) -> String {
        self.transcript
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn fraction(&self) -> Option<f32> {
        let done = self.builds.done + self.copies.done;
        let expected = self.builds.expected + self.copies.expected;
        (expected > 0).then(|| (done as f32 / expected as f32).clamp(0.0, 1.0))
    }

    fn status_text(&self) -> String {
        let mut parts = Vec::new();
        if self.builds.expected > 0 {
            parts.push(format!(
                "{}/{} built",
                self.builds.done, self.builds.expected
            ));
        }
        if self.copies.expected > 0 {
            parts.push(format!(
                "{}/{} copied",
                self.copies.done, self.copies.expected
            ));
        }
        let (done, expected) = self
            .transfers
            .values()
            .fold((0u64, 0u64), |(d, e), (td, te)| (d + td, e + te));
        if expected > 0 {
            parts.push(format!("{:.1}/{:.0} MB", mb(done), mb(expected)));
        }
        parts.join(" · ")
    }

    pub fn bar_line(&self) -> Option<String> {
        let fraction = self.fraction()?;
        Some(render_bar(fraction, &self.status_text()))
    }
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_000_000.0
}

fn render_bar(progress: f32, status: &str) -> String {
    let progress = progress.clamp(0.0, 1.0);
    let filled = progress * BAR_CELLS as f32;
    let full = filled.floor() as usize;
    let half = (filled - full as f32) >= 0.5 && full < BAR_CELLS;

    let mut bar = String::from("[");
    bar.push_str(BAR);
    for _ in 0..full {
        bar.push('━');
    }
    let mut used = full;
    if half {
        bar.push('╸');
        used += 1;
    }
    for _ in used..BAR_CELLS {
        bar.push('─');
    }
    bar.push_str(RESET);
    bar.push(']');

    let status = if status.is_empty() {
        String::new()
    } else {
        format!(" {status}")
    };
    format!("{bar} {:>3.0}%{status}", progress * 100.0)
}

fn sanitize(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if ch == '\t' {
            out.push_str("    ");
        } else if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
        if out.chars().count() >= DISPLAY_CHARS {
            out.push_str("...");
            break;
        }
    }
    out.trim().to_string()
}
