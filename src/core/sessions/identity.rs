use anyhow::{Result, bail};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

// atomic rename avoids partial state files
pub(in crate::core) fn atomic_write(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("cade");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(".{stem}.tmp.{}.{nanos}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, path)) {
        std::fs::remove_file(&tmp).ok();
        return Err(e);
    }
    Ok(())
}

pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// path-safe session ids only
pub(in crate::core) fn is_valid_session(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(super) fn is_valid_client_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(super) fn validate_client_id(s: &str) -> Result<()> {
    if is_valid_client_id(s) {
        Ok(())
    } else {
        bail!("invalid cade lease client id")
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .is_err()
    {
        let seed = format!("{}-{}-{}", std::process::id(), now_secs(), new_session_id());
        for (i, byte) in seed.as_bytes().iter().enumerate() {
            buf[i % bytes] ^= *byte;
            buf[(i * 7 + 3) % bytes] = buf[(i * 7 + 3) % bytes].wrapping_add(*byte);
        }
    }
    buf.into_iter().map(|b| format!("{b:02x}")).collect()
}

pub(super) fn new_client_id() -> String {
    random_hex(16)
}

pub(in crate::core) fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

pub(super) fn stable_hash_hex(text: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(raw: &str) -> Option<(u32, String)> {
    let end = raw.rfind(") ")?;
    let fields: Vec<&str> = raw[end + 2..].split_whitespace().collect();
    let ppid = fields.get(1)?.parse::<u32>().ok()?;
    let start_time = fields.get(19)?.to_string();
    Some((ppid, start_time))
}

#[cfg(target_os = "linux")]
pub(super) fn process_start_time(pid: u32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_proc_stat(&raw).map(|(_, start)| start)
}

#[cfg(target_os = "macos")]
pub(super) fn process_start_time(pid: u32) -> Option<String> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let info_len = std::mem::size_of::<libc::proc_bsdinfo>();
    let rc = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            info_len as libc::c_int,
        )
    };
    if rc < info_len as libc::c_int {
        return None;
    }
    let info = unsafe { info.assume_init() };
    (info.pbi_pid == pid as u32)
        .then(|| format!("{}-{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn process_start_time(_pid: u32) -> Option<String> {
    None
}

#[cfg(unix)]
pub(super) fn parent_pid() -> Option<u32> {
    u32::try_from(unsafe { libc::getppid() }).ok()
}

#[cfg(not(unix))]
pub(super) fn parent_pid() -> Option<u32> {
    None
}

pub(super) fn process_holder_is_live(pid: u32, start_time: &str) -> bool {
    process_start_time(pid)
        .map(|current| current == start_time)
        .unwrap_or(false)
}

pub(super) fn configured_client_id(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("CADE_CLIENT_ID").ok())
        .filter(|id| !id.is_empty())
}

pub(in crate::core) fn direnv_session_id(
    client_id: Option<&str>,
    owner_pid: Option<u32>,
) -> Option<String> {
    if let Some(client_id) = configured_client_id(client_id) {
        return Some(format!("direnv-lease-{}", stable_hash_hex(&client_id)));
    }
    let pid = owner_pid.or_else(parent_pid)?;
    let start_time = process_start_time(pid)?;
    Some(format!("direnv-{pid}-{}", stable_hash_hex(&start_time)))
}

pub(in crate::core) fn direnv_fallback_session_id(root: &Path) -> String {
    format!(
        "direnv-root-{}",
        stable_hash_hex(root.to_string_lossy().as_ref())
    )
}
