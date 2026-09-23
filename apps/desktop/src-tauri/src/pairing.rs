//! Turning what the user typed into a working, verified connection.

use yacs_client::Client;
use yacs_core::Pairing;

use crate::secrets::Stored;

pub struct Connected {
    /// Normalized: trimmed, no trailing slash.
    pub server_url: String,
    pub client: Client,
    pub stored: Stored,
}

/// Derive the pairing from the phrase and prove the relay is reachable and
/// accepts the token. Nothing is persisted here, so a failure leaves no trace.
pub async fn connect(
    server_url: &str,
    token: Option<&str>,
    phrase: String,
) -> Result<Connected, String> {
    let server_url = server_url.trim().trim_end_matches('/').to_owned();
    let token = token
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);

    // Argon2id is deliberately slow; keep it off the async runtime.
    let pairing = tauri::async_runtime::spawn_blocking(move || Pairing::from_phrase(&phrase))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    let client =
        Client::new(&server_url, token.clone(), pairing.clone()).map_err(|e| e.to_string())?;
    client
        .config()
        .await
        .map_err(|e| format!("couldn't connect to {server_url}: {e}"))?;

    Ok(Connected {
        server_url,
        client,
        stored: Stored { pairing, token },
    })
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use clap::Parser;
    use yacs_server::{Config, SystemClock};

    use super::*;

    const PHRASE: &str = "tundra velvet anchor pickle orbit meadow";

    async fn relay(extra: &[&str]) -> (String, tempfile::TempDir) {
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

    #[tokio::test]
    async fn connects_and_normalizes_input() {
        let (url, _data) = relay(&[]).await;
        let connected = connect(&format!("  {url}/ "), Some("  "), PHRASE.into())
            .await
            .unwrap();
        assert_eq!(connected.server_url, url);
        assert_eq!(connected.stored.token, None);
        assert_eq!(
            connected.stored.pairing,
            Pairing::from_phrase(PHRASE).unwrap()
        );
    }

    #[tokio::test]
    async fn checks_the_access_token() {
        let (url, _data) = relay(&["--access-token", "s3cret"]).await;
        let err = connect(&url, None, PHRASE.into()).await.err().unwrap();
        assert!(err.contains("access token"), "{err}");
        let ok = connect(&url, Some("s3cret"), PHRASE.into()).await.unwrap();
        assert_eq!(ok.stored.token.as_deref(), Some("s3cret"));
    }

    #[tokio::test]
    async fn reports_unusable_input() {
        let err = connect("http://127.0.0.1:1", None, PHRASE.into())
            .await
            .err()
            .unwrap();
        assert!(
            err.starts_with("couldn't connect to http://127.0.0.1:1"),
            "{err}"
        );
        let err = connect("ftp://example.com", None, PHRASE.into())
            .await
            .err()
            .unwrap();
        assert!(err.contains("http://"), "{err}");
        let err = connect("http://127.0.0.1:1", None, "   ".into())
            .await
            .err()
            .unwrap();
        assert!(err.contains("empty"), "{err}");
    }
}
