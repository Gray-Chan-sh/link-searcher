//! Real-time file watcher with debounce for tracking filesystem changes.
//!
//! Uses `notify` + `notify-debouncer-full` to watch directories recursively
//! and emit debounced [`FileChangeEvent`]s to a channel consumer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use notify::RecursiveMode;
use notify::Event;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};

/// A file-system change event produced by the debounced watcher.
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    /// The directory ID (from `dir_config`) that this event belongs to.
    pub dir_id: String,
    /// The absolute path of the changed file.
    pub path: PathBuf,
    /// The kind of change.
    pub kind: ChangeKind,
}

/// The type of file-system change detected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChangeKind {
    Create,
    Modify,
    Delete,
}

/// Commands sent to the background watcher thread.
pub enum WatcherCommand {
    StartWatch {
        dir_id: String,
        path: PathBuf,
    },
    StopWatch {
        dir_id: String,
    },
    Shutdown,
}

/// A handle to a background file watcher thread.
///
/// Create one via [`FileWatcher::new`], then call [`start_watching`](Self::start_watching)
/// and [`stop_watching`](Self::stop_watching) to manage watched directories.
/// Drop the handle to shut down the watcher.
pub struct FileWatcher {
    #[allow(dead_code)]
    watcher: Option<notify::RecommendedWatcher>,
    tx: mpsc::Sender<WatcherCommand>,
}

impl FileWatcher {
    /// Create a new debounced file watcher.
    ///
    /// Returns a handle and a [`mpsc::Receiver`] that receives [`FileChangeEvent`]s
    /// with a 300 ms debounce window.
    pub fn new() -> (Self, mpsc::Receiver<FileChangeEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let watches: Arc<Mutex<HashMap<String, PathBuf>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let event_tx_cb = event_tx;
        let watches_cb = Arc::clone(&watches);
        let watches_cmd = Arc::clone(&watches);

        std::thread::spawn(move || {
            let mut debouncer = match new_debouncer(
                Duration::from_millis(300),
                None,
                move |result: DebounceEventResult| {
                    let Ok(events) = result else { return };
                    let watches = match watches_cb.lock() {
                        Ok(w) => w,
                        Err(_) => return,
                    };
                    for debounced in &events {
                        let Some(kind) = classify_event(&debounced.event) else { continue };
                        for path in &debounced.event.paths {
                            let Some(dir_id) = find_matching_dir(&watches, path) else {
                                continue;
                            };
                            if event_tx_cb
                                .send(FileChangeEvent {
                                    dir_id,
                                    path: path.clone(),
                                    kind,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                },
            ) {
                Ok(d) => d,
                Err(e) => {
                    log::error!("Failed to create file watcher debouncer: {e}");
                    return;
                }
            };

            loop {
                match cmd_rx.recv() {
                    Ok(WatcherCommand::StartWatch { dir_id, path }) => {
                        if let Ok(mut w) = watches_cmd.lock() {
                            w.insert(dir_id, path.clone());
                        }
                        if let Err(e) =
                            debouncer.watch(&path, RecursiveMode::Recursive)
                        {
                            log::warn!("Failed to watch directory {path:?}: {e}");
                        }
                    }
                    Ok(WatcherCommand::StopWatch { dir_id }) => {
                        let path = watches_cmd.lock().ok().and_then(|mut w| {
                            w.remove(&dir_id)
                        });
                        if let Some(p) = path {
                            if let Err(e) = debouncer.unwatch(&p) {
                                log::warn!("Failed to unwatch directory {p:?}: {e}");
                            }
                        }
                    }
                    Ok(WatcherCommand::Shutdown) | Err(_) => break,
                }
            }
        });

        (Self { watcher: None, tx: cmd_tx }, event_rx)
    }

    /// Start watching a directory for changes.
    ///
    /// If the directory is already watched, the mapping is updated.
    pub fn start_watching(&mut self, dir_id: String, path: PathBuf) -> Result<()> {
        self.tx.send(WatcherCommand::StartWatch { dir_id, path })?;
        Ok(())
    }

    /// Stop watching a directory by its ID.
    pub fn stop_watching(&mut self, dir_id: &str) -> Result<()> {
        self.tx
            .send(WatcherCommand::StopWatch {
                dir_id: dir_id.to_string(),
            })?;
        Ok(())
    }

    /// Get a reference to the command sender channel.
    pub fn tx(&self) -> &mpsc::Sender<WatcherCommand> {
        &self.tx
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        let _ = self.tx.send(WatcherCommand::Shutdown);
    }
}

/// Map a [`notify::Event`] to a [`ChangeKind`], returning `None` for events
/// that should be ignored (e.g. access or other meta-events).
fn classify_event(event: &Event) -> Option<ChangeKind> {
    match event.kind {
        notify::EventKind::Create(_) => Some(ChangeKind::Create),
        notify::EventKind::Modify(_) => Some(ChangeKind::Modify),
        notify::EventKind::Remove(_) => Some(ChangeKind::Delete),
        _ => None,
    }
}

/// Find the first watched directory that is an ancestor of `path`.
/// Normalizes both sides to forward slashes so DB-stored paths match on Windows.
fn find_matching_dir(
    watches: &HashMap<String, PathBuf>,
    path: &Path,
) -> Option<String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    for (dir_id, dir_path) in watches {
        let dir_normalized = dir_path.to_string_lossy().replace('\\', "/");
        if normalized.starts_with(&dir_normalized) {
            return Some(dir_id.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watcher_creation_and_shutdown() {
        let (watcher, _rx) = FileWatcher::new();
        // Just verify creation and Drop don't panic
        drop(watcher);
    }

    #[test]
    fn test_start_stop_watch() {
        let (mut watcher, rx) = FileWatcher::new();
        let dir = std::env::temp_dir().join("ls_watcher_test_start_stop");
        let _ = std::fs::create_dir_all(&dir);

        watcher
            .start_watching("test-dir".into(), dir.clone())
            .expect("start_watching");
        watcher
            .stop_watching("test-dir")
            .expect("stop_watching");

        // Give the thread a moment to process commands
        std::thread::sleep(Duration::from_millis(50));
        drop(rx);
        drop(watcher);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_change_kind_classification() {
        use notify::EventKind;
        use notify::Event;

        let make_event = |kind: EventKind| Event {
            kind,
            paths: vec![PathBuf::from("/tmp/test.txt")],
            attrs: notify::event::EventAttributes::default(),
            ..Event::default()
        };

        assert_eq!(
            classify_event(&make_event(EventKind::Create(notify::event::CreateKind::File))),
            Some(ChangeKind::Create)
        );
        assert_eq!(
            classify_event(&make_event(EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )))),
            Some(ChangeKind::Modify)
        );
        assert_eq!(
            classify_event(&make_event(EventKind::Remove(notify::event::RemoveKind::File))),
            Some(ChangeKind::Delete)
        );
        // Unrelated events are ignored
        assert_eq!(
            classify_event(&make_event(EventKind::Access(notify::event::AccessKind::Read))),
            None
        );
    }

    #[test]
    fn test_find_matching_dir() {
        let mut watches = HashMap::new();
        watches.insert("d1".into(), PathBuf::from("/home/user/docs"));
        watches.insert("d2".into(), PathBuf::from("/home/user/photos"));

        assert_eq!(
            find_matching_dir(
                &watches,
                Path::new("/home/user/docs/report.txt")
            ),
            Some("d1".into())
        );
        assert_eq!(
            find_matching_dir(
                &watches,
                Path::new("/home/user/photos/vacation/img.png")
            ),
            Some("d2".into())
        );
        assert_eq!(
            find_matching_dir(
                &watches,
                Path::new("/home/user/other/file.txt")
            ),
            None
        );
    }
}