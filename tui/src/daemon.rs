//! Finding, starting and stopping the playback daemon.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Must match `BIND_ADDR` in the player crate.
pub const BASE_URL: &str = "http://127.0.0.1:13723";
pub const AUTH_HEADER: &str = "X-Xmusic-Token";

const PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const PID_FILE: &str = "daemon.pid";
const TOKEN_FILE: &str = "control.token";
const DAEMON_BINARY: &str = "xmusic-player";
const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug)]
pub enum Status {
    Stopped,
    Compatible,
    Incompatible(String),
    Unreachable(String),
    Unrecognized(String),
}

#[derive(Deserialize)]
struct Health {
    ok: bool,
    #[serde(default)]
    service: Option<String>,
    version: String,
    #[serde(default)]
    protocol: Option<u32>,
}

pub fn status() -> Status {
    match ureq::get(&format!("{BASE_URL}/health"))
        .timeout(PROBE_TIMEOUT)
        .call()
    {
        Ok(response) => match response.into_json::<Health>() {
            Ok(health)
                if health.ok
                    && health
                        .service
                        .as_deref()
                        .is_none_or(|service| service == DAEMON_BINARY) =>
            {
                if health.protocol != Some(PROTOCOL_VERSION) {
                    Status::Incompatible(format!("{} (legacy protocol)", health.version))
                } else if health.version != env!("CARGO_PKG_VERSION") {
                    Status::Incompatible(health.version)
                } else if let Err(error) = control_token() {
                    Status::Unreachable(error)
                } else {
                    Status::Compatible
                }
            }
            Ok(health) => Status::Unrecognized(format!(
                "port 13723 belongs to service {:?}",
                health.service.unwrap_or_else(|| "unknown".into())
            )),
            Err(error) => Status::Unrecognized(format!(
                "port 13723 returned an invalid health response: {error}"
            )),
        },
        Err(ureq::Error::Status(code, _)) => {
            Status::Unrecognized(format!("port 13723 returned HTTP {code} for /health"))
        }
        Err(ureq::Error::Transport(error)) => match runtime_lock_held() {
            Ok(true) => Status::Unreachable(error.to_string()),
            Ok(false) => Status::Stopped,
            Err(message) => Status::Unreachable(message),
        },
    }
}

fn support_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".xmusic"))
}

fn pid_file() -> Option<PathBuf> {
    support_dir().map(|dir| dir.join(PID_FILE))
}

fn token_file() -> Option<PathBuf> {
    support_dir().map(|dir| dir.join(TOKEN_FILE))
}

pub fn control_token() -> Result<String, String> {
    let path = token_file().ok_or("HOME unset, cannot locate control token")?;
    let token = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let token = token.trim();
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("control token {} is malformed", path.display()));
    }
    Ok(token.to_string())
}

/// Finds the daemon binary.
///
/// It normally sits next to this one, but "next to" needs care: `current_exe`
/// is allowed to return the symlink that was invoked rather than its target,
/// and every packaged install is a symlink. Try the resolved location first and
/// fall back to PATH.
pub fn locate_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;

    let mut candidates = Vec::new();
    if let Ok(resolved) = std::fs::canonicalize(&exe) {
        candidates.push(resolved.with_file_name(DAEMON_BINARY));
    }
    candidates.push(exe.with_file_name(DAEMON_BINARY));

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .or_else(on_path)
}

fn on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(DAEMON_BINARY))
        .find(|candidate| candidate.is_file())
}

/// Starts a compatible daemon if one is not already available.
pub fn ensure_running() -> Result<bool, String> {
    match status() {
        Status::Compatible => return Ok(false),
        Status::Incompatible(version) => {
            stop().map_err(|error| {
                format!(
                    "cannot replace daemon version {version} with {}: {error}",
                    env!("CARGO_PKG_VERSION")
                )
            })?;
        }
        Status::Unreachable(message) => {
            return Err(format!(
                "daemon process exists but is not answering: {message}; use --kill-daemon"
            ));
        }
        Status::Unrecognized(message) => return Err(message),
        Status::Stopped => {}
    }

    let binary = locate_binary().ok_or_else(|| {
        "xmusic-player not found beside xmusic or on PATH. Build the workspace \
         (`cargo build`), install it (`cargo install --path player`), or start \
         the daemon yourself."
            .to_string()
    })?;

    let mut command = Command::new(&binary);
    command.stdin(Stdio::null());
    match support_dir() {
        Some(dir) => {
            std::fs::create_dir_all(&dir)
                .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("daemon.log"))
                .map_err(|error| format!("cannot open daemon log: {error}"))?;
            let errors = log
                .try_clone()
                .map_err(|error| format!("cannot open daemon log: {error}"))?;
            command.stdout(log).stderr(errors);
        }
        None => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }

    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", binary.display()))?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        match status() {
            Status::Compatible => {
                reap_in_background(child);
                return Ok(true);
            }
            Status::Incompatible(version) => {
                terminate_child(&mut child);
                return Err(format!(
                    "started daemon reported version {version}, expected {}",
                    env!("CARGO_PKG_VERSION")
                ));
            }
            Status::Unrecognized(message) => {
                terminate_child(&mut child);
                return Err(message);
            }
            Status::Stopped | Status::Unreachable(_) => {}
        }
        if let Some(exit) = child
            .try_wait()
            .map_err(|error| format!("cannot inspect daemon process: {error}"))?
        {
            return Err(format!(
                "{} exited during startup with {exit}; see ~/.xmusic/daemon.log",
                binary.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    terminate_child(&mut child);
    Err(format!(
        "daemon did not answer within {}s - see ~/.xmusic/daemon.log",
        STARTUP_TIMEOUT.as_secs()
    ))
}

fn reap_in_background(mut child: Child) {
    let _ = std::thread::Builder::new()
        .name("daemon-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Stops the daemon. HTTP is preferred; the locked PID fallback is used only
/// after verifying that the PID still belongs to xmusic-player.
pub fn stop() -> Result<String, String> {
    let mut request = ureq::post(&format!("{BASE_URL}/quit")).timeout(Duration::from_millis(1500));
    if let Ok(token) = control_token() {
        request = request.set(AUTH_HEADER, &token);
    }
    let asked = request.send_json(serde_json::json!({})).is_ok();

    if asked {
        for _ in 0..25 {
            if matches!(status(), Status::Stopped) {
                cleanup_runtime_files();
                return Ok("daemon stopped".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    let pid = verified_locked_pid()?;
    signal(pid, libc::SIGTERM, "TERM")?;
    if wait_for_exit(pid, Duration::from_secs(5)) {
        cleanup_runtime_files();
        return Ok(format!("daemon {pid} terminated"));
    }

    signal(pid, libc::SIGKILL, "KILL")?;
    if !wait_for_exit(pid, Duration::from_secs(2)) {
        return Err(format!("daemon {pid} survived SIGKILL"));
    }
    cleanup_runtime_files();
    Ok(format!("daemon {pid} killed"))
}

fn verified_locked_pid() -> Result<u32, String> {
    let path = pid_file().ok_or("HOME unset, cannot locate pid file")?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|_| "daemon is not answering and no pid file exists".to_string())?;
    if !lock_is_held(&file)? {
        cleanup_runtime_files();
        return Err(format!(
            "pid file {} is stale; refusing to signal an unverified process",
            path.display()
        ));
    }

    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let pid = raw
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("pid file {} is malformed", path.display()))?;
    if !process_is_player(pid)? {
        return Err(format!(
            "pid {pid} does not belong to {DAEMON_BINARY}; refusing to signal it"
        ));
    }
    Ok(pid)
}

fn runtime_lock_held() -> Result<bool, String> {
    let Some(path) = pid_file() else {
        return Ok(false);
    };
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot open {}: {error}", path.display())),
    };
    lock_is_held(&file)
}

#[cfg(unix)]
fn lock_is_held(file: &File) -> Result<bool, String> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    let raw_error = error.raw_os_error();
    if raw_error == Some(libc::EWOULDBLOCK) || raw_error == Some(libc::EAGAIN) {
        Ok(true)
    } else {
        Err(format!("cannot inspect daemon lock: {error}"))
    }
}

#[cfg(not(unix))]
fn lock_is_held(_file: &File) -> Result<bool, String> {
    Err("daemon locking is only supported on Unix".into())
}

#[cfg(target_os = "macos")]
fn process_is_player(pid: u32) -> Result<bool, String> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    let pid = i32::try_from(pid).map_err(|_| "pid is outside the platform range")?;
    let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe {
        libc::proc_pidpath(
            pid,
            buffer.as_mut_ptr().cast(),
            libc::PROC_PIDPATHINFO_MAXSIZE as u32,
        )
    };
    if length <= 0 {
        return Ok(false);
    }
    let path = CStr::from_bytes_until_nul(&buffer)
        .map_err(|error| format!("cannot decode daemon process path: {error}"))?;
    Ok(Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()))
        .file_name()
        .is_some_and(|name| name == DAEMON_BINARY))
}

#[cfg(target_os = "linux")]
fn process_is_player(pid: u32) -> Result<bool, String> {
    let path = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map_err(|error| format!("cannot inspect daemon process {pid}: {error}"))?;
    Ok(path
        .file_name()
        .is_some_and(|name| name == DAEMON_BINARY))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_is_player(_pid: u32) -> Result<bool, String> {
    Err("process identity verification is unsupported on this platform".into())
}

fn signal(pid: u32, signal: libc::c_int, name: &str) -> Result<(), String> {
    let pid = i32::try_from(pid).map_err(|_| "pid is outside the platform range")?;
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "kill -{name} {pid} failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !process_alive(pid)
}

fn cleanup_runtime_files() {
    if let Some(path) = pid_file() {
        let _ = std::fs::remove_file(path);
    }
    if let Some(path) = token_file() {
        let _ = std::fs::remove_file(path);
    }
}
