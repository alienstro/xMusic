//! Owns the daemon's process lock and per-run control token.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const PID_FILE: &str = "daemon.pid";
const TOKEN_FILE: &str = "control.token";

pub struct RuntimeGuard {
    _pid_file: File,
    pid_path: PathBuf,
    token_path: PathBuf,
    pid: u32,
    token: String,
}

fn support_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".xmusic"))
}

pub fn acquire() -> Result<RuntimeGuard, String> {
    let dir = support_dir().ok_or("HOME unset, cannot create daemon runtime files")?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;

    let pid_path = dir.join(PID_FILE);
    let token_path = dir.join(TOKEN_FILE);
    let mut pid_file = secure_open(&pid_path)?;
    lock_exclusive(&pid_file).map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            "another xmusic-player already owns the daemon lock".to_string()
        } else {
            format!("cannot lock {}: {error}", pid_path.display())
        }
    })?;

    let pid = std::process::id();
    rewrite(&mut pid_file, &pid.to_string(), &pid_path)?;

    let token = random_token()?;
    if let Err(error) = write_secret(&token_path, &token) {
        let _ = std::fs::remove_file(&pid_path);
        return Err(error);
    }

    Ok(RuntimeGuard {
        _pid_file: pid_file,
        pid_path,
        token_path,
        pid,
        token,
    })
}

impl RuntimeGuard {
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.token_path)
            .is_ok_and(|value| value.trim() == self.token)
        {
            let _ = std::fs::remove_file(&self.token_path);
        }
        if std::fs::read_to_string(&self.pid_path)
            .is_ok_and(|value| value.trim() == self.pid.to_string())
        {
            let _ = std::fs::remove_file(&self.pid_path);
        }
    }
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| format!("cannot create control token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_secret(path: &Path, value: &str) -> Result<(), String> {
    let mut file = secure_open(path)?;
    rewrite(&mut file, value, path)
}

fn rewrite(file: &mut File, value: &str, path: &Path) -> Result<(), String> {
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| file.write_all(value.as_bytes()))
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn secure_open(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot secure {}: {error}", path.display()))?;
    }
    Ok(file)
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "daemon locking is only supported on Unix",
    ))
}
