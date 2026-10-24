//! Crash/diagnostic logging. Installs a rolling file logger under the app's data directory and
//! a panic hook that routes panic messages through it before falling back to the default
//! stderr hook, so a crash leaves a record even when nobody was watching the terminal.

use crate::error::{PlayerError, Result};
use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

const LOG_FILE_PREFIX: &str = "lumen.log";
const MAX_LOG_FILES: usize = 7;

pub fn log_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("logs")
}

/// Sets up file-backed logging and the panic hook. Returns a guard that must be kept alive
/// (e.g. via `app.manage(...)`) for the duration of the program — dropping it stops the
/// background flush thread and the writer silently stops accepting lines.
pub fn init(app_data_dir: &Path) -> Result<WorkerGuard> {
    let dir = log_dir(app_data_dir);
    std::fs::create_dir_all(&dir).map_err(PlayerError::from)?;

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(&dir)
        .map_err(|e| PlayerError::Io(e.to_string()))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Another call (e.g. from an earlier test in the same process) may already own the global
    // subscriber; that's not an error here — the appender above still gets created either way,
    // and the first caller in a real run always wins.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();

    install_panic_hook();
    Ok(guard)
}

fn log_panic_message(message: &str) {
    tracing::error!(target: "panic", "{message}");
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log_panic_message(&info.to_string());
        default_hook(info);
    }));
}

/// The last `lines` lines of the most recently modified log file, for a "copy diagnostics" UI
/// affordance. Empty string if no log file exists yet.
pub async fn tail(app_data_dir: &Path, lines: usize) -> Result<String> {
    let dir = log_dir(app_data_dir);
    let mut entries: Vec<PathBuf> = match tokio::fs::read_dir(&dir).await {
        Ok(mut read_dir) => {
            let mut paths = Vec::new();
            while let Some(entry) = read_dir.next_entry().await.map_err(PlayerError::from)? {
                let path = entry.path();
                if path.is_file() {
                    paths.push(path);
                }
            }
            paths
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(PlayerError::from(e)),
    };
    entries.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    let Some(latest) = entries.last() else {
        return Ok(String::new());
    };
    let content = tokio::fs::read_to_string(latest)
        .await
        .map_err(PlayerError::from)?;
    let tail: Vec<&str> = content.lines().rev().take(lines).collect();
    Ok(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct VecWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for VecWriter {
        type Writer = VecWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-diag-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn panic_message_is_logged_through_tracing() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = VecWriter(buf.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_panic_message("boom at crates/lumen-core/src/foo.rs:10");
        });
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(output.contains("boom at crates/lumen-core/src/foo.rs:10"));
    }

    #[test]
    fn init_creates_the_log_directory_and_a_log_file() {
        let dir = temp_dir("rolling");
        let _guard = init(&dir).expect("init should succeed");
        let files: Vec<_> = std::fs::read_dir(log_dir(&dir))
            .expect("log dir should exist")
            .collect();
        assert!(!files.is_empty(), "expected at least one log file");
    }

    #[tokio::test]
    async fn tail_returns_empty_string_when_no_log_dir_exists_yet() {
        let dir = temp_dir("no-logs");
        assert_eq!(tail(&dir, 10).await.unwrap(), "");
    }

    #[tokio::test]
    async fn tail_returns_the_last_n_lines_in_order() {
        let dir = temp_dir("tail");
        let logs = log_dir(&dir);
        std::fs::create_dir_all(&logs).unwrap();
        let content = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(logs.join("lumen.log.2026-01-01"), content).unwrap();

        let result = tail(&dir, 3).await.unwrap();
        assert_eq!(result, "line 18\nline 19\nline 20");
    }

    #[tokio::test]
    async fn tail_uses_the_most_recently_modified_file_when_several_exist() {
        let dir = temp_dir("tail-multi");
        let logs = log_dir(&dir);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("lumen.log.2026-01-01"), "old line").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        std::fs::write(logs.join("lumen.log.2026-01-02"), "new line").unwrap();

        let result = tail(&dir, 10).await.unwrap();
        assert_eq!(result, "new line");
    }
}
