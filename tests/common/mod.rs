use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_cade");

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub struct Sandbox {
    pub root: PathBuf,
    pub state: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("cade-it-{}-{id}", std::process::id()));
        let root = base.join("project");
        let state = base.join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        Sandbox { root, state }
    }

    pub fn write(&self, rel: &str, contents: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    pub fn dir(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    pub fn write_snapshot(&self, session: &str, contents: &str) {
        let dir = self.state.join("cade").join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{session}.env")), contents).unwrap();
    }

    pub fn run(&self, cwd: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(cwd)
            .env_clear()
            .env("XDG_STATE_HOME", &self.state)
            .env("HOME", &self.state);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().expect("run cade")
    }

    pub fn allow(&self, cwd: &Path) {
        let out = self.run(cwd, &["allow"], &[]);
        assert!(out.status.success(), "allow failed: {:?}", out);
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            std::fs::remove_dir_all(base).ok();
        }
    }
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
