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

/// What the "Pair a phone" QR code encodes: the relay's web app with the
/// pairing in the fragment, which browsers never send to the server.
pub struct PhoneLink {
    pub url: String,
    /// Why the phone might not get far with this link, if there's a reason.
    pub warning: Option<String>,
}

pub fn phone_link(server_url: &str, pairing: &Pairing, token: Option<&str>) -> PhoneLink {
    let mut url = format!(
        "{}/#pair={}",
        server_url.trim_end_matches('/'),
        pairing.to_secret()
    );
    if let Some(token) = token {
        url.push_str("&token=");
        url.extend(url::form_urlencoded::byte_serialize(token.as_bytes()));
    }

    let parsed = url::Url::parse(server_url).ok();
    let host = parsed
        .as_ref()
        .and_then(|u| u.host_str())
        .unwrap_or_default();
    let local = matches!(host, "localhost" | "[::1]") || host.starts_with("127.");
    let warning = if local {
        Some(format!(
            "Your phone can't reach {host}: it's this computer. Pair this computer with the relay's network address (its IP or domain) to pair a phone."
        ))
    } else if parsed.is_some_and(|u| u.scheme() == "http") {
        Some("The relay uses http://, so the phone's browser won't allow Copy and Paste or installing the app. Put it behind HTTPS (see deploy/ in the repo).".into())
    } else {
        None
    };
    PhoneLink { url, warning }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{PHRASE, relay};

    fn pairing() -> Pairing {
        Pairing {
            channel_id: yacs_core::ChannelId::from_bytes([7; 32]),
            key: yacs_core::ChannelKey::from_bytes([9; 32]),
        }
    }

    #[test]
    fn phone_link_carries_the_pairing_in_the_fragment() {
        let link = phone_link("https://clip.example.com/", &pairing(), Some("s3cret &x"));
        let secret = pairing().to_secret();
        assert_eq!(
            link.url,
            format!("https://clip.example.com/#pair={secret}&token=s3cret+%26x")
        );
        assert_eq!(link.warning, None);
        let (_, fragment) = link.url.split_once('#').unwrap();
        assert!(!link.url[..link.url.len() - fragment.len()].contains(&secret));

        let link = phone_link("http://192.168.0.5:8080", &pairing(), None);
        assert_eq!(link.url, format!("http://192.168.0.5:8080/#pair={secret}"));
        assert!(link.warning.unwrap().contains("HTTPS"));

        for local in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://[::1]:8080",
        ] {
            let warning = phone_link(local, &pairing(), None).warning.unwrap();
            assert!(warning.contains("can't reach"), "{local}: {warning}");
        }
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
