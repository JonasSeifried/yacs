use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use yacs_client::spaces::{
    Accepted, DEFAULT_SPACE_NAME, Link, Space, Spaces, clean_name, invite_url, normalize_relay,
};
use yacs_client::{Client, CodeOutcome, join_with_code, stream_of};
use yacs_core::api::{ClipMeta, ServerConfig};

use crate::content::Content;
use yacs_core::{Clip, ClipItem, Pairing, Payload};

mod big;
mod config;
mod content;
mod relay;
mod update;

const EXAMPLES: &str = "\
Examples:
  yacs join                          once: paste an invite link from the desktop app
  yacs space new --relay URL         or start a new space, then `yacs invite` other devices
  yacs send ~/.ssh/id_ed25519.pub    a text file arrives as text, an image as an image
  yacs send report.pdf               other files arrive as files
  yacs send disk.iso                 big files too, in chunks, with a progress line
  cat notes.txt | yacs send
  yacs send --text \"hello\"
  yacs recv > clip.txt
  yacs recv -o ~/Downloads           a file clip keeps its name in that folder
  yacs update                        get the newest version
  yacs relay update                  on the relay's machine: update the relay (Docker)";

/// Share your clipboard with your other devices through a YACS relay.
#[derive(Parser)]
#[command(name = "yacs", version, after_help = EXAMPLES)]
struct Cli {
    /// Relay URL, e.g. https://clip.example.com. Not needed after `yacs join`.
    /// The older names, `--server` and YACS_SERVER, work too.
    #[arg(
        long = "relay",
        value_name = "URL",
        visible_alias = "server",
        env = "YACS_RELAY",
        global = true
    )]
    server: Option<String>,

    /// Access token, if the relay requires one.
    #[arg(long, env = "YACS_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,

    /// A space's secret from `yacs space export`, instead of the saved space.
    /// Prefer the env var over the flag so it doesn't end up in your shell history.
    #[arg(long, env = "YACS_SPACE", hide_env_values = true, global = true)]
    space_secret: Option<String>,

    /// Name shown to your other devices. Defaults to the host name.
    #[arg(long, env = "YACS_DEVICE_NAME", global = true)]
    device_name: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Join a space once, so other commands need no flags.
    ///
    /// Asks for an invite link (on a computer in the space: Settings → Invite
    /// a device… → Copy link, or `yacs invite`), checks it with the relay and
    /// saves the space, readable only by you. Replaces the space saved before.
    #[command(alias = "pair")]
    Join {
        /// What to call the space here. Defaults to the name in the link.
        #[arg(long)]
        name: Option<String>,
    },
    /// Leave the space: forget its key on this machine. Its clips stay for its other devices.
    #[command(alias = "unpair")]
    Leave,
    /// List the spaces this machine is in.
    Spaces,
    /// Invite another device: prints a link that works once, within 24
    /// hours, and in a terminal also shows a code to type on the other device,
    /// waiting until it's used.
    Invite {
        /// Only the code, and wait for it even when not in a terminal.
        #[arg(long)]
        code: bool,
    },
    /// Start, rename or export a space.
    Space {
        #[command(subcommand)]
        command: SpaceCommand,
    },
    /// Send a file, text, or stdin.
    Send {
        /// File to send; `-` or nothing reads stdin. Text files arrive as
        /// text, images (png, jpg, gif, webp) as images, other files as files.
        file: Option<PathBuf>,
        /// Send this text instead of a file.
        #[arg(short, long, conflicts_with = "file")]
        text: Option<String>,
        /// Send the file as a file, even if it's text or an image.
        #[arg(short = 'f', long, requires = "file")]
        as_file: bool,
        /// How long the relay keeps it, e.g. 5m, 1h, 24h. Defaults to the relay's default.
        #[arg(long, value_parser = humantime::parse_duration)]
        ttl: Option<Duration>,
    },
    /// List the space's clips, newest first.
    List,
    /// Print the newest clip, or the one with the given id.
    Recv {
        id: Option<String>,
        /// Write to a file instead of stdout. Required for images, and for
        /// files unless stdout is piped. An existing folder keeps the names
        /// of the clip's files.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Delete one clip for all devices.
    Delete { id: String },
    /// Delete the space's whole history.
    Clear,
    /// Show the relay and its limits.
    Info,
    /// Update yacs to the latest release (checked against the release signature).
    Update {
        /// Only say whether there's a newer version.
        #[arg(long)]
        check: bool,
    },
    /// Manage a relay running on this machine.
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
}

#[derive(Subcommand)]
enum SpaceCommand {
    /// Start a new space on the relay given with --relay, and save it.
    /// Replaces the space saved before.
    New {
        /// What to call the space here.
        #[arg(long, default_value = DEFAULT_SPACE_NAME)]
        name: String,
    },
    /// Rename the space on this machine. Other devices keep their own name for it.
    Rename { name: String },
    /// Print the space's secret, for YACS_SPACE in scripts and containers
    /// (with YACS_RELAY). Anyone with it can read and send the space's clips.
    Export,
}

#[derive(Subcommand)]
enum RelayCommand {
    /// Pull the newest relay image and restart the relay with it, using the
    /// compose files it was started with. Run it on the relay's machine.
    Update,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut cli = Cli::parse();
    if cli.server.is_none() {
        cli.server = std::env::var("YACS_SERVER").ok().filter(|s| !s.is_empty());
    }
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    match &cli.command {
        Command::Join { name } => return join(&cli, name.as_deref()).await,
        Command::Space {
            command: SpaceCommand::New { name },
        } => return new_space(&cli, name).await,
        Command::Update { check } => return update::run(*check).await,
        Command::Relay {
            command: RelayCommand::Update,
        } => return relay::update(),
        Command::Leave | Command::Spaces | Command::Space { .. } => {
            return manage(&cli.command);
        }
        _ => {}
    }
    let (server, client) = connect(&cli)?;
    let device_name = device_name(&cli);

    match cli.command {
        Command::Join { .. }
        | Command::Leave
        | Command::Spaces
        | Command::Space { .. }
        | Command::Update { .. }
        | Command::Relay { .. } => {
            unreachable!("handled above")
        }
        Command::Invite { code } => {
            let saved = config::load(&config::path()?)?;
            let name = saved
                .current()
                .filter(|s| {
                    s.relay == server && s.pairing().ok().as_ref() == Some(client.pairing())
                })
                .map_or(DEFAULT_SPACE_NAME, |s| s.name.as_str());
            if !code {
                let secret = client.invite(name, &device_name).await?;
                println!("{}", invite_url(&server, &secret));
                eprintln!(
                    "Works once, within 24 hours: open it on a phone, or paste it into Settings on a computer or into `yacs join`.\nWhoever opens it first joins \"{name}\", so only send it to the device you mean."
                );
            }
            if code || std::io::stderr().is_terminal() {
                offer_codes(&client, &server, name, &device_name, code).await?;
            }
        }
        Command::Send {
            file,
            text,
            as_file,
            ttl,
        } => {
            let (item, what) = match (text, file) {
                (Some(text), _) => content::from_text(text),
                (None, Some(path)) => {
                    let config = client.config().await?;
                    match content::from_file(&path, as_file, config.inline_file_limit())? {
                        Content::Inline(item, what) => (item, what),
                        Content::Big(path, file) => {
                            let (meta, what) =
                                big::send(&client, &config, path, file, device_name, ttl).await?;
                            print_sent(&what, &meta);
                            return Ok(());
                        }
                    }
                }
                (None, None) => content::from_stdin()?,
            };
            let clip = Clip {
                created_at_ms: now_ms() as i64,
                device_name,
                items: vec![item],
            };
            let meta = client.push(&Payload::Clip(clip), ttl).await?;
            print_sent(&what, &meta);
        }
        Command::List => print_list(&client.list().await?),
        Command::Recv { id, output } => {
            let found = match &id {
                Some(id) => client.get(id).await?,
                None => client.latest().await?,
            };
            let Some((meta, Payload::Clip(clip))) = found else {
                bail!("no clip found");
            };
            match stream_of(&clip) {
                Some(stream) => big::recv(&client, &meta.id, stream, output.as_deref()).await?,
                None => write_clip(&clip, output.as_deref())?,
            }
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
            println!("max clip     {}", human_size(c.max_size_bytes));
            println!("history      {} clips", c.max_clips);
            match &c.chunked {
                Some(chunked) => println!(
                    "big files    yes, in chunks of {}",
                    human_size(chunked.chunk_size().into())
                ),
                None => println!("big files    no (relays from 0.3.0 take them)"),
            }
        }
    }
    Ok(())
}

/// Shows codes until one is used (or Ctrl+C): a new one after a wrong guess
/// or when one expires.
async fn offer_codes(
    client: &Client,
    server: &str,
    space_name: &str,
    device_name: &str,
    only_code: bool,
) -> Result<()> {
    let mut first = true;
    loop {
        let offer = client.offer_code().await?;
        let nameplate = offer.nameplate();
        let code = offer.code();
        if only_code {
            println!("{code}");
        } else {
            eprintln!(
                "{}Or type this code on the other device: {code}",
                if first { "\n" } else { "" }
            );
        }
        if first {
            eprintln!(
                "It works while this runs (Ctrl+C stops it). The other device also needs the relay: {server}"
            );
            first = false;
        }
        let outcome = tokio::select! {
            outcome = client.complete_code(offer, space_name, device_name) => outcome?,
            _ = tokio::signal::ctrl_c() => {
                client.close_code(nameplate).await;
                return Ok(());
            }
        };
        match outcome {
            CodeOutcome::Joined { device } => {
                eprintln!("{device} joined \"{space_name}\".");
                return Ok(());
            }
            CodeOutcome::WrongCode => eprintln!("Someone typed a wrong code, so here's a new one."),
            CodeOutcome::Expired => {}
        }
    }
}

/// Shown to other devices. Defaults to the host name.
fn device_name(cli: &Cli) -> String {
    cli.device_name
        .clone()
        .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned())
}

fn print_sent(what: &str, meta: &ClipMeta) {
    eprintln!(
        "sent {what}, expires in {}",
        human_duration(meta.expires_at_ms.saturating_sub(meta.created_at_ms))
    );
}

const NOT_IN_A_SPACE: &str =
    "not in a space: run `yacs join` with an invite link or code, or set YACS_RELAY and YACS_SPACE";

/// Flags and env vars win over the saved space, which is only used for its
/// own relay: a token never goes to a relay it wasn't saved for.
fn connect(cli: &Cli) -> Result<(String, Client)> {
    let flag_server = cli.server.as_deref().map(normalize_relay);
    let saved = config::load(&config::path()?)?;
    let space = saved.current();
    let (server, pairing) = match (&cli.space_secret, space) {
        (Some(secret), _) => {
            let pairing = Pairing::from_secret(secret.trim())
                .context("YACS_SPACE isn't a space's secret; print one with `yacs space export`")?;
            let server = flag_server.or_else(|| space.map(|s| s.relay.clone()));
            (server.context(NOT_IN_A_SPACE)?, pairing)
        }
        (None, Some(space)) => {
            if let Some(url) = flag_server.filter(|url| *url != space.relay) {
                bail!(
                    "this machine's space \"{}\" is on {}, not {url}; set YACS_SPACE to use another space",
                    space.name,
                    space.relay
                );
            }
            (space.relay.clone(), space.pairing()?)
        }
        (None, None) if std::env::var_os("YACS_PHRASE").is_some() => bail!(
            "YACS_PHRASE is no longer used: set YACS_SPACE to what `yacs space export` prints on a machine in the space"
        ),
        (None, None) => bail!(NOT_IN_A_SPACE),
    };
    let token = cli
        .token
        .clone()
        .or_else(|| saved.token(&server).map(str::to_owned));
    let client = Client::new(&server, token, pairing)?;
    Ok((server, client))
}

async fn join(cli: &Cli, name: Option<&str>) -> Result<()> {
    if std::io::stdin().is_terminal() {
        eprintln!(
            "Paste an invite link or type the code (on a computer in the space: Settings → Invite a device…,\nor `yacs invite`). It isn't shown."
        );
    }
    let input = prompt_secret("Invite link or code: ")?;
    let joining = if yacs_core::looks_like_code(&input) {
        let code: yacs_core::Code = input.parse()?;
        let relay = match &cli.server {
            Some(server) => normalize_relay(server),
            None => match config::load(&config::path()?)?.current() {
                Some(space) => space.relay.clone(),
                None => server_or_prompt(cli)?,
            },
        };
        let invite = join_with_code(&relay, &code, &device_name(cli)).await?;
        Accepted {
            relay,
            pairing: invite.pairing(),
            token: invite.token.clone(),
            name: clean_name(&invite.space_name),
            inviter: clean_name(&invite.inviter),
        }
    } else {
        let link = Link::parse(&input).map_err(|e| {
            anyhow::anyhow!(
                "{e}; copy one on a computer in the space from Settings → Invite a device…, or run `yacs invite` there"
            )
        })?;
        link.accept().await?
    };
    let name = name
        .or(joining.name.as_deref())
        .unwrap_or(DEFAULT_SPACE_NAME);
    let token = cli.token.clone().or(joining.token);
    let (token, config) = check(&joining.relay, token, &joining.pairing).await?;
    let space = Space::new(name, &joining.relay, &joining.pairing);
    let path = save_current(space.clone(), token)?;
    let from = joining
        .inviter
        .map(|device| format!(", invited by {device}"))
        .unwrap_or_default();
    eprintln!(
        "Joined \"{}\" on {} (relay {}{from}).",
        space.name,
        space.relay,
        relay_version(&config)
    );
    eprintln!("Saved to {}; `yacs leave` forgets it.", path.display());
    Ok(())
}

async fn new_space(cli: &Cli, name: &str) -> Result<()> {
    let server = server_or_prompt(cli)?;
    let pairing = Pairing::generate()?;
    let (token, config) = check(&server, cli.token.clone(), &pairing).await?;
    let space = Space::new(name, &server, &pairing);
    let path = save_current(space.clone(), token)?;
    eprintln!(
        "Started \"{}\" on {} (relay {}).",
        space.name,
        space.relay,
        relay_version(&config)
    );
    eprintln!(
        "Saved to {}. Add other devices with the link from `yacs invite`.",
        path.display()
    );
    Ok(())
}

/// Proves the relay is reachable and takes the token, asking for one if it's
/// missing. Returns the token that worked.
async fn check(
    server: &str,
    mut token: Option<String>,
    pairing: &Pairing,
) -> Result<(Option<String>, ServerConfig)> {
    loop {
        let client = Client::new(server, token.clone(), pairing.clone())?;
        match client.config().await {
            Ok(config) => return Ok((token, config)),
            Err(yacs_client::Error::Unauthorized)
                if token.is_none() && std::io::stdin().is_terminal() =>
            {
                let entered = prompt_secret("The relay needs its access token: ")?;
                if entered.is_empty() {
                    bail!("the relay needs an access token");
                }
                token = Some(entered);
            }
            Err(e) => return Err(e).with_context(|| format!("couldn't reach {server}")),
        }
    }
}

fn save_current(space: Space, token: Option<String>) -> Result<PathBuf> {
    let path = config::path()?;
    let mut saved = config::load(&path).unwrap_or_default();
    if let Some(old) = saved
        .current()
        .filter(|old| old.pairing().ok() != space.pairing().ok())
    {
        eprintln!("This replaces \"{}\" on {}.", old.name, old.relay);
    }
    saved.set_current(space, token);
    config::save(&path, &saved)?;
    Ok(path)
}

fn relay_version(config: &ServerConfig) -> &str {
    config.version.as_deref().unwrap_or("before 0.2.0")
}

/// The commands that only read or change the saved spaces.
fn manage(command: &Command) -> Result<()> {
    let path = config::path()?;
    let mut saved = config::load(&path)?;
    let current = |saved: &Spaces| saved.current().cloned().context(NOT_IN_A_SPACE);
    match command {
        Command::Leave => match saved.leave_current() {
            Some(space) => {
                config::save_or_remove(&path, &saved)?;
                eprintln!(
                    "Left \"{}\". Its clips stay on {} for its other devices.",
                    space.name, space.relay
                );
            }
            None => eprintln!("Not in a space."),
        },
        Command::Spaces => {
            if saved.spaces.is_empty() {
                eprintln!("Not in a space. Join one with `yacs join`.");
            }
            for space in &saved.spaces {
                println!("{}\t{}", space.name, space.relay);
            }
        }
        Command::Space {
            command: SpaceCommand::Rename { name },
        } => {
            let old = current(&saved)?.name;
            let Some(name) = saved.rename_current(name) else {
                bail!("the name can't be empty");
            };
            config::save(&path, &saved)?;
            eprintln!("Renamed \"{old}\" to \"{name}\" on this machine.");
        }
        Command::Space {
            command: SpaceCommand::Export,
        } => {
            let space = current(&saved)?;
            println!("{}", space.secret());
            eprintln!(
                "Use it as YACS_SPACE, with YACS_RELAY={}. Anyone with it can read and send the space's clips.",
                space.relay
            );
        }
        _ => unreachable!("not a command that manages spaces"),
    }
    Ok(())
}

fn server_or_prompt(cli: &Cli) -> Result<String> {
    let server = match &cli.server {
        Some(server) => server.clone(),
        None => prompt_line("Relay URL: ")?,
    };
    if server.trim().is_empty() {
        bail!("no relay URL given");
    }
    Ok(normalize_relay(&server))
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
/// Files go into `--output` (a folder keeps their names), or one file to a
/// piped stdout.
fn write_clip(clip: &Clip, output: Option<&Path>) -> Result<()> {
    let files: Vec<&yacs_core::File> = clip
        .items
        .iter()
        .filter_map(|i| match i {
            ClipItem::File(file) => Some(file),
            _ => None,
        })
        .collect();
    if !files.is_empty() {
        return write_files(&files, output);
    }

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

fn write_files(files: &[&yacs_core::File], output: Option<&Path>) -> Result<()> {
    let listing = files
        .iter()
        .map(|f| format!("{} ({})", f.name, human_size(f.data.len() as u64)))
        .collect::<Vec<_>>()
        .join(", ");
    match (output, files) {
        (Some(dir), _) if dir.is_dir() => {
            for file in files {
                let path = dir.join(file.safe_name());
                if path.exists() {
                    bail!("{} already exists", path.display());
                }
                write_file(&path, &file.data)?;
                eprintln!("saved {}", path.display());
            }
            Ok(())
        }
        (Some(path), [file]) => write_file(path, &file.data),
        (Some(path), _) => bail!(
            "clip has {} files; --output must be an existing folder, not {}",
            files.len(),
            path.display()
        ),
        (None, [file]) if !std::io::stdout().is_terminal() => {
            Ok(std::io::stdout().lock().write_all(&file.data)?)
        }
        (None, _) => bail!("clip holds {listing}; use --output FOLDER (or FILE)"),
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
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}
