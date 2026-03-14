use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

const DEBOUNCE_MS: u64 = 500;

/// Watches a set of paths and sends the service name on the channel when a change is detected.
/// Returns a sender that stops the watcher thread when dropped or when any value is sent.
pub fn spawn_watcher(
    service: String,
    paths: Vec<PathBuf>,
    ignore: Vec<String>,
    tx: mpsc::Sender<String>,
) -> std_mpsc::SyncSender<()> {
    let (stop_tx, stop_rx) = std_mpsc::sync_channel::<()>(1);

    std::thread::spawn(move || {
        let (raw_tx, raw_rx) = std_mpsc::channel::<notify::Result<Event>>();
        let mut watcher = match RecommendedWatcher::new(raw_tx, notify::Config::default()) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("watcher init failed for {service}: {e}");
                return;
            }
        };

        for path in &paths {
            if let Err(e) = watcher.watch(path, RecursiveMode::Recursive) {
                tracing::warn!("cannot watch {}: {e}", path.display());
            }
        }

        let mut last_trigger = std::time::Instant::now()
            .checked_sub(Duration::from_millis(DEBOUNCE_MS + 1))
            .unwrap_or(std::time::Instant::now());

        loop {
            // Check stop signal (non-blocking)
            if stop_rx.try_recv().is_ok() {
                break;
            }

            // Poll for file events with a short timeout so we can check stop regularly
            match raw_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(event)) => {
                    let relevant = event.paths.iter().any(|p| {
                        let s = p.to_string_lossy();
                        !ignore.iter().any(|ig| s.contains(ig.as_str()))
                    });
                    if !relevant {
                        continue;
                    }
                    let now = std::time::Instant::now();
                    if now.duration_since(last_trigger) >= Duration::from_millis(DEBOUNCE_MS) {
                        last_trigger = now;
                        let _ = tx.blocking_send(service.clone());
                    }
                }
                Ok(Err(e)) => tracing::warn!("watch error for {service}: {e}"),
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    stop_tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_spawn_watcher_returns_stop_sender() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel::<String>(1);
        let stop_tx = spawn_watcher("svc".into(), vec![dir.path().to_path_buf()], vec![], tx);
        // Sending stop should not panic
        let _ = stop_tx.send(());
    }

    #[tokio::test]
    async fn test_stop_sender_terminates_watcher_thread() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);
        let stop_tx = spawn_watcher("svc".into(), vec![dir.path().to_path_buf()], vec![], tx);

        // Stop the watcher thread
        let _ = stop_tx.send(());
        // Allow the thread to exit
        tokio::time::sleep(Duration::from_millis(200)).await;
        // rx should yield nothing (tx is dropped when thread exits)
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_file_change_triggers_event() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
        let stop_tx = spawn_watcher("my-svc".into(), vec![dir.path().to_path_buf()], vec![], tx);

        // Brief pause so the watcher is set up before we write
        tokio::time::sleep(Duration::from_millis(100)).await;

        let file = dir.path().join("test.txt");
        let mut f = std::fs::File::create(&file).unwrap();
        writeln!(f, "change").unwrap();
        drop(f);

        // Wait for debounce + event propagation
        let result = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
        let _ = stop_tx.send(());
        assert!(
            matches!(result, Ok(Some(ref s)) if s == "my-svc"),
            "expected file change event for 'my-svc', got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_ignored_paths_suppressed() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
        let stop_tx = spawn_watcher(
            "svc".into(),
            vec![dir.path().to_path_buf()],
            vec!["target".into()], // ignore "target" subdirectory
            tx,
        );

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Write into the ignored path
        let target = dir.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        let mut f = std::fs::File::create(target.join("ignored.txt")).unwrap();
        writeln!(f, "ignored").unwrap();
        drop(f);

        // No event should arrive within a short window
        let result = tokio::time::timeout(Duration::from_millis(700), rx.recv()).await;
        let _ = stop_tx.send(());
        assert!(
            result.is_err(),
            "should not have received event for ignored path"
        );
    }
}
