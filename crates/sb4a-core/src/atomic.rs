// SPDX-License-Identifier: GPL-3.0-or-later
//! Crash-safe file writes: temp file in the same directory, fsync, rename, fsync dir.

use crate::error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

pub fn write(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, error::UNSAFE_PATH.0))?;
    let tmp = dir.join(format!(".commit-{}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        sync_dir(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    write(path, &bytes, 0o600)
}

pub fn read_limited(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let file = File::open(path)?;
    let mut buffer = Vec::new();
    file.take(limit + 1).read_to_end(&mut buffer)?;
    if buffer.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input_too_large",
        ));
    }
    Ok(buffer)
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> io::Result<T> {
    let bytes = read_limited(path, limit)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Exclusive advisory lock with a bounded wait. Returns a guard that unlocks on drop.
pub struct Lock(File);

impl Lock {
    pub fn try_acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            Ok(Self(file))
        } else {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                error::MODULE_BUSY.0,
            ))
        }
    }

    pub fn acquire(path: &Path, timeout: std::time::Duration) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(Self(file));
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    error::MODULE_BUSY.0,
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// 0700 directory that must not be a symlink.
pub fn private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                error::UNSAFE_DIRECTORY.0,
            ));
        }
    }
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Keep at most `keep` rotated files of `path` (path.1 .. path.N) and cap the
/// live file at `max_bytes` by moving it aside instead of growing forever.
pub fn rotate(path: &Path, max_bytes: u64, keep: usize) -> io::Result<()> {
    let too_big = fs::metadata(path)
        .map(|m| m.len() >= max_bytes)
        .unwrap_or(false);
    if !too_big {
        return Ok(());
    }
    if keep == 0 {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    let _ = fs::remove_file(rotated(path, keep));
    for index in (1..keep).rev() {
        let from = rotated(path, index);
        if from.exists() {
            let _ = fs::rename(&from, rotated(path, index + 1));
        }
    }
    fs::rename(path, rotated(path, 1))
}

fn rotated(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_bounds_files() {
        let base = std::env::temp_dir().join(format!("sb4a-rot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let path = base.join("core.log");
        fs::write(&path, vec![b'x'; 100]).unwrap();

        // Under the cap: untouched.
        rotate(&path, 200, 2).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 100);
        assert!(!rotated(&path, 1).exists());

        // Over the cap: moved aside, live file gone.
        rotate(&path, 50, 2).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::metadata(rotated(&path, 1)).unwrap().len(), 100);

        // A second rotation shifts .1 -> .2 and drops the excess.
        fs::write(&path, vec![b'y'; 100]).unwrap();
        rotate(&path, 50, 2).unwrap();
        assert!(rotated(&path, 1).exists());
        assert!(rotated(&path, 2).exists());
        assert!(!rotated(&path, 3).exists());

        let _ = fs::remove_dir_all(&base);
    }
}
