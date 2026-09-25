//! Files too big for one clip: sent and received in chunks, straight from
//! and to disk, with a progress line on stderr.

use std::fs::File;
use std::io::{BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use yacs_client::{Client, LocalFiles, Sink};
use yacs_core::api::{ClipMeta, ServerConfig};
use yacs_core::{Clip, ClipItem, Stream, StreamFile};

use crate::human_size;

pub async fn send(
    client: &Client,
    config: &ServerConfig,
    path: PathBuf,
    file: StreamFile,
    device_name: String,
    ttl: Option<Duration>,
) -> Result<(ClipMeta, String)> {
    let Some(chunked) = &config.chunked else {
        bail!(
            "{} is {}, but this relay takes up to {} per clip; relays from 0.3.0 take files of any size",
            file.name,
            human_size(file.size),
            human_size(config.inline_file_limit())
        );
    };
    let label = format!("{} ({})", file.name, human_size(file.size));
    let size = file.size;
    let mut stream = Stream::new(vec![file])?;
    stream.chunk_size = chunked.chunk_size();
    let clip = Clip {
        created_at_ms: crate::now_ms() as i64,
        device_name,
        items: vec![ClipItem::Stream(stream)],
    };
    let line = ProgressLine::new(format!("sending {label}"));
    let sent = client
        .push_stream(
            &clip,
            LocalFiles::new(vec![(path, size)]),
            ttl,
            &|done, total| line.update(done, total),
            // Frees the relay's quota right away instead of in a day.
            async {
                let _ = tokio::signal::ctrl_c().await;
            },
        )
        .await;
    line.clear();
    Ok((sent?, label))
}

/// Like `write_files` for inline files: into `output` (a folder keeps the
/// names), or one file to a piped stdout.
pub async fn recv(client: &Client, id: &str, stream: &Stream, output: Option<&Path>) -> Result<()> {
    let files = &stream.files;
    let listing = files
        .iter()
        .map(|f| format!("{} ({})", f.name, human_size(f.size)))
        .collect::<Vec<_>>()
        .join(", ");
    let targets = match (output, &files[..]) {
        (Some(dir), _) if dir.is_dir() => {
            let paths: Vec<PathBuf> = files.iter().map(|f| dir.join(f.safe_name())).collect();
            if let Some(taken) = paths.iter().find(|p| p.exists()) {
                bail!("{} already exists", taken.display());
            }
            Some(paths)
        }
        (Some(path), [_]) => Some(vec![path.to_owned()]),
        (Some(path), _) => bail!(
            "clip has {} files; --output must be an existing folder, not {}",
            files.len(),
            path.display()
        ),
        (None, [_]) if !std::io::stdout().is_terminal() => None,
        (None, _) => bail!("clip holds {listing}; use --output FOLDER (or FILE)"),
    };

    let line = ProgressLine::new(format!("receiving {listing}"));
    let saved = targets.is_some();
    let mut sink = CliSink {
        targets: targets.unwrap_or_default(),
        current: None,
        done: Vec::new(),
    };
    let result = client
        .fetch_stream(id, stream, &mut sink, &|done, total| {
            line.update(done, total)
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|()| sink.finish());
    line.clear();
    if let Err(e) = result {
        sink.discard();
        return Err(e);
    }
    if saved {
        for path in &sink.done {
            eprintln!("saved {}", path.display());
        }
    }
    Ok(())
}

/// Writes each file to `{target}.part`, renamed once it's complete. No
/// targets means stdout.
struct CliSink {
    targets: Vec<PathBuf>,
    current: Option<(Box<dyn Write + Send>, Option<PathBuf>)>,
    done: Vec<PathBuf>,
}

impl CliSink {
    fn part(target: &Path) -> PathBuf {
        let mut name = target.as_os_str().to_owned();
        name.push(".part");
        PathBuf::from(name)
    }

    fn finish(&mut self) -> Result<()> {
        let Some((mut out, target)) = self.current.take() else {
            return Ok(());
        };
        out.flush()?;
        drop(out);
        if let Some(target) = target {
            std::fs::rename(Self::part(&target), &target)
                .with_context(|| format!("writing {}", target.display()))?;
            self.done.push(target);
        }
        Ok(())
    }

    fn discard(&mut self) {
        if let Some((_, Some(target))) = self.current.take() {
            let _ = std::fs::remove_file(Self::part(&target));
        }
    }
}

impl Sink for CliSink {
    fn start_file(&mut self, index: usize) -> std::io::Result<()> {
        self.finish().map_err(std::io::Error::other)?;
        let out: (Box<dyn Write + Send>, Option<PathBuf>) = match self.targets.get(index) {
            Some(target) => {
                let part = Self::part(target);
                let file = File::create(&part).map_err(|e| {
                    std::io::Error::new(e.kind(), format!("creating {}: {e}", part.display()))
                })?;
                (Box::new(BufWriter::new(file)), Some(target.clone()))
            }
            None => (Box::new(BufWriter::new(std::io::stdout())), None),
        };
        self.current = Some(out);
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        let (out, _) = self.current.as_mut().expect("start_file comes first");
        out.write_all(data)
    }
}

/// `sending disk.iso (4.7 GB)  42%  2.0 GB`, redrawn in place a few times a
/// second. Nothing when stderr isn't a terminal.
struct ProgressLine {
    label: String,
    last: Mutex<Option<Instant>>,
    shown: bool,
}

impl ProgressLine {
    fn new(label: String) -> Self {
        Self {
            label,
            last: Mutex::new(None),
            shown: std::io::stderr().is_terminal(),
        }
    }

    fn update(&self, done: u64, total: u64) {
        if !self.shown {
            return;
        }
        let mut last = self.last.lock().expect("progress lock poisoned");
        if last.is_some_and(|t| t.elapsed() < Duration::from_millis(200)) && done < total {
            return;
        }
        *last = Some(Instant::now());
        let percent = (done * 100).checked_div(total).unwrap_or(100);
        eprint!("\r\x1b[2K{}  {percent}%  {}", self.label, human_size(done));
    }

    fn clear(&self) {
        if self.shown {
            eprint!("\r\x1b[2K");
        }
    }
}
