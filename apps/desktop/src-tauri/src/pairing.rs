//! Starting or joining a space: a working, verified connection first, and the
//! invite link other devices join with.

use yacs_client::Client;
use yacs_client::spaces::{InviteLink, Space, normalize_relay};
use yacs_core::Pairing;

pub struct Connected {
    /// Normalized: trimmed, no trailing slash.
    pub relay: String,
    pub client: Client,
    pub token: Option<String>,
}

/// Proves the relay is reachable and accepts the token. Nothing is persisted
/// here, so a failure leaves no trace.
pub async fn connect(
    relay: &str,
    token: Option<&str>,
    pairing: Pairing,
) -> Result<Connected, String> {
    let relay = normalize_relay(relay);
    let token = token
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    let client = Client::new(&relay, token.clone(), pairing).map_err(|e| e.to_string())?;
    client
        .config()
        .await
        .map_err(|e| format!("couldn't connect to {relay}: {e}"))?;
    Ok(Connected {
        relay,
        client,
        token,
    })
}

/// What "Invite a device" shows as a QR code and link: the relay's web app
/// with the space in the fragment, which browsers never send to the server.
/// Phones open it; computers and `yacs join` take it pasted.
pub struct Invite {
    pub url: String,
    /// Why other devices might not get far with this link, if there's a reason.
    pub warning: Option<String>,
}

pub fn invite(space: &Space, token: Option<&str>) -> Result<Invite, String> {
    let url = InviteLink::new(space, token)
        .map_err(|e| e.to_string())?
        .to_url();
    let parsed = url::Url::parse(&space.relay).ok();
    let host = parsed
        .as_ref()
        .and_then(|u| u.host_str())
        .unwrap_or_default();
    let local = matches!(host, "localhost" | "[::1]") || host.starts_with("127.");
    let warning = if local {
        Some(format!(
            "Other devices can't reach {host}: it's this computer. Start the space with the relay's network address (its IP or domain) to invite other devices."
        ))
    } else if parsed.is_some_and(|u| u.scheme() == "http") {
        Some("The relay uses http://, so a phone's browser won't allow Copy and Paste or installing the app. Put it behind HTTPS (see deploy/ in the repo).".into())
    } else {
        None
    };
    Ok(Invite { url, warning })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::relay;

    fn pairing() -> Pairing {
        Pairing {
            channel_id: yacs_core::ChannelId::from_bytes([7; 32]),
            key: yacs_core::ChannelKey::from_bytes([9; 32]),
        }
    }

    #[test]
    fn invite_carries_the_space_in_the_fragment() {
        let space = Space::new("Home", "https://clip.example.com/", &pairing());
        let link = invite(&space, Some("s3cret &x")).unwrap();
        let secret = pairing().to_secret();
        assert_eq!(
            link.url,
            format!("https://clip.example.com/#pair={secret}&token=s3cret+%26x&name=Home")
        );
        assert_eq!(link.warning, None);
        let (_, fragment) = link.url.split_once('#').unwrap();
        assert!(!link.url[..link.url.len() - fragment.len()].contains(&secret));

        let space = Space::new("Home", "http://192.168.0.5:8080", &pairing());
        let link = invite(&space, None).unwrap();
        assert_eq!(
            link.url,
            format!("http://192.168.0.5:8080/#pair={secret}&name=Home")
        );
        assert!(link.warning.unwrap().contains("HTTPS"));

        for local in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://[::1]:8080",
        ] {
            let space = Space::new("Home", local, &pairing());
            let warning = invite(&space, None).unwrap().warning.unwrap();
            assert!(warning.contains("can't reach"), "{local}: {warning}");
        }
    }

    #[tokio::test]
    async fn connects_and_normalizes_input() {
        let (url, _data) = relay(&[]).await;
        let connected = connect(&format!("  {url}/ "), Some("  "), pairing())
            .await
            .unwrap();
        assert_eq!(connected.relay, url);
        assert_eq!(connected.token, None);
        assert_eq!(connected.client.pairing(), &pairing());
    }

    #[tokio::test]
    async fn checks_the_access_token() {
        let (url, _data) = relay(&["--access-token", "s3cret"]).await;
        let err = connect(&url, None, pairing()).await.err().unwrap();
        assert!(err.contains("access token"), "{err}");
        let ok = connect(&url, Some("s3cret"), pairing()).await.unwrap();
        assert_eq!(ok.token.as_deref(), Some("s3cret"));
    }

    #[tokio::test]
    async fn reports_unusable_input() {
        let err = connect("http://127.0.0.1:1", None, pairing())
            .await
            .err()
            .unwrap();
        assert!(
            err.starts_with("couldn't connect to http://127.0.0.1:1"),
            "{err}"
        );
        let err = connect("ftp://example.com", None, pairing())
            .await
            .err()
            .unwrap();
        assert!(err.contains("http://"), "{err}");
    }
}
