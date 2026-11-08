use std::{
    env::current_exe,
    fs::{
        File,
        create_dir_all,
        read,
        read_to_string,
        remove_file,
    },
    io::{
        Write as _,
        stderr,
    },
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{
        Command,
        Stdio,
    },
    thread::sleep,
    time::Duration,
};

use misstep::{
    OptionExt as _,
    Result,
    ResultExt as _,
    bail,
};
use serde::{
    Deserialize,
    Serialize,
};

use crate::{
    config,
    core::{
        Cade,
        activation::{
            ActivationPlan,
            activation_plan,
            chain_key,
        },
        sessions::{
            atomic_write,
            gc_roots::root_nix_store_paths,
            identity::{
                process_holder_is_live,
                process_start_time,
            },
            is_valid_session,
        },
    },
    progress::start,
    verbosity,
};

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Serialize, Deserialize)]
struct JobResult {
    key:     String,
    outcome: JobOutcome,
}

#[derive(Serialize, Deserialize)]
enum JobOutcome {
    Ready(ActivationPlan),
    Failed(String),
}

pub(super) enum JobPoll {
    Ready(ActivationPlan),
    Failed(String),
    Running,
    Spawned,
}

struct JobFiles {
    key:    PathBuf,
    pid:    PathBuf,
    log:    PathBuf,
    result: PathBuf,
}

impl JobFiles {
    fn new(cade: &Cade, session: &str) -> Self {
        let dir = jobs_dir(cade);
        Self {
            key:    dir.join(format!("{session}.key")),
            pid:    dir.join(format!("{session}.pid")),
            log:    dir.join(format!("{session}.log")),
            result: dir.join(format!("{session}.json")),
        }
    }

    fn clear(&self) {
        for path in [&self.key, &self.pid, &self.log, &self.result] {
            let _ = remove_file(path);
        }
    }

    fn live_pid(&self) -> Option<i32> {
        let raw = read_to_string(&self.pid).ok()?;
        let (pid_text, start_time) = raw.split_once(' ')?;
        let pid = pid_text.parse::<u32>().ok()?;
        process_holder_is_live(pid, start_time)
            .then(|| i32::try_from(pid).ok())
            .flatten()
    }

    fn kill(&self) {
        let Some(pid) = self.live_pid() else {
            return;
        };
        // SAFETY: kill only sends a signal. The negative pid targets the job's
        // own process group, which `process_group(0)` made it lead, so nix
        // children die with it.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }

    fn take_result(&self, key: &str) -> Option<JobPoll> {
        let result: JobResult = serde_json::from_slice(&read(&self.result).ok()?).ok()?;
        if result.key != key {
            let _ = remove_file(&self.result);
            return None;
        }

        self.replay_log();
        self.clear();
        Some(match result.outcome {
            JobOutcome::Ready(plan) => JobPoll::Ready(plan),
            JobOutcome::Failed(message) => JobPoll::Failed(message),
        })
    }

    fn replay_log(&self) {
        if let Ok(log) = read(&self.log) {
            let _ = stderr().lock().write_all(&log);
        }
    }
}

pub(super) fn jobs_dir(cade: &Cade) -> PathBuf {
    cade.state_dir.join("jobs")
}

pub(super) fn poll_or_spawn(cade: &Cade, session: &str, key: &str) -> Result<JobPoll> {
    let files = JobFiles::new(cade, session);
    if let Some(done) = files.take_result(key) {
        return Ok(done);
    }

    let job_key = read_to_string(&files.key).ok();
    if files.live_pid().is_some() {
        if job_key.as_deref() == Some(key) {
            return Ok(JobPoll::Running);
        }
        files.kill();
    } else if job_key.as_deref() == Some(key) {
        if let Some(done) = files.take_result(key) {
            return Ok(done);
        }
        files.replay_log();
        files.clear();
        return Ok(JobPoll::Failed(
            "background reload exited without a result".to_owned(),
        ));
    }

    spawn(cade, session, key, &files)?;
    Ok(JobPoll::Spawned)
}

pub(super) fn wait_for_job(
    cade: &Cade,
    session: &str,
    key: &str,
    subject: &str,
) -> Result<JobPoll> {
    let mut poll = poll_or_spawn(cade, session, key)?;
    if !matches!(poll, JobPoll::Running | JobPoll::Spawned) {
        return Ok(poll);
    }

    let spinner = start(subject);
    while matches!(poll, JobPoll::Running | JobPoll::Spawned) {
        sleep(WAIT_POLL_INTERVAL);
        poll = poll_or_spawn(cade, session, key)?;
    }
    spinner.done();
    Ok(poll)
}

pub(super) fn cancel(cade: &Cade, session: &str) {
    let files = JobFiles::new(cade, session);
    files.kill();
    files.clear();
}

fn spawn(cade: &Cade, session: &str, key: &str, files: &JobFiles) -> Result<()> {
    create_dir_all(jobs_dir(cade)).context("create background reload dir")?;
    files.clear();
    atomic_write(&files.key, key.as_bytes()).context("write background reload key")?;
    let log = File::create(&files.log).context("create background reload log")?;

    let mut command = Command::new(current_exe().context("resolve cade executable")?);
    if let Some(path) = config::current().path.as_ref() {
        command.arg("--config").arg(path);
    }
    command
        .arg(format!("--verbosity={}", verbosity::current()))
        .args(["reload-job", "--session", session])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .process_group(0);

    let child = command.spawn().context("spawn background reload")?;
    let pid = child.id();
    let start_time = process_start_time(pid).unwrap_or_default();
    atomic_write(&files.pid, format!("{pid} {start_time}").as_bytes())
        .context("write background reload pid")
        .erase()
}

pub fn run_reload_job(cade: &Cade, session: &str) -> Result<()> {
    if !is_valid_session(session) {
        bail!("invalid cade session id");
    }

    let key = chain_key(cade)?
        .context("no approved .cade layers here")?
        .key;
    let outcome = match activation_plan(cade, session) {
        Ok(plan) => {
            root_nix_store_paths(cade, session, &plan.nix_store_paths);
            JobOutcome::Ready(plan)
        },
        Err(error) => JobOutcome::Failed(format!("{error}")),
    };
    let body = serde_json::to_vec(&JobResult { key, outcome }).context("serialize reload job")?;
    atomic_write(&JobFiles::new(cade, session).result, &body)
        .context("write reload job result")
        .erase()
}
