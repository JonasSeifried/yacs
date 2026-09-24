use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use yacs_client::Client;
use yacs_core::api::ClipMeta;
use yacs_core::{Clip, ClipItem, Pairing, Payload};

mod config;
mod content;
mod update;

const EXAMPLES: &str = "\
Examples:
  yacs pair                          pair once; paste the link from the desktop app
  yacs send ~/.ssh/id_ed25519.pub    a text file arrives as text, an image as an image
  cat notes.txt | yacs send
  yacs send --text \"hello\"
  yacs recv > clip.txt
  yacs update                        get the newest version";

/// Share your clipboard through a self-hosted YACS relay.
#[derive(Parser)]
#[command(name = "yacs", version, after_help = EXAMPLES)]
struct Cli {
    /// Relay URL, e.g. https://clip.example.com. Not needed after `yacs pair`.
    #[arg(long, env = "YACS_SERVER", global = true)]
    server: Option<String>,

    /// Access token, if the relay requires one.
    #[arg(long, env = "YACS_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,

    /// Pairing phrase, instead of the saved pairing. Prefer the env var over
    /// the flag so it doesn't end up in your shell history.
    #[arg(long, env = "YACS_PHRASE", hide_env_values = true, global = true)]
    phrase: Option<String>,

    /// Name shown to your other devices. Defaults to the host name.
    #[arg(long, env = "YACS_DEVICE_NAME", global = true)]
    device_name: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pair this machine once, so other commands need no flags.
    ///
    /// Asks for the pairing link (on a paired computer: Settings → Pair
    /// another device… → Copy link) or the phrase, checks it with the relay and saves
    /// it, readable only by you.
    Pair,
    /// Forget the saved pairing. Clips on the relay stay.
    Unpair,
    /// Send a file (text or image), text, or stdin.
    Send {
        /// File to send; `-` or nothing reads stdin. Text files arrive as
        /// text, images (png, jpg, gif, webp) as images.
        file: Option<PathBuf>,
        /// Send this text instead of a file.
        #[arg(short, long, conflicts_with = "file")]
        text: Option<String>,
        /// How long the relay keeps it, e.g. 5m, 1h, 24h. Defaults to the relay's default.
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
    /// Show the relay and its limits.
    Info,
    /// Update yacs to the latest release (checked against the release signature).
    Update {
        /// Only say whether there's a newer version.
        #[arg(long)]
        check: bool,
    },
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
    match cli.command {
        Command::Pair => return pair(&cli).await,
        Command::Update { check } => return update::run(check).await,
        Command::Unpair => {
            let path = config::path()?;
            if config::remove(&path)? {
                eprintln!("Forgot the pairing ({}).", path.display());
            } else {
                eprintln!("Not paired.");
            }
            return Ok(());
        }
        _ => {}
    }
    let (server, client) = connect(&cli)?;

    match cli.command {
        Command::Pair | Command::Unpair | Command::Update { .. } => unreachable!("handled above"),
        Command::Send { file, text, ttl } => {
            let (item, what) = match (text, file) {
                (Some(text), _) => content::from_text(text),
                (None, Some(path)) => content::from_file(&path)?,
                (None, None) => content::from_stdin()?,
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
                "sent {what}, expires in {}",
                human_duration(meta.expires_at_ms.saturating_sub(meta.created_at_ms))
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
            let version = c.version.as_deref().unwrap_or("before 0.2.0");
            println!("relay        {server} ({version})");
            println!("default ttl  {}", human_duration(c.default_ttl_secs * 1000));
            println!("max ttl      {}", human_duration(c.max_ttl_secs * 1000));
            println!("max size     {}", human_size(c.max_size_bytes));
            println!("history      {} clips", c.max_clips);
        }
    }
    Ok(())
}

const NOT_PAIRED: &str = "not paired: run `yacs pair` (or set YACS_SERVER and YACS_PHRASE)";

/// Flags and env vars win over the saved pairing, which is only used for its
/// own relay: a token never goes to a relay it wasn't saved for.
fn connect(cli: &Cli) -> Result<(String, Client)> {
    let flag_server = cli.server.as_deref().map(normalize_server);
    let saved = match (&flag_server, &cli.phrase) {
        (Some(_), Some(_)) => None,
        _ => config::load(&config::path()?)?,
    }
    .filter(|s| flag_server.as_ref().is_none_or(|url| *url == s.server));

    let Some(server) = flag_server.or_else(|| saved.as_ref().map(|s| s.server.clone())) else {
        bail!(NOT_PAIRED);
    };
    let token = cli
        .token
        .clone()
        .or_else(|| saved.as_ref().and_then(|s| s.token.clone()));
    let pairing = match (&cli.phrase, &saved) {
        (Some(phrase), _) => Pairing::from_phrase(phrase)?,
        (None, Some(saved)) => saved.pairing()?,
        (None, None) if std::io::stdin().is_terminal() => {
            Pairing::from_phrase(&prompt_secret("Pairing phrase: ")?)?
        }
        (None, None) => bail!(NOT_PAIRED),
    };
    let client = Client::new(&server, token, pairing)?;
    Ok((server, client))
}

async fn pair(cli: &Cli) -> Result<()> {
    let path = config::path()?;
    let (server, mut token, pairing) = match &cli.phrase {
        Some(phrase) => (
            server_or_prompt(cli)?,
            cli.token.clone(),
            Pairing::from_phrase(phrase)?,
        ),
        None => {
            if std::io::stdin().is_terminal() {
                eprintln!(
                    "Paste the pairing link (on a paired computer: Settings → Pair another device… → Copy link),\nor type the pairing phrase. Neither is shown."
                );
            }
            let input = prompt_secret("Link or phrase: ")?;
            match PairLink::parse(&input)? {
                Some(link) => (link.server, cli.token.clone().or(link.token), link.pairing),
                None => (
                    server_or_prompt(cli)?,
                    cli.token.clone(),
                    Pairing::from_phrase(&input)?,
                ),
            }
        }
    };

    let config = loop {
        let client = Client::new(&server, token.clone(), pairing.clone())?;
        match client.config().await {
            Ok(config) => break config,
            Err(yacs_client::Error::Unauthorized)
                if token.is_none() && std::io::stdin().is_terminal() =>
            {
                let entered = prompt_secret("The relay needs its access token: ")?;
                if entered.is_empty() {
                    bail!("the relay needs an access token");
                }
                token = Some(entered);
            }
            Err(e) => return Err(e).with_context(|| format!("couldn't pair with {server}")),
        }
    };

    config::save(&path, &config::Saved::new(server.clone(), token, &pairing))?;
    let version = config.version.as_deref().unwrap_or("before 0.2.0");
    eprintln!("Paired with {server} (relay {version}).");
    eprintln!("Saved to {}; `yacs unpair` forgets it.", path.display());
    Ok(())
}

/// `https://relay/#pair=v1.<channel>.<key>&token=…`, from the desktop app.
struct PairLink {
    server: String,
    token: Option<String>,
    pairing: Pairing,
}

impl PairLink {
    /// `None` if `input` isn't a link at all (so it's a phrase).
    fn parse(input: &str) -> Result<Option<Self>> {
        let input = input.trim();
        if !(input.starts_with("https://") || input.starts_with("http://")) {
            return Ok(None);
        }
        let mut url = url::Url::parse(input).context("that link isn't a valid URL")?;
        let fragment = url.fragment().unwrap_or_default().to_owned();
        let mut secret = None;
        let mut token = None;
        for (key, value) in url::form_urlencoded::parse(fragment.as_bytes()) {
            match &*key {
                "pair" => secret = Some(value.into_owned()),
                "token" if !value.is_empty() => token = Some(value.into_owned()),
                _ => {}
            }
        }
        let Some(secret) = secret else {
            bail!("that link has no pairing in it; copy it from Settings → Pair another device…");
        };
        let pairing =
            Pairing::from_secret(&secret).context("the pairing in that link is damaged")?;
        url.set_fragment(None);
        Ok(Some(Self {
            server: normalize_server(url.as_str()),
            token,
            pairing,
        }))
    }
}

fn server_or_prompt(cli: &Cli) -> Result<String> {
    let server = match &cli.server {
        Some(server) => server.clone(),
        None => prompt_line("Relay URL: ")?,
    };
    if server.trim().is_empty() {
        bail!("no relay URL given");
    }
    Ok(normalize_server(&server))
}

fn normalize_server(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

/// Hidden when typed; read as a plain line when piped in.
fn prompt_secret(label: &str) -> Result<String> {
    let value = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(label).context("reading input")?
    } else {
        read_line()?
    };
    Ok(value.trim().to_owned())
}

fn prompt_line(label: &str) -> Result<String> {
    eprint!("{label}");
    std::io::stderr().flush()?;
    Ok(read_line()?.trim().to_owned())
}

fn read_line() -> Result<String> {
    let mut line = String::new();
    if std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading input")?
        == 0
    {
        bail!("no input");
    }
    Ok(line)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pairing_links() {
        let pairing = Pairing {
            channel_id: yacs_core::ChannelId::from_bytes([7; 32]),
            key: yacs_core::ChannelKey::from_bytes([9; 32]),
        };
        let secret = pairing.to_secret();

        let link = PairLink::parse(&format!(
            " https://clip.example.com/#pair={secret}&token=s3cret+%26x\n"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(link.server, "https://clip.example.com");
        assert_eq!(link.token.as_deref(), Some("s3cret &x"));
        assert_eq!(link.pairing, pairing);

        let link = PairLink::parse(&format!("http://10.0.0.2:8080/yacs/#pair={secret}"))
            .unwrap()
            .unwrap();
        assert_eq!(link.server, "http://10.0.0.2:8080/yacs");
        assert_eq!(link.token, None);

        assert!(PairLink::parse("tundra velvet anchor").unwrap().is_none());
        assert!(PairLink::parse("https://clip.example.com/").is_err());
        assert!(PairLink::parse("https://clip.example.com/#pair=v1.nope").is_err());
    }
}
