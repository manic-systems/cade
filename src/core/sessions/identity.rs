#[cfg(target_os = "linux")] use std::fs::read_to_string;
#[cfg(target_os = "macos")]
use std::mem::{
    MaybeUninit,
    size_of,
};
use std::{
    env::var,
    fmt::Write as _,
    fs::{
        File,
        remove_file,
        rename,
        write,
    },
    io::{
        Read as _,
        Result as IoResult,
    },
    path::Path,
    process::id as process_id,
    time::{
        SystemTime,
        UNIX_EPOCH,
    },
};

use anyhow::{
    Result,
    bail,
};
#[cfg(target_os = "macos")]
use libc::{
    PROC_PIDTBSDINFO,
    c_int,
    c_void,
    pid_t,
    proc_bsdinfo,
    proc_pidinfo,
};

pub fn atomic_write(path: &Path, body: &[u8]) -> IoResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cade");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let tmp = parent.join(format!(".{stem}.tmp.{}.{nanos}", process_id()));
    if let Err(error) = write(&tmp, body).and_then(|()| rename(&tmp, path)) {
        let _ = remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub fn is_valid_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) fn is_valid_client_id(client_id: &str) -> bool {
    !client_id.is_empty()
        && client_id.len() <= 128
        && client_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) fn validate_client_id(client_id: &str) -> Result<()> {
    if is_valid_client_id(client_id) {
        Ok(())
    } else {
        bail!("invalid cade lease client id")
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0_u8; bytes];
    if File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut buf))
        .is_err()
    {
        let seed = format!("{}-{}-{}", process_id(), now_secs(), new_session_id());
        for (index, byte) in seed.as_bytes().iter().enumerate() {
            buf[index % bytes] ^= *byte;
            buf[(index * 7 + 3) % bytes] = buf[(index * 7 + 3) % bytes].wrapping_add(*byte);
        }
    }
    let mut output = String::with_capacity(bytes * 2);
    for byte in buf {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub(super) fn new_client_id() -> String {
    random_hex(16)
}

pub fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{}-{nanos}", process_id())
}

pub fn stable_hash_hex(text: &str) -> String {
    let mut hash = 0xCBF2_9CE4_8422_2325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01B3);
    }
    format!("{hash:016x}")
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(raw: &str) -> Option<(u32, String)> {
    let end = raw.rfind(") ")?;
    let tail = raw.get(end.checked_add(2)?..)?;
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let ppid = fields.get(1)?.parse::<u32>().ok()?;
    let start_time = fields.get(19)?.to_string();
    Some((ppid, start_time))
}

#[cfg(target_os = "linux")]
pub(super) fn process_start_time(pid: u32) -> Option<String> {
    let raw = read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_proc_stat(&raw).map(|(_, start)| start)
}

#[cfg(target_os = "macos")]
#[expect(
    clippy::undocumented_unsafe_blocks,
    reason = "PROC_PIDTBSDINFO buffer size plus success count check ensures proc_bsdinfo \
              initialized before reading"
)]
pub(super) fn process_start_time(pid: u32) -> Option<String> {
    let posix_pid = pid_t::try_from(pid).ok()?;
    let mut info = MaybeUninit::<proc_bsdinfo>::uninit();
    let info_len = size_of::<proc_bsdinfo>();
    let info_len_arg = c_int::try_from(info_len).expect("proc_bsdinfo size fits in c_int");
    let rc = unsafe {
        proc_pidinfo(
            posix_pid,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<c_void>(),
            info_len_arg,
        )
    };
    if rc < info_len_arg {
        return None;
    }
    let initialized_info = unsafe { info.assume_init() };
    (initialized_info.pbi_pid == pid).then(|| {
        format!(
            "{}-{}",
            initialized_info.pbi_start_tvsec, initialized_info.pbi_start_tvusec
        )
    })
}

#[cfg(unix)]
#[expect(
    clippy::undocumented_unsafe_blocks,
    reason = "getppid takes no pointers and has no caller preconditions"
)]
pub(super) fn parent_pid() -> Option<u32> {
    u32::try_from(unsafe { libc::getppid() }).ok()
}

pub(super) fn process_holder_is_live(pid: u32, start_time: &str) -> bool {
    process_start_time(pid).is_some_and(|current| current == start_time)
}

pub(super) fn configured_client_id(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_owned)
        .or_else(|| var("CADE_CLIENT_ID").ok())
        .filter(|id| !id.is_empty())
}

pub fn direnv_session_id(client_id: Option<&str>, owner_pid: Option<u32>) -> Option<String> {
    if let Some(resolved) = configured_client_id(client_id) {
        return Some(format!("direnv-lease-{}", stable_hash_hex(&resolved)));
    }
    let pid = owner_pid.or_else(parent_pid)?;
    let start_time = process_start_time(pid)?;
    Some(format!("direnv-{pid}-{}", stable_hash_hex(&start_time)))
}

pub fn direnv_fallback_session_id(root: &Path) -> String {
    format!(
        "direnv-root-{}",
        stable_hash_hex(root.to_string_lossy().as_ref())
    )
}
