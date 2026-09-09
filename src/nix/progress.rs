use cognos::internal::json::{Actions, Activities, ResultType, Verbosity, parse_line};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::mem::take;

const RECENT_LINES: usize = 5;
const LINE_BYTES: usize = 4 * 1024;
const DISPLAY_CHARS: usize = 200;
const TRANSCRIPT_CAP: usize = 200;
const BAR_CELLS: u128 = 24;

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
                    let line = take(&mut self.carry);
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

    pub fn finish(&mut self) {
        let line = take(&mut self.carry);
        self.line(&line);
    }

    fn line(&mut self, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        if let Some(action) = parse_line(&text) {
            self.saw_nix = true;
            self.observe(action);
        } else {
            let line = sanitize(raw);
            if !line.is_empty() {
                self.push_recent(line.clone());
                self.push_transcript(line);
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
                    Activities::Unknown
                    | Activities::CopyPath
                    | Activities::Realise
                    | Activities::Build
                    | Activities::OptimiseStore
                    | Activities::VerifyPath
                    | Activities::Substitute
                    | Activities::QueryPathInfo
                    | Activities::PostBuildHook
                    | Activities::BuildWaiting
                    | Activities::FetchTree => {}
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
                    let done = fields.first().and_then(Value::as_u64).unwrap_or(0);
                    let expected = fields.get(1).and_then(Value::as_u64).unwrap_or(0);
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
                    if let Some(text) = fields.first().and_then(Value::as_str) {
                        let line = sanitize(text.as_bytes());
                        if !line.is_empty() {
                            self.push_recent(line.clone());
                            self.push_transcript(line);
                        }
                    }
                }
                ResultType::FileLinked
                | ResultType::UntrustedPath
                | ResultType::CorruptedPath
                | ResultType::SetPhase
                | ResultType::SetExpected
                | ResultType::FetchStatus => {}
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

    pub const fn saw_nix(&self) -> bool {
        self.saw_nix
    }

    pub fn error_text(&self) -> String {
        self.transcript
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
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
        let (done, expected) = self.transfers.values().fold(
            (0_u128, 0_u128),
            |(done_total, expected_total), &(done_bytes, expected_bytes)| {
                (
                    done_total + u128::from(done_bytes),
                    expected_total + u128::from(expected_bytes),
                )
            },
        );
        if expected > 0 {
            let tenths = rounded_div(done, 100_000);
            let expected_mb = rounded_div(expected, 1_000_000);
            parts.push(format!("{}.{}/{expected_mb} MB", tenths / 10, tenths % 10));
        }
        parts.join(" \u{b7} ")
    }

    pub fn bar_line(&self) -> Option<String> {
        let expected = u128::from(self.builds.expected) + u128::from(self.copies.expected);
        if expected == 0 {
            return None;
        }
        let done = u128::from(self.builds.done) + u128::from(self.copies.done);
        Some(render_bar(
            done.min(expected),
            expected,
            &self.status_text(),
        ))
    }
}

const fn rounded_div(value: u128, divisor: u128) -> u128 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    quotient
        + ((remainder > divisor / 2 || (remainder * 2 == divisor && quotient % 2 == 1)) as u128)
}

fn render_bar(done: u128, expected: u128, status: &str) -> String {
    let half_cells = done * BAR_CELLS * 2 / expected;
    let full = half_cells / 2;
    let half = half_cells % 2 == 1;

    let mut bar = String::from("[");
    bar.push_str(BAR);
    for _ in 0..full {
        bar.push('\u{2501}');
    }
    let mut used = full;
    if half {
        bar.push('\u{2578}');
        used += 1;
    }
    for _ in used..BAR_CELLS {
        bar.push('\u{2500}');
    }
    bar.push_str(RESET);
    bar.push(']');

    let suffix = if status.is_empty() {
        String::new()
    } else {
        format!(" {status}")
    };
    let percent = rounded_div(done * 100, expected);
    format!("{bar} {percent:>3}%{suffix}")
}

fn sanitize(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for control in chars.by_ref() {
                    if ('@'..='~').contains(&control) {
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
    out.trim().to_owned()
}
