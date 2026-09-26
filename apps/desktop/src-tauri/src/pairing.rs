//! Starting or joining a space: a working, verified connection first, and the
//! invite link other devices join with.

use yacs_client::Client;
use yacs_client::spaces::{Space, SpaceLink, invite_url, normalize_relay};
use yacs_core::InviteSecret;
use yacs_core::Pairing;

pub struct Connected {
    /// Normalized: trimmed, no trailing slash.
    pub relay: String,
    pub client: Client,
    pub token: Option<String>,
}

/// Proves the relay is reachable and lets this device into the space,
/// registering a new one (which may take the account key). Nothing is
/// persisted here, so a failure leaves no trace.
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
        .check()
        .await
        .map_err(|e| format!("couldn't connect to {relay}: {e}"))?;
    Ok(Connected {
        relay,
        client,
        token,
    })
}

/// What "Invite a device" shows as a QR code and link: the relay's web app
/// with the invite's secret in the fragment, which browsers never send to the
/// server. Phones open it; computers and `yacs join` take it pasted.
pub struct Invite {
    pub url: String,
    /// Why other devices might not get far with this link, if there's a reason.
    pub warning: Option<String>,
}

pub fn invite(relay: &str, secret: &InviteSecret) -> Invite {
    Invite {
        url: invite_url(relay, secret),
        warning: warning(relay),
    }
}

/// For the bundled `yacs` command, on this computer: the space itself, so
/// installing needs no round trip to the relay.
pub fn space_link(space: &Space, token: Option<&str>) -> Result<String, String> {
    Ok(SpaceLink::new(space, token)
        .map_err(|e| e.to_string())?
        .to_url())
}

fn warning(relay: &str) -> Option<String> {
    let parsed = url::Url::parse(relay).ok();
    let host = parsed
        .as_ref()
        .and_then(|u| u.host_str())
        .unwrap_or_default();
    let local = matches!(host, "localhost" | "[::1]") || host.starts_with("127.");
    if local {
        Some(format!(
            "Other devices can't reach {host}: it's this computer. Start the space with the relay's network address (its IP or domain) to invite other devices."
        ))
    } else if parsed.is_some_and(|u| u.scheme() == "http") {
        Some("The relay uses http://, so a phone's browser won't allow Copy and Paste or installing the app. Put it behind HTTPS (see deploy/ in the repo).".into())
    } else {
        None
    }
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
    fn invite_carries_the_secret_in_the_fragment() {
        let secret = InviteSecret::from_bytes([3; 32]);
        let link = invite("https://clip.example.com", &secret);
        assert_eq!(link.url, format!("https://clip.example.com/#join={secret}"));
        assert_eq!(link.warning, None);

        let link = invite("http://192.168.0.5:8080", &secret);
        assert!(link.warning.unwrap().contains("HTTPS"));

        for local in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://[::1]:8080",
        ] {
            let warning = invite(local, &secret).warning.unwrap();
            assert!(warning.contains("can't reach"), "{local}: {warning}");
        }
    }

    #[test]
    fn space_link_carries_the_space_and_token() {
        let space = Space::new("Home", "https://clip.example.com/", &pairing());
        let url = space_link(&space, Some("s3cret &x")).unwrap();
        let secret = pairing().to_secret();
        assert_eq!(
            url,
            format!("https://clip.example.com/#pair={secret}&token=s3cret+%26x&name=Home")
        );
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
    async fn checks_the_account_key() {
        let (url, _data) = relay(&["--access-token", "s3cret"]).await;
        let err = connect(&url, None, pairing()).await.err().unwrap();
        assert!(err.contains("account key"), "{err}");
        let err = connect(&url, Some("nope"), pairing()).await.err().unwrap();
        assert!(err.contains("account key"), "{err}");
        let ok = connect(&url, Some("s3cret"), pairing()).await.unwrap();
        assert_eq!(ok.token.as_deref(), Some("s3cret"));
        // The space is registered now: joining it needs no key.
        connect(&url, None, pairing()).await.unwrap();
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
