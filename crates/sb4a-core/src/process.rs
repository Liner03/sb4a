// SPDX-License-Identifier: GPL-3.0-or-later
//! Process identity, signalling and cgroup escape.

use crate::error::{self, Code};
use crate::state::ProcessIdentity;
use std::fs;
use std::io;
use std::path::Path;

/// Read `/proc/<pid>` start time + executable so a reused PID is never mistaken
/// for the process we started.
pub fn identity(pid: i32) -> Result<ProcessIdentity, Code> {
    if pid <= 1 {
        return Err(error::PID_INVALID);
    }
    let base = Path::new("/proc").join(pid.to_string());
    let executable = fs::read_link(base.join("exe")).map_err(|_| error::PROCESS_DEAD)?;
    let raw = fs::read_to_string(base.join("stat")).map_err(|_| error::PROCESS_DEAD)?;
    let end = raw.rfind(')').ok_or(error::STAT_INVALID)?;
    let fields: Vec<&str> = raw[end + 1..].split_whitespace().collect();
    if fields.len() < 20 || fields[0] == "Z" {
        return Err(error::PROCESS_DEAD);
    }
    let executable = executable.to_string_lossy();
    Ok(ProcessIdentity {
        pid,
        start: fields[19].to_string(),
        executable: executable.trim_end_matches(" (deleted)").to_string(),
    })
}

pub fn same_process(want: &ProcessIdentity) -> bool {
    if want.pid <= 1 || want.start.is_empty() {
        return false;
    }
    matches!(identity(want.pid), Ok(got) if got == *want)
}

pub fn terminate(want: &ProcessIdentity) -> io::Result<()> {
    if !same_process(want) {
        return Ok(());
    }
    signal(want.pid, libc::SIGTERM)
}

pub fn force_terminate(want: &ProcessIdentity) -> io::Result<()> {
    if !same_process(want) {
        return Ok(());
    }
    signal(want.pid, libc::SIGKILL)
}

pub fn signal(pid: i32, sig: i32) -> io::Result<()> {
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn alive(pid: i32) -> bool {
    pid > 1 && unsafe { libc::kill(pid, 0) } == 0
}

/// Android force-stop kills every descendant of the App cgroup even after
/// `setsid`. Move only our own PID into init's cgroup, or refuse to start.
pub fn independent_cgroup() -> Result<(), Code> {
    let init = fs::read("/proc/1/cgroup").map_err(|_| error::CGROUP_UNAVAILABLE)?;
    let own = fs::read("/proc/self/cgroup").map_err(|_| error::CGROUP_UNAVAILABLE)?;
    if init == own {
        return Ok(());
    }
    let mounts =
        fs::read_to_string("/proc/self/mountinfo").map_err(|_| error::CGROUP_UNAVAILABLE)?;
    let init_text = String::from_utf8_lossy(&init);
    let mut joined = 0;
    for line in init_text.lines() {
        let mut parts = line.splitn(3, ':');
        let (_, controllers, path) = match (parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => continue,
        };
        for mount in mounts.lines() {
            let mut halves = mount.splitn(2, " - ");
            let (front, back) = match (halves.next(), halves.next()) {
                (Some(a), Some(b)) => (a, b),
                _ => continue,
            };
            let front: Vec<&str> = front.split_whitespace().collect();
            let back: Vec<&str> = back.split_whitespace().collect();
            if front.len() < 5 || back.len() < 3 {
                continue;
            }
            let mut matched = controllers.is_empty() && back[0] == "cgroup2";
            if back[0] == "cgroup" {
                for controller in controllers.split(',') {
                    if format!(",{},", back[2]).contains(&format!(",{},", controller)) {
                        matched = true;
                    }
                }
            }
            if !matched || !path.starts_with(front[3]) {
                continue;
            }
            let relative = &path[front[3].len()..];
            let group = Path::new(front[4]).join(relative).join("cgroup.procs");
            fs::write(&group, std::process::id().to_string())
                .map_err(|_| error::CGROUP_MOVE_FAILED)?;
            joined += 1;
        }
    }
    if joined == 0 {
        return Err(error::CGROUP_MOVE_FAILED);
    }
    let after = fs::read("/proc/self/cgroup").map_err(|_| error::CGROUP_UNAVAILABLE)?;
    if after != init {
        return Err(error::CGROUP_MOVE_FAILED);
    }
    Ok(())
}

pub fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Child dies with the supervisor.
pub fn parent_death_signal(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_of_self_is_stable() {
        let me = identity(std::process::id() as i32).expect("self identity");
        assert!(same_process(&me));
        assert!(!me.start.is_empty());
    }

    #[test]
    fn rejects_bogus_pid() {
        assert_eq!(identity(0).unwrap_err(), error::PID_INVALID);
        assert_eq!(identity(1).unwrap_err(), error::PID_INVALID);
    }

    #[test]
    fn dead_process_is_not_same() {
        let ghost = ProcessIdentity {
            pid: 2_000_000,
            start: "1".into(),
            executable: "/nonexistent".into(),
        };
        assert!(!same_process(&ghost));
        assert!(terminate(&ghost).is_ok());
    }
}
