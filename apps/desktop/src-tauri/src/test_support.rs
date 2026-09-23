//! Shared by the unit tests: a real relay on a random local port.

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use yacs_server::{Config, SystemClock};

pub const PHRASE: &str = "tundra velvet anchor pickle orbit meadow";

/// Returns the relay's URL; it stores clips until the `TempDir` is dropped.
pub async fn relay(extra: &[&str]) -> (String, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let dir = data.path().to_str().unwrap().to_owned();
    let mut args = vec!["yacs-server", "--data-dir", &dir];
    args.extend_from_slice(extra);
    let config = Config::try_parse_from(args).unwrap().validate().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(yacs_server::run(
        listener,
        config,
        Arc::new(SystemClock),
        std::future::pending(),
    ));
    (format!("http://{addr}"), data)
}
