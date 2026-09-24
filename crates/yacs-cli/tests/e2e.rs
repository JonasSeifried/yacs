//! Runs the real `yacs` binary against a real relay on a random local port.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc;

use assert_cmd::cargo::cargo_bin_cmd;
use clap::Parser;
use tempfile::TempDir;
use yacs_server::{Config, SystemClock};

const PHRASE: &str = "tundra velvet anchor pickle orbit meadow";

/// A relay running on its own thread for the lifetime of the test.
struct Relay {
    url: String,
    _data: TempDir,
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
    }
}

fn yacs(relay: &Relay, args: &[&str]) -> assert_cmd::Command {
    let mut cmd = cargo_bin_cmd!("yacs");
    cmd.env_clear()
        .env("YACS_SERVER", &relay.url)
        .env("YACS_PHRASE", PHRASE)
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
    yacs(&relay, &["send", "hello from the cli"])
        .assert()
        .success();
    assert_eq!(stdout(&mut yacs(&relay, &["recv"])), "hello from the cli");

    yacs(&relay, &["send"])
        .write_stdin("piped\nlines\n")
        .assert()
        .success();
    assert_eq!(stdout(&mut yacs(&relay, &["recv"])), "piped\nlines\n");
}

#[test]
fn history_list_recv_by_id_and_delete() {
    let relay = relay(&[]);
    yacs(&relay, &["send", "first"]).assert().success();
    yacs(&relay, &["send", "second", "--ttl", "1h"])
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

    yacs(&relay, &["send", "--image", png.to_str().unwrap()])
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
fn wrong_phrase_cannot_read_and_sees_its_own_empty_channel() {
    let relay = relay(&[]);
    yacs(&relay, &["send", "secret"]).assert().success();
    let err = stderr_of_failure(yacs(&relay, &["recv"]).env("YACS_PHRASE", "some other phrase"));
    assert!(err.contains("no clip found"), "{err}");
}

#[test]
fn phrase_normalization_pairs_devices() {
    let relay = relay(&[]);
    yacs(&relay, &["send", "typed on a phone"])
        .assert()
        .success();
    let sloppy = "  Tundra VELVET anchor  pickle orbit meadow ";
    assert_eq!(
        stdout(yacs(&relay, &["recv"]).env("YACS_PHRASE", sloppy)),
        "typed on a phone"
    );
}

#[test]
fn access_token_is_sent_and_enforced() {
    let relay = relay(&["--access-token", "s3cret"]);
    let err = stderr_of_failure(&mut yacs(&relay, &["send", "x"]));
    assert!(err.contains("access token"), "{err}");
    yacs(&relay, &["send", "x"])
        .env("YACS_TOKEN", "s3cret")
        .assert()
        .success();
}

#[test]
fn info_shows_server_limits() {
    let relay = relay(&["--max-ttl", "7d"]);
    let info = stdout(&mut yacs(&relay, &["info"]));
    assert!(info.contains("default ttl  15m"), "{info}");
    assert!(info.contains("max ttl      7d"), "{info}");
    assert!(info.contains("history      50 clips"), "{info}");
}

#[test]
fn missing_server_is_a_clear_error() {
    let mut cmd = cargo_bin_cmd!("yacs");
    cmd.env_clear().env("YACS_PHRASE", PHRASE).args(["list"]);
    assert!(stderr_of_failure(&mut cmd).contains("no relay configured"));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_hears_about_new_clips_right_away() {
    use yacs_core::api::ChannelEvent;
    use yacs_core::{ClipItem, Pairing, Payload};

    let relay = relay(&[]);
    let pairing = Pairing::from_phrase(PHRASE).unwrap();
    let client = yacs_client::Client::new(&relay.url, None, pairing).unwrap();
    let mut events = client.events().await.unwrap();

    let mut send = yacs(&relay, &["send", "live"]);
    tokio::task::spawn_blocking(move || send.assert().success())
        .await
        .unwrap();

    let Some(ChannelEvent::Added { clip }) = events.next().await.unwrap() else {
        panic!("expected an added clip");
    };
    let (_, Payload::Clip(received)) = client.get(&clip.id).await.unwrap().unwrap();
    assert_eq!(received.items, [ClipItem::Text("live".into())]);
}
