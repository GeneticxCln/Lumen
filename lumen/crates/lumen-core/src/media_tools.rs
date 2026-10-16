//! Helpers for running ffmpeg/ffprobe on files of unknown quality.
//!
//! A damaged or hostile video can make ffmpeg run for a very long time. Callers use
//! `run_with_timeout` so a bad file costs a bounded wait and a killed process, not a blocked
//! worker thread forever. The thumbnail cache gets a stable file naming scheme and a size cap so
//! it cannot grow without limit.

use crate::error::{PlayerError, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Runs `command` to completion, or kills it after `limit`. Both output pipes are drained on
/// their own threads while waiting: a child that fills an unread pipe blocks forever, which would
/// look like a timeout it never had a chance to beat.
pub fn run_with_timeout(mut command: Command, limit: Duration) -> Result<Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so a timeout can stop the program and anything it started. Killing
    // only the parent would leave a child holding the pipes open.
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .spawn()
        .map_err(|error| PlayerError::Io(format!("Could not start {program}: {error}")))?;

    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );

    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(child.id() as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
                let _ = child.kill();
                let _ = child.wait();
                // The drain threads end by themselves once the killed group closes its pipes;
                // waiting for them here could only add delay.
                drop((stdout, stderr));
                return Err(PlayerError::Io(format!(
                    "{program} did not finish within {} seconds and was stopped",
                    limit.as_secs()
                )));
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// A short, stable cache file stem for a set of identifying parts (path, size, mtime, frame...).
/// SHA-256 rather than `DefaultHasher`, whose output is not guaranteed to survive a Rust upgrade
/// and would silently orphan the whole cache.
pub fn cache_name(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Deletes the oldest `.jpg` files in `dir` until what remains is at most `max_bytes`.
/// Hidden temporary files are left alone: they may belong to a write in progress.
pub fn prune_cache(dir: &Path, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_string();
            if name.starts_with('.') || !name.ends_with(".jpg") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            Some((
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                meta.len(),
                path,
            ))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    if total <= max_bytes {
        return;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn a_process_that_finishes_returns_its_output() {
        let output =
            run_with_timeout(shell("echo out; echo err 1>&2"), Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "err");
    }

    #[test]
    fn a_process_that_runs_too_long_is_killed() {
        let started = Instant::now();
        let error = run_with_timeout(shell("sleep 30"), Duration::from_millis(300)).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "returned promptly, not after 30 s"
        );
        assert!(String::from(error).contains("did not finish"));
    }

    #[test]
    fn large_output_does_not_deadlock_the_wait() {
        // 2 MB is far past a pipe's buffer; without draining, the child would block and time out.
        let output =
            run_with_timeout(shell("head -c 2000000 /dev/zero"), Duration::from_secs(10)).unwrap();
        assert_eq!(output.stdout.len(), 2_000_000);
    }

    #[test]
    fn a_missing_program_is_a_clear_error() {
        let error = run_with_timeout(Command::new("lumen-no-such-tool"), Duration::from_secs(1))
            .unwrap_err();
        assert!(String::from(error).contains("lumen-no-such-tool"));
    }

    #[test]
    fn cache_names_are_stable_and_distinguish_their_parts() {
        let name = cache_name(&["/videos/a.mp4", "1000", "5", "2"]);
        assert_eq!(name.len(), 16);
        assert_eq!(
            name,
            cache_name(&["/videos/a.mp4", "1000", "5", "2"]),
            "same input, same name"
        );
        assert_ne!(
            name,
            cache_name(&["/videos/a.mp4", "1000", "5", "3"]),
            "frame time changes the name"
        );
        // Boundaries matter: ("ab","c") must not collide with ("a","bc").
        assert_ne!(cache_name(&["ab", "c"]), cache_name(&["a", "bc"]));
        // Pinned against an independent computation (SHA-256 over length-prefixed parts, first 8
        // bytes). If this changes, every user's thumbnail cache is orphaned on upgrade.
        assert_eq!(cache_name(&["x"]), "f91b14e7bbea4c5b");
        assert_eq!(name, "a78e61484be451a3");
    }

    #[test]
    fn pruning_removes_the_oldest_files_first_and_leaves_temporaries() {
        let dir = std::env::temp_dir().join(format!("lumen-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, age) in [("old.jpg", 300u64), ("mid.jpg", 200), ("new.jpg", 100)] {
            let path = dir.join(name);
            std::fs::write(&path, vec![0u8; 1000]).unwrap();
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
        std::fs::write(dir.join(".partial.jpg"), vec![0u8; 5000]).unwrap();

        prune_cache(&dir, 2000);
        assert!(!dir.join("old.jpg").exists(), "oldest goes first");
        assert!(dir.join("mid.jpg").exists() && dir.join("new.jpg").exists());
        assert!(
            dir.join(".partial.jpg").exists(),
            "a write in progress is not touched"
        );

        prune_cache(&dir, 10_000);
        assert!(
            dir.join("mid.jpg").exists(),
            "under the cap, nothing is removed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
