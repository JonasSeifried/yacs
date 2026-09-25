//! Big files in the background: one upload or download at a time, with its
//! progress in Spotlight. Spotlight may hide meanwhile; it asks for the
//! progress again when it's shown.

use std::fs::File;
use std::future::Future;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;
use yacs_client::Sink;
use yacs_core::StreamFile;

use crate::windows;

/// Sent to Spotlight as a transfer progresses and when it ends.
pub const EVENT_TRANSFER_CHANGED: &str = "transfer-changed";
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub direction: Direction,
    /// "disk.iso" or "3 files".
    pub label: String,
    pub done: u64,
    pub total: u64,
}

/// How a transfer ended, for Spotlight's notice.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finished {
    pub direction: Direction,
    pub ok: bool,
    /// Stopped by the user: not an error worth showing in red.
    pub cancelled: bool,
    pub message: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Changed {
    transfer: Option<Transfer>,
    finished: Option<Finished>,
}

#[derive(Default)]
pub struct Transfers {
    current: Mutex<Option<Running>>,
}

struct Running {
    transfer: Transfer,
    cancel: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
    reported: Instant,
}

impl Transfers {
    pub fn current(&self) -> Option<Transfer> {
        self.lock().as_ref().map(|r| r.transfer.clone())
    }

    /// The transfer stops at its next step and reports "Cancelled".
    pub fn cancel(&self) {
        if let Some(running) = &*self.lock() {
            running.cancelled.store(true, Ordering::Relaxed);
            running.cancel.notify_one();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Running>> {
        self.current.lock().expect("transfers lock poisoned")
    }
}

/// Hands progress from a running transfer to Spotlight, a few times a second.
#[derive(Clone)]
pub struct Reporter(AppHandle);

impl Reporter {
    pub fn report(&self, done: u64, total: u64) {
        let transfers = self.0.state::<Transfers>();
        let mut current = transfers.lock();
        let Some(running) = current.as_mut() else {
            return;
        };
        running.transfer.done = done;
        running.transfer.total = total;
        if running.reported.elapsed() < PROGRESS_EVERY && done < total {
            return;
        }
        running.reported = Instant::now();
        let transfer = running.transfer.clone();
        drop(current);
        emit(&self.0, Some(transfer), None);
    }
}

/// Starts `work` as the one transfer, unless another is running. It gets a
/// [`Reporter`] and a cancel signal, and returns the message to show once
/// it's done.
pub fn start<Fut>(
    app: &AppHandle,
    direction: Direction,
    label: String,
    total: u64,
    work: impl FnOnce(Reporter, Arc<Notify>) -> Fut,
) -> Result<Transfer, String>
where
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    let transfers = app.state::<Transfers>();
    let transfer = Transfer {
        direction,
        label,
        done: 0,
        total,
    };
    let cancel = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut current = transfers.lock();
        if let Some(running) = &*current {
            let what = match running.transfer.direction {
                Direction::Upload => "upload",
                Direction::Download => "download",
            };
            return Err(format!(
                "Wait for the {what} of {} to finish, or cancel it.",
                running.transfer.label
            ));
        }
        *current = Some(Running {
            transfer: transfer.clone(),
            cancel: cancel.clone(),
            cancelled: cancelled.clone(),
            reported: Instant::now(),
        });
    }
    emit(app, Some(transfer.clone()), None);

    let work = work(Reporter(app.clone()), cancel);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = work.await;
        *app.state::<Transfers>().lock() = None;
        let (ok, message) = match result {
            Ok(message) => (true, message),
            Err(message) => (false, message),
        };
        tracing::info!(?direction, ok, %message, "transfer finished");
        let finished = Finished {
            direction,
            ok,
            cancelled: !ok && cancelled.load(Ordering::Relaxed),
            message,
        };
        emit(&app, None, Some(finished));
    });
    Ok(transfer)
}

fn emit(app: &AppHandle, transfer: Option<Transfer>, finished: Option<Finished>) {
    let changed = Changed { transfer, finished };
    let _ = app.emit_to(windows::SPOTLIGHT, EVENT_TRANSFER_CHANGED, changed);
}

/// `disk.iso` or `3 files`.
pub fn label<'a>(mut names: impl ExactSizeIterator<Item = &'a str>) -> String {
    match names.len() {
        1 => names.next().expect("one name").to_owned(),
        n => format!("{n} files"),
    }
}

/// Writes downloaded files into a folder, each as `name.part` until it's
/// complete, then under a free name (numbered if taken).
pub struct FolderSink {
    dir: PathBuf,
    names: Vec<String>,
    current: Option<(BufWriter<File>, PathBuf, usize)>,
    saved: Vec<PathBuf>,
}

impl FolderSink {
    pub fn new(dir: &Path, files: &[StreamFile]) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        Ok(Self {
            dir: dir.to_owned(),
            names: files.iter().map(StreamFile::safe_name).collect(),
            current: None,
            saved: Vec::new(),
        })
    }

    fn close(&mut self) -> std::io::Result<()> {
        let Some((mut out, part, index)) = self.current.take() else {
            return Ok(());
        };
        out.flush()?;
        drop(out);
        let path = crate::clipboard::free_path(&self.dir, &self.names[index]);
        std::fs::rename(&part, &path)?;
        self.saved.push(path);
        Ok(())
    }

    /// Every file's final path, once all arrived.
    pub fn finish(mut self) -> Result<Vec<PathBuf>, String> {
        self.close()
            .map_err(|e| format!("can't save the file: {e}"))?;
        Ok(self.saved)
    }

    /// Removes the unfinished file; finished ones stay.
    pub fn discard(mut self) {
        if let Some((out, part, _)) = self.current.take() {
            drop(out);
            let _ = std::fs::remove_file(part);
        }
    }
}

impl Sink for FolderSink {
    fn start_file(&mut self, index: usize) -> std::io::Result<()> {
        self.close()?;
        let part = self.dir.join(format!("{}.part", self.names[index]));
        let file = File::create(&part)?;
        self.current = Some((BufWriter::with_capacity(1 << 20, file), part, index));
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        let (out, _, _) = self.current.as_mut().expect("start_file comes first");
        out.write_all(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> StreamFile {
        StreamFile {
            name: name.into(),
            mime: "application/octet-stream".into(),
            size: 0,
        }
    }

    #[test]
    fn folder_sink_saves_under_free_names_and_discards_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "taken").unwrap();

        let mut sink = FolderSink::new(dir.path(), &[file("../a.txt"), file("b")]).unwrap();
        sink.start_file(0).unwrap();
        sink.write(b"one").unwrap();
        sink.start_file(1).unwrap();
        let saved = sink.finish().unwrap();
        assert_eq!(saved, [dir.path().join("a (1).txt"), dir.path().join("b")]);
        assert_eq!(std::fs::read(&saved[0]).unwrap(), b"one");
        assert_eq!(std::fs::read(&saved[1]).unwrap(), b"");

        let mut sink = FolderSink::new(dir.path(), &[file("c")]).unwrap();
        sink.start_file(0).unwrap();
        sink.write(b"half").unwrap();
        sink.discard();
        assert!(!dir.path().join("c").exists());
        assert!(!dir.path().join("c.part").exists());
    }

    #[test]
    fn labels() {
        assert_eq!(label(["disk.iso"].into_iter()), "disk.iso");
        assert_eq!(label(["a", "b", "c"].into_iter()), "3 files");
    }
}
