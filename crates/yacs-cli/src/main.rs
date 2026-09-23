use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use yacs_client::Client;
use yacs_core::api::ClipMeta;
use yacs_core::{Clip, ClipItem, Image, Pairing, Payload};

/// Share your clipboard through a self-hosted YACS relay.
#[derive(Parser)]
#[command(name = "yacs", version)]
struct Cli {
    /// Relay URL, e.g. https://clip.example.com
    #[arg(long, env = "YACS_SERVER", global = true)]
    server: Option<String>,

    /// Access token, if the relay requires one.
    #[arg(long, env = "YACS_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,

    /// Pairing phrase. Prompted for if not set; prefer the env var over the flag
    /// so it doesn't end up in your shell history.
    #[arg(long, env = "YACS_PHRASE", hide_env_values = true, global = true)]
    phrase: Option<String>,

    /// Name shown to your other devices.
    #[arg(long, env = "YACS_DEVICE_NAME", global = true)]
    device_name: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send text (argument or stdin) or an image.
    Send {
        /// Text to send. Reads stdin if neither this nor --image is given.
        text: Option<String>,
        /// Send an image file instead (png, jpg, gif, webp).
        #[arg(long, conflicts_with = "text")]
        image: Option<PathBuf>,
        /// How long the server keeps it, e.g. 5m, 1h, 24h. Defaults to the server's default.
        #[arg(long, value_parser = humantime::parse_duration)]
        ttl: Option<Duration>,
    },
    /// List the channel's clips, newest first.
    List,
    /// Print the newest clip, or the one with the given id.
    Recv {
        id: Option<String>,
        /// Write to a file instead of stdout. Required for image clips.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Delete one clip for all devices.
    Delete { id: String },
    /// Delete the channel's whole history.
    Clear,
    /// Show the relay's limits.
    Info,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let Some(server) = cli.server else {
        bail!("no relay configured: pass --server or set YACS_SERVER");
    };
    let phrase = match cli.phrase {
        Some(phrase) => phrase,
        None => rpassword::prompt_password("Pairing phrase: ").context("reading pairing phrase")?,
    };
    let pairing = Pairing::from_phrase(&phrase)?;
    let client = Client::new(&server, cli.token, pairing)?;

    match cli.command {
        Command::Send { text, image, ttl } => {
            let item = match (text, image) {
                (Some(text), _) => ClipItem::Text(text),
                (None, Some(path)) => ClipItem::Image(read_image(&path)?),
                (None, None) => {
                    let mut text = String::new();
                    std::io::stdin()
                        .read_to_string(&mut text)
                        .context("reading stdin")?;
                    ClipItem::Text(text)
                }
            };
            let device_name = cli
                .device_name
                .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned());
            let clip = Clip {
                created_at_ms: now_ms() as i64,
                device_name,
                items: vec![item],
            };
            let meta = client.push(&Payload::Clip(clip), ttl).await?;
            eprintln!(
                "sent {} (expires in {})",
                meta.id,
                human_duration(meta.expires_at_ms.saturating_sub(now_ms()))
            );
        }
        Command::List => print_list(&client.list().await?),
        Command::Recv { id, output } => {
            let found = match &id {
                Some(id) => client.get(id).await?,
                None => client.latest().await?,
            };
            let Some((_, Payload::Clip(clip))) = found else {
                bail!("no clip found");
            };
            write_clip(&clip, output.as_deref())?;
        }
        Command::Delete { id } => {
            if !client.delete(&id).await? {
                bail!("no clip with id {id}");
            }
        }
        Command::Clear => client.clear().await?,
        Command::Info => {
            let c = client.config().await?;
            println!("default ttl  {}", human_duration(c.default_ttl_secs * 1000));
            println!("max ttl      {}", human_duration(c.max_ttl_secs * 1000));
            println!("max size     {}", human_size(c.max_size_bytes));
            println!("history      {} clips", c.max_clips);
        }
    }
    Ok(())
}

fn read_image(path: &Path) -> Result<Image> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let mime = match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => bail!("unsupported image type (use png, jpg, gif or webp)"),
    };
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Image {
        mime: mime.into(),
        data,
    })
}

/// Text formats go to stdout (plain text preferred); images need `--output`.
fn write_clip(clip: &Clip, output: Option<&Path>) -> Result<()> {
    let image = clip.items.iter().find_map(|i| match i {
        ClipItem::Image(image) => Some(image),
        _ => None,
    });
    let text = [0, 1, 2].iter().find_map(|&rank| {
        clip.items.iter().find_map(|i| match (rank, i) {
            (0, ClipItem::Text(t)) | (1, ClipItem::Html(t)) | (2, ClipItem::Rtf(t)) => Some(t),
            _ => None,
        })
    });

    match (output, image, text) {
        (Some(path), Some(image), _) => write_file(path, &image.data),
        (Some(path), None, Some(text)) => write_file(path, text.as_bytes()),
        (None, _, Some(text)) => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(text.as_bytes())?;
            if stdout.is_terminal() && !text.ends_with('\n') {
                stdout.write_all(b"\n")?;
            }
            Ok(())
        }
        (None, Some(image), None) => bail!("clip is an image ({}); use --output FILE", image.mime),
        (_, None, None) => bail!("clip has no content this CLI can show"),
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn print_list(clips: &[ClipMeta]) {
    if clips.is_empty() {
        eprintln!("no clips");
        return;
    }
    let now = now_ms();
    println!(
        "{:<26}  {:>8}  {:>10}  {:>9}",
        "ID", "AGE", "EXPIRES IN", "SIZE"
    );
    for c in clips {
        println!(
            "{:<26}  {:>8}  {:>10}  {:>9}",
            c.id,
            human_duration(now.saturating_sub(c.created_at_ms)),
            human_duration(c.expires_at_ms.saturating_sub(now)),
            human_size(c.size),
        );
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after 1970")
        .as_millis() as u64
}

/// Coarse, single-unit durations for tables: `45s`, `13m`, `2h`, `3d`.
fn human_duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1000 => format!("{bytes} B"),
        1000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1e3),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}
