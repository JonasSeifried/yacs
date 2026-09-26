//! Runs the real `yacs` binary against a real relay on a random local port.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc;

use assert_cmd::cargo::cargo_bin_cmd;
use clap::Parser;
use tempfile::TempDir;
use yacs_server::{Config, SystemClock};

/// The space the env-configured commands use.
fn secret() -> String {
    space(7).to_secret()
}

fn space(n: u8) -> yacs_core::Pairing {
    yacs_core::Pairing::from_root(&[n; 32])
}

/// A relay running on its own thread for the lifetime of the test.
struct Relay {
    url: String,
    _data: TempDir,
    /// Where `yacs join` saves, so tests never touch the real config.
    home: TempDir,
}

fn relay(extra: &[&str]) -> Relay {
    let data = TempDir::new().unwrap();
    let dir = data.path().to_str().unwrap().to_owned();
    let mut args = vec!["yacs-server", "--data-dir", &dir];
    args.extend_from_slice(extra);
    let config = Config::try_parse_from(args).unwrap().validate().unwrap();

    let (tx, rx) = mpsc::channel::<SocketAddr>();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            yacs_server::run(
                listener,
                config,
                Arc::new(SystemClock),
                std::future::pending(),
            )
            .await
            .unwrap();
        });
    });
    Relay {
        url: format!("http://{}", rx.recv().unwrap()),
        _data: data,
        home: TempDir::new().unwrap(),
    }
}

/// Configured through env vars, like a script would.
fn yacs(relay: &Relay, args: &[&str]) -> assert_cmd::Command {
    let mut cmd = saved(relay, args);
    cmd.env("YACS_SERVER", &relay.url)
        .env("YACS_SPACE", secret());
    cmd
}

/// Only what `yacs join` saved.
fn saved(relay: &Relay, args: &[&str]) -> assert_cmd::Command {
    let mut cmd = cargo_bin_cmd!("yacs");
    cmd.env_clear()
        .env("YACS_CONFIG", relay.home.path().join("cli.json"))
        .env("YACS_DEVICE_NAME", "e2e")
        .args(args);
    cmd
}

fn stdout(cmd: &mut assert_cmd::Command) -> String {
    String::from_utf8(cmd.assert().success().get_output().stdout.clone()).unwrap()
}

fn stderr_of_failure(cmd: &mut assert_cmd::Command) -> String {
    String::from_utf8(cmd.assert().failure().get_output().stderr.clone()).unwrap()
}

#[test]
fn text_round_trip_via_argument_and_stdin() {
    let relay = relay(&[]);
    yacs(&relay, &["send", "--text", "hello from the cli\n"])
        .assert()
        .success();
    assert_eq!(stdout(&mut yacs(&relay, &["recv"])), "hello from the cli\n");

    // The final newline of piped input is dropped, like `$(…)` does.
    yacs(&relay, &["send"])
        .write_stdin("piped\nlines\n")
        .assert()
        .success();
    assert_eq!(stdout(&mut yacs(&relay, &["recv"])), "piped\nlines");
}

#[test]
fn sends_text_files_as_text_and_other_files_as_files() {
    let relay = relay(&[]);
    let dir = TempDir::new().unwrap();
    let key = dir.path().join("id_ed25519.pub");
    std::fs::write(&key, "ssh-ed25519 AAAAC3Nza me@server\n").unwrap();

    let sent = yacs(&relay, &["send", key.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let sent = String::from_utf8(sent).unwrap();
    assert!(
        sent.starts_with("sent id_ed25519.pub (31 B), expires in 15m"),
        "{sent}"
    );
    assert_eq!(
        stdout(&mut yacs(&relay, &["recv"])),
        "ssh-ed25519 AAAAC3Nza me@server"
    );

    let binary = dir.path().join("data.bin");
    std::fs::write(&binary, [0, 159, 146, 150]).unwrap();
    yacs(&relay, &["send", binary.to_str().unwrap()])
        .assert()
        .success();
    // Piped stdout gets the bytes; a folder gets the file under its name.
    let piped = yacs(&relay, &["recv"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(piped, [0, 159, 146, 150]);
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    yacs(&relay, &["recv", "-o", out.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        std::fs::read(out.join("data.bin")).unwrap(),
        [0, 159, 146, 150]
    );
    let err = stderr_of_failure(&mut yacs(&relay, &["recv", "-o", out.to_str().unwrap()]));
    assert!(err.contains("already exists"), "{err}");

    // --as-file keeps a text file a file.
    yacs(&relay, &["send", "--as-file", key.to_str().unwrap()])
        .assert()
        .success();
    let as_file = dir.path().join("key-copy.pub");
    yacs(&relay, &["recv", "-o", as_file.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(as_file).unwrap(),
        "ssh-ed25519 AAAAC3Nza me@server\n"
    );

    let err = stderr_of_failure(&mut yacs(&relay, &["send", "hello"]));
    assert!(err.contains("no such file: hello"), "{err}");
    assert!(err.contains("--text"), "{err}");
}

#[test]
fn history_list_recv_by_id_and_delete() {
    let relay = relay(&[]);
    yacs(&relay, &["send", "-t", "first"]).assert().success();
    yacs(&relay, &["send", "-t", "second", "--ttl", "1h"])
        .assert()
        .success();

    let list = stdout(&mut yacs(&relay, &["list"]));
    let ids: Vec<&str> = list
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(ids.len(), 2, "{list}");
    assert_eq!(stdout(&mut yacs(&relay, &["recv", ids[1]])), "first");

    yacs(&relay, &["delete", ids[1]]).assert().success();
    assert!(stderr_of_failure(&mut yacs(&relay, &["recv", ids[1]])).contains("no clip found"));
    assert!(stderr_of_failure(&mut yacs(&relay, &["delete", ids[1]])).contains("no clip with id"));

    yacs(&relay, &["clear"]).assert().success();
    assert!(stderr_of_failure(&mut yacs(&relay, &["recv"])).contains("no clip found"));
}

#[test]
fn image_round_trip_needs_output_file() {
    let relay = relay(&[]);
    let dir = TempDir::new().unwrap();
    let png = dir.path().join("in.png");
    let bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 255, 7];
    std::fs::write(&png, bytes).unwrap();

    yacs(&relay, &["send", png.to_str().unwrap()])
        .assert()
        .success();
    assert!(stderr_of_failure(&mut yacs(&relay, &["recv"])).contains("use --output"));

    let out = dir.path().join("out.png");
    yacs(&relay, &["recv", "-o", out.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(std::fs::read(out).unwrap(), bytes);
}

#[test]
fn another_space_cannot_read_and_sees_its_own_empty_channel() {
    let relay = relay(&[]);
    yacs(&relay, &["send", "-t", "secret"]).assert().success();
    let err = stderr_of_failure(yacs(&relay, &["recv"]).env("YACS_SPACE", space(8).to_secret()));
    assert!(err.contains("no clip found"), "{err}");
}

#[test]
fn access_token_is_sent_and_enforced() {
    let relay = relay(&["--access-token", "s3cret"]);
    let err = stderr_of_failure(&mut yacs(&relay, &["send", "-t", "x"]));
    assert!(err.contains("access token"), "{err}");
    yacs(&relay, &["send", "-t", "x"])
        .env("YACS_TOKEN", "s3cret")
        .assert()
        .success();
}

/// Big files go in chunks, and neither side holds them in memory.
/// The file isn't held here as a whole either, just to keep the test light.
#[test]
fn big_files_stream_through_the_relay() {
    const BLOCKS: usize = 200;
    let relay = relay(&[]);
    // A saved space, as on a server.
    saved(&relay, &["join"])
        .write_stdin(invite_link(&relay, None))
        .assert()
        .success();
    let dir = TempDir::new().unwrap();
    let big = dir.path().join("disk.img");
    // 1 MiB blocks that differ, so misplaced chunks would show.
    let block = |i: usize| -> Vec<u8> {
        let mut block: Vec<u8> = (0..1024 * 1024).map(|j: u32| (j % 253) as u8).collect();
        block[0] = i as u8;
        block
    };
    let mut file = std::fs::File::create(&big).unwrap();
    for i in 0..BLOCKS {
        std::io::Write::write_all(&mut file, &block(i)).unwrap();
    }
    drop(file);

    let sent = measured(&relay, &["send", big.to_str().unwrap()]);
    assert!(sent.starts_with("sent disk.img (209.7 MB)"), "{sent}");

    let list = stdout(&mut saved(&relay, &["list"]));
    assert!(list.contains("209.7 MB"), "{list}");

    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let received = measured(&relay, &["recv", "-o", out.to_str().unwrap()]);
    assert!(received.contains("saved"), "{received}");
    let mut got = std::fs::File::open(out.join("disk.img")).unwrap();
    let mut buf = vec![0; 1024 * 1024];
    for i in 0..BLOCKS {
        std::io::Read::read_exact(&mut got, &mut buf).unwrap();
        assert!(buf == block(i), "block {i} differs");
    }
    assert_eq!(std::io::Read::read(&mut got, &mut buf).unwrap(), 0);
    assert!(!out.join("disk.img.part").exists());

    // Piped, as with small files; counted, not kept.
    let mut recv = std::process::Command::new(env!("CARGO_BIN_EXE_yacs"))
        .env_clear()
        .env("YACS_CONFIG", relay.home.path().join("cli.json"))
        .arg("recv")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let piped = std::io::copy(&mut recv.stdout.take().unwrap(), &mut std::io::sink()).unwrap();
    assert!(recv.wait().unwrap().success());
    assert_eq!(piped, (BLOCKS * 1024 * 1024) as u64);
}

/// Runs `yacs` with the saved space, checks it succeeded and peaked below
/// 50 MiB of memory, and returns its stderr.
fn measured(relay: &Relay, args: &[&str]) -> String {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_yacs"))
        .env_clear()
        .env("YACS_CONFIG", relay.home.path().join("cli.json"))
        .env("YACS_DEVICE_NAME", "e2e")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut stderr, &mut text).unwrap();
        text
    });
    let (succeeded, peak) = wait_measured(child);
    let stderr = reader.join().unwrap();
    assert!(succeeded, "{args:?} failed: {stderr}");
    if let Some(peak) = peak {
        assert!(peak < 50 << 20, "{args:?} peaked at {} MiB", peak >> 20);
    }
    stderr
}

/// Waits for `child`: whether it succeeded, and its peak memory in bytes.
///
/// Not `ru_maxrss` here: Linux carries the parent's peak over into it at
/// `exec`, and this test process runs the relay. `VmHWM` starts fresh with
/// the new program; it's sampled until the child exits.
#[cfg(target_os = "linux")]
fn wait_measured(mut child: std::process::Child) -> (bool, Option<u64>) {
    let path = format!("/proc/{}/status", child.id());
    let mut peak = 0;
    loop {
        let hwm = std::fs::read_to_string(&path).ok().and_then(|status| {
            let line = status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?;
            line.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()
        });
        peak = peak.max(hwm.unwrap_or(0) * 1024);
        if let Some(status) = child.try_wait().unwrap() {
            return (status.success(), Some(peak));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `wait4` reaps the child itself: its rusage is the point.
#[cfg(target_os = "macos")]
#[allow(clippy::zombie_processes)]
fn wait_measured(child: std::process::Child) -> (bool, Option<u64>) {
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::wait4(pid, &mut status, 0, &mut usage) }, pid);
    let succeeded = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
    // Bytes on macOS.
    (succeeded, Some(usage.ru_maxrss as u64))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn wait_measured(mut child: std::process::Child) -> (bool, Option<u64>) {
    (child.wait().unwrap().success(), None)
}

#[test]
fn info_shows_server_limits() {
    let relay = relay(&["--max-ttl", "7d"]);
    let info = stdout(&mut yacs(&relay, &["info"]));
    assert!(
        info.contains(&format!("relay        {} (", relay.url)),
        "{info}"
    );
    assert!(info.contains("default ttl  15m"), "{info}");
    assert!(info.contains("max ttl      7d"), "{info}");
    assert!(info.contains("history      50 clips"), "{info}");
    assert!(
        info.contains("big files    yes, in chunks of 4.2 MB"),
        "{info}"
    );
}

#[test]
fn not_in_a_space_is_a_clear_error() {
    let relay = relay(&[]);
    let err = stderr_of_failure(saved(&relay, &["list"]).env("YACS_SPACE", secret()));
    assert!(err.contains("not in a space: run `yacs join`"), "{err}");
    let err = stderr_of_failure(saved(&relay, &["list"]).env("YACS_PHRASE", "a b c"));
    assert!(err.contains("YACS_PHRASE is no longer used"), "{err}");
    let err = stderr_of_failure(yacs(&relay, &["list"]).env("YACS_SPACE", "tundra velvet"));
    assert!(err.contains("yacs space export"), "{err}");
}

/// What the desktop's "Copy link" gives you.
fn invite_link(relay: &Relay, token: Option<&str>) -> String {
    let token = token.map(|t| format!("&token={t}")).unwrap_or_default();
    format!("{}/#pair={}{token}&name=Server+room", relay.url, secret())
}

#[test]
fn joins_with_a_link_and_remembers_it() {
    let relay = relay(&["--access-token", "s3cret"]);
    let err = stderr_of_failure(saved(&relay, &["join"]).write_stdin(invite_link(&relay, None)));
    assert!(err.contains("access token"), "{err}");
    assert!(!relay.home.path().join("cli.json").exists());
    let err = stderr_of_failure(saved(&relay, &["join"]).write_stdin("tundra velvet anchor"));
    assert!(err.contains("isn't an invite link"), "{err}");

    let out = saved(&relay, &["pair"])
        .write_stdin(format!("{}\n", invite_link(&relay, Some("s3cret"))))
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains(&format!("Joined \"Server room\" on {}", relay.url)),
        "{out}"
    );

    // No flags or env from here on, and the same channel as the link.
    saved(&relay, &["send", "-t", "from the server"])
        .assert()
        .success();
    assert_eq!(
        stdout(yacs(&relay, &["recv"]).env("YACS_TOKEN", "s3cret")),
        "from the server"
    );
    assert_eq!(
        stdout(&mut saved(&relay, &["spaces"])),
        format!("Server room\t{}\n", relay.url)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(relay.home.path().join("cli.json")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    saved(&relay, &["leave"]).assert().success();
    assert!(!relay.home.path().join("cli.json").exists());
    let err = stderr_of_failure(&mut saved(&relay, &["list"]));
    assert!(err.contains("not in a space"), "{err}");
}

#[test]
fn starts_a_space_and_invites_another_machine() {
    let relay = relay(&[]);
    saved(&relay, &["space", "new", "--name", "Lab"])
        .env("YACS_SERVER", format!("{}/", relay.url))
        .assert()
        .success();
    saved(&relay, &["send", "-t", "hi"]).assert().success();

    let link = stdout(&mut saved(&relay, &["invite"]));
    assert!(
        link.starts_with(&format!("{}/#join=v2.", relay.url)),
        "{link}"
    );
    let other = TempDir::new().unwrap();
    let on_other = |args: &[&str]| {
        let mut cmd = saved(&relay, args);
        cmd.env("YACS_CONFIG", other.path().join("cli.json"));
        cmd
    };
    let joined = on_other(&["join"])
        .write_stdin(link.clone())
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let joined = String::from_utf8(joined).unwrap();
    assert!(joined.contains("Joined \"Lab\""), "{joined}");
    assert!(joined.contains("invited by e2e"), "{joined}");
    assert_eq!(stdout(&mut on_other(&["recv"])), "hi");

    // Once only.
    let third = TempDir::new().unwrap();
    let err = stderr_of_failure(
        saved(&relay, &["join"])
            .env("YACS_CONFIG", third.path().join("cli.json"))
            .write_stdin(link),
    );
    assert!(err.contains("already used or has expired"), "{err}");

    // Names are each machine's own.
    on_other(&["space", "rename", "  The lab "])
        .assert()
        .success();
    assert!(stdout(&mut on_other(&["spaces"])).starts_with("The lab\t"));
    assert!(stdout(&mut saved(&relay, &["spaces"])).starts_with("Lab\t"));
    let err = stderr_of_failure(&mut on_other(&["space", "rename", " "]));
    assert!(err.contains("can't be empty"), "{err}");

    // An exported space works from the environment alone.
    let exported = stdout(&mut saved(&relay, &["space", "export"]));
    let mut script = cargo_bin_cmd!("yacs");
    script
        .env_clear()
        .env("YACS_CONFIG", other.path().join("none.json"))
        .env("YACS_SERVER", &relay.url)
        .env("YACS_SPACE", exported.trim())
        .arg("recv");
    assert_eq!(stdout(&mut script), "hi");
}

#[test]
fn saved_token_only_goes_to_its_own_relay() {
    let relay = relay(&["--access-token", "s3cret"]);
    saved(&relay, &["join"])
        .write_stdin(invite_link(&relay, Some("s3cret")))
        .assert()
        .success();
    let other = self::relay(&["--access-token", "s3cret"]);
    let err = stderr_of_failure(
        saved(&relay, &["list"])
            .env("YACS_SERVER", &other.url)
            .env("YACS_SPACE", secret()),
    );
    assert!(err.contains("access token"), "{err}");
    let err = stderr_of_failure(saved(&relay, &["list"]).env("YACS_SERVER", &other.url));
    assert!(err.contains("not "), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn client_hears_about_new_clips_right_away() {
    use yacs_core::api::ChannelEvent;
    use yacs_core::{ClipItem, Payload};

    let relay = relay(&[]);
    let client = yacs_client::Client::new(&relay.url, None, space(7)).unwrap();
    let mut events = client.events().await.unwrap();

    let mut send = yacs(&relay, &["send", "-t", "live"]);
    tokio::task::spawn_blocking(move || send.assert().success())
        .await
        .unwrap();

    let Some(ChannelEvent::Added { clip }) = events.next().await.unwrap() else {
        panic!("expected an added clip");
    };
    let (_, Payload::Clip(received)) = client.get(&clip.id).await.unwrap().unwrap();
    assert_eq!(received.items, [ClipItem::Text("live".into())]);
}
