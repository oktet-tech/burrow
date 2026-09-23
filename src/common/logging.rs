use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

const MAX_LOG_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
const MAX_ROTATED: u32 = 5;

/// Platform-appropriate log file path.
///
/// macOS: ~/Library/Logs/Burrow/burrow.log
/// Linux: ~/.local/state/burrow/burrow.log
pub fn log_path() -> PathBuf {
    let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());

    #[cfg(target_os = "macos")]
    {
        home.unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library/Logs/Burrow/burrow.log")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".local/state/burrow/burrow.log")
    }
}

/// Rotate log files if the current one exceeds MAX_LOG_SIZE.
/// burrow.log -> .1, .1 -> .2, ..., .4 -> .5 (oldest dropped)
fn rotate_if_needed(path: &PathBuf) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let meta = fs::metadata(path)?;
    if meta.len() < MAX_LOG_SIZE {
        return Ok(());
    }

    let base = path.to_string_lossy().to_string();

    // Shift existing rotated files (oldest first to avoid clobbering)
    for i in (1..MAX_ROTATED).rev() {
        let from = format!("{base}.{i}");
        let to = format!("{base}.{}", i + 1);
        if std::path::Path::new(&from).exists() {
            fs::rename(&from, &to)?;
        }
    }

    fs::rename(path, format!("{base}.1"))?;
    Ok(())
}

/// Append-only log file that rotates once it grows past MAX_LOG_SIZE, so a
/// long-running daemon can't grow it without bound.
struct RotatingFile {
    path: PathBuf,
    file: File,
    size: u64,
}

impl RotatingFile {
    fn open(path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        Ok(Self { path, file, size })
    }

    fn rotate(&mut self) -> io::Result<()> {
        // Checks the on-disk size: another process sharing the file may
        // already have rotated it.
        rotate_if_needed(&self.path)?;
        self.file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.size = self.file.metadata()?.len();
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size >= MAX_LOG_SIZE && self.rotate().is_err() {
            // Keep logging to the current file; retry after another full cycle.
            self.size = 0;
        }
        let n = self.file.write(buf)?;
        self.size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Set up tracing with file output, plus stderr when it is a terminal.
/// The log file rotates at startup and whenever it exceeds the size limit.
/// When `broadcast` is provided, log events are also pushed into the broadcast
/// channel for IPC subscribers (daemon mode).
pub fn init_logging(broadcast: Option<&super::log_broadcast::LogBroadcast>) {
    let path = log_path();

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Err(e) = rotate_if_needed(&path) {
        eprintln!("warning: log rotation failed: {e}");
    }

    let env_filter = tracing_subscriber::EnvFilter::try_from_env("BURROW_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let file = RotatingFile::open(path.clone());

    let broadcast_layer = broadcast.map(|b| b.layer());

    match file {
        Ok(file) => {
            use tracing_subscriber::prelude::*;

            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false);

            // The daemon's stderr is /dev/null; formatting for it is wasted work.
            let stderr_layer = io::stderr()
                .is_terminal()
                .then(|| tracing_subscriber::fmt::layer().with_writer(io::stderr));

            tracing_subscriber::registry()
                .with(env_filter)
                .with(file_layer)
                .with(stderr_layer)
                .with(broadcast_layer)
                .init();
        }
        Err(e) => {
            // Fallback: stderr-only logging
            eprintln!("warning: cannot open log file {}: {e}", path.display());
            use tracing_subscriber::prelude::*;

            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
                .with(broadcast_layer)
                .init();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_path_is_absolute() {
        let path = log_path();
        assert!(path.is_absolute());
        assert!(path.to_string_lossy().contains("burrow.log"));
    }

    #[test]
    fn rotate_noop_when_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("burrow.log");
        fs::write(&path, "small content").unwrap();

        rotate_if_needed(&path.to_path_buf()).unwrap();

        // File should still be there, no .1 created
        assert!(path.exists());
        assert!(!dir.path().join("burrow.log.1").exists());
    }

    #[test]
    fn rotate_moves_when_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("burrow.log");

        // Create a file just over the limit
        let data = vec![b'x'; (MAX_LOG_SIZE + 1) as usize];
        fs::write(&path, &data).unwrap();

        rotate_if_needed(&path.to_path_buf()).unwrap();

        // Original gone, .1 exists
        assert!(!path.exists());
        assert!(dir.path().join("burrow.log.1").exists());
    }

    #[test]
    fn rotate_shifts_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("burrow.log");

        // Pre-create .1 and .2
        fs::write(dir.path().join("burrow.log.1"), "one").unwrap();
        fs::write(dir.path().join("burrow.log.2"), "two").unwrap();

        // Create oversized current file
        let data = vec![b'x'; (MAX_LOG_SIZE + 1) as usize];
        fs::write(&path, &data).unwrap();

        rotate_if_needed(&path.to_path_buf()).unwrap();

        assert!(!path.exists());
        assert_eq!(fs::read_to_string(dir.path().join("burrow.log.2")).unwrap(), "one");
        assert_eq!(fs::read_to_string(dir.path().join("burrow.log.3")).unwrap(), "two");
        assert!(dir.path().join("burrow.log.1").exists());
    }

    #[test]
    fn rotate_drops_oldest_beyond_max() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("burrow.log");

        // Fill all rotation slots
        for i in 1..=MAX_ROTATED {
            fs::write(dir.path().join(format!("burrow.log.{i}")), format!("slot{i}")).unwrap();
        }

        let data = vec![b'x'; (MAX_LOG_SIZE + 1) as usize];
        fs::write(&path, &data).unwrap();

        rotate_if_needed(&path.to_path_buf()).unwrap();

        // .5 should now contain what was .4 (slot4)
        assert_eq!(
            fs::read_to_string(dir.path().join("burrow.log.5")).unwrap(),
            "slot4"
        );
        // slot5 (the oldest) was overwritten
    }

    #[test]
    fn rotating_file_rotates_past_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("burrow.log");
        fs::write(&path, vec![b'x'; MAX_LOG_SIZE as usize]).unwrap();

        let mut file = RotatingFile::open(path.clone()).unwrap();
        file.write_all(b"fresh line\n").unwrap();

        assert!(dir.path().join("burrow.log.1").exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "fresh line\n");
    }

    #[test]
    fn rotate_noop_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.log");
        rotate_if_needed(&path.to_path_buf()).unwrap();
    }
}
