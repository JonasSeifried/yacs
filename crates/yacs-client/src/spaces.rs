//! The spaces a device takes part in, as the desktop app (`spaces.json`) and
//! the `yacs` command (`cli.json`) store them. Each app keeps its own file,
//! readable only by the user: it holds every space's key.
//!
//! The list can hold several spaces, possibly on different relays; the apps
//! use the first one for now. Access tokens are kept per relay, apart from the
//! spaces, so an invite never has to carry one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use yacs_core::Pairing;

/// What a device calls its first space. Names are each device's own label:
/// they never go to the relay, and renaming one doesn't rename it elsewhere.
pub const DEFAULT_SPACE_NAME: &str = "My devices";
/// Long enough for "Anna & me (work laptop)", short enough for Spotlight.
pub const MAX_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub name: String,
    /// Base URL, normalized by [`normalize_relay`].
    pub relay: String,
    /// `Pairing::to_secret`.
    secret: String,
}

impl Space {
    pub fn new(name: &str, relay: &str, pairing: &Pairing) -> Self {
        Self {
            name: clean_name(name).unwrap_or_else(|| DEFAULT_SPACE_NAME.into()),
            relay: normalize_relay(relay),
            secret: pairing.to_secret(),
        }
    }

    pub fn pairing(&self) -> Result<Pairing, yacs_core::Error> {
        Pairing::from_secret(&self.secret)
    }

    /// For `YACS_SPACE`: as secret as the key.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spaces {
    #[serde(default)]
    pub spaces: Vec<Space>,
    /// Access token by relay URL.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tokens: BTreeMap<String, String>,
}

impl Spaces {
    /// The space in use: the first one.
    pub fn current(&self) -> Option<&Space> {
        self.spaces.first()
    }

    pub fn token(&self, relay: &str) -> Option<&str> {
        self.tokens.get(&normalize_relay(relay)).map(String::as_str)
    }

    /// Makes `space` the one in use. While the apps show one space, it
    /// replaces the others.
    pub fn set_current(&mut self, space: Space, token: Option<String>) {
        match token {
            Some(token) => {
                self.tokens.insert(space.relay.clone(), token);
            }
            None => {
                self.tokens.remove(&space.relay);
            }
        }
        self.spaces = vec![space];
        self.forget_unused_tokens();
    }

    /// Returns the new name, or `None` if there's no space or the name is blank.
    pub fn rename_current(&mut self, name: &str) -> Option<String> {
        let name = clean_name(name)?;
        let space = self.spaces.first_mut()?;
        space.name.clone_from(&name);
        Some(name)
    }

    /// Forgets the space in use (and the relay's token, if no other space
    /// uses that relay). Returns it, if there was one.
    pub fn leave_current(&mut self) -> Option<Space> {
        if self.spaces.is_empty() {
            return None;
        }
        let left = self.spaces.remove(0);
        self.forget_unused_tokens();
        Some(left)
    }

    fn forget_unused_tokens(&mut self) {
        let spaces = &self.spaces;
        self.tokens
            .retain(|relay, _| spaces.iter().any(|s| s.relay == *relay));
    }
}

/// Adds a device to a space: `https://relay/#pair=v1.<channel>.<key>&token=…&name=…`,
/// shown as a QR code and link by the desktop app and printed by `yacs invite`.
/// Browsers never send the fragment to the relay. It works for as long as the
/// space exists, so it's as secret as the key; one-time invites replace it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteLink {
    pub relay: String,
    pub pairing: Pairing,
    /// The relay's access token, which the invited device needs as well.
    pub token: Option<String>,
    /// The inviter's name for the space, suggested to the invited device.
    pub name: Option<String>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("that isn't an invite link")]
    NotALink,
    #[error("that link has no space in it")]
    NoSpace,
    #[error("the space in that link is damaged")]
    Damaged,
}

impl InviteLink {
    pub fn new(space: &Space, token: Option<&str>) -> Result<Self, yacs_core::Error> {
        Ok(Self {
            relay: space.relay.clone(),
            pairing: space.pairing()?,
            token: token.map(str::to_owned),
            name: Some(space.name.clone()),
        })
    }

    pub fn to_url(&self) -> String {
        let mut fragment = url::form_urlencoded::Serializer::new(String::new());
        // The secret is base64url and a dot: nothing to escape.
        let mut url = format!("{}/#pair={}", self.relay, self.pairing.to_secret());
        if let Some(token) = &self.token {
            fragment.append_pair("token", token);
        }
        if let Some(name) = &self.name {
            fragment.append_pair("name", name);
        }
        let rest = fragment.finish();
        if !rest.is_empty() {
            url.push('&');
            url.push_str(&rest);
        }
        url
    }

    pub fn parse(input: &str) -> Result<Self, LinkError> {
        let mut url = url::Url::parse(input.trim()).map_err(|_| LinkError::NotALink)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(LinkError::NotALink);
        }
        let fragment = url.fragment().unwrap_or_default().to_owned();
        let (mut secret, mut token, mut name) = (None, None, None);
        for (key, value) in url::form_urlencoded::parse(fragment.as_bytes()) {
            let value = Some(value.into_owned()).filter(|v| !v.is_empty());
            match &*key {
                "pair" => secret = value,
                "token" => token = value,
                "name" => name = value.as_deref().and_then(clean_name),
                _ => {}
            }
        }
        let pairing = Pairing::from_secret(&secret.ok_or(LinkError::NoSpace)?)
            .map_err(|_| LinkError::Damaged)?;
        url.set_fragment(None);
        Ok(Self {
            relay: normalize_relay(url.as_str()),
            pairing,
            token,
            name,
        })
    }
}

/// Trimmed, whitespace collapsed, at most [`MAX_NAME_CHARS`]; `None` if blank.
pub fn clean_name(name: &str) -> Option<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = name.chars().take(MAX_NAME_CHARS).collect();
    let name = name.trim_end();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Trimmed, without a trailing slash, so one relay always has one key in `tokens`.
pub fn normalize_relay(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use yacs_core::{ChannelId, ChannelKey};

    use super::*;

    fn pairing(n: u8) -> Pairing {
        Pairing {
            channel_id: ChannelId::from_bytes([n; 32]),
            key: ChannelKey::from_bytes([n + 1; 32]),
        }
    }

    #[test]
    fn names_are_cleaned_and_default() {
        assert_eq!(clean_name("  Anna \t &  me "), Some("Anna & me".into()));
        assert_eq!(clean_name(" \n "), None);
        assert_eq!(clean_name(&"x".repeat(100)).unwrap().len(), MAX_NAME_CHARS);
        assert_eq!(
            clean_name(&format!("{} y", "x".repeat(MAX_NAME_CHARS - 1))),
            Some("x".repeat(MAX_NAME_CHARS - 1))
        );
        let space = Space::new("  ", "https://relay.example/ ", &pairing(1));
        assert_eq!(space.name, DEFAULT_SPACE_NAME);
        assert_eq!(space.relay, "https://relay.example");
        assert_eq!(space.pairing().unwrap(), pairing(1));
    }

    #[test]
    fn set_rename_and_leave() {
        let mut spaces = Spaces::default();
        assert_eq!(spaces.rename_current("Work"), None);
        assert_eq!(spaces.leave_current(), None);

        spaces.set_current(
            Space::new("Home", "https://a.example", &pairing(1)),
            Some("t".into()),
        );
        assert_eq!(spaces.token("https://a.example/"), Some("t"));
        assert_eq!(spaces.rename_current("  "), None);
        assert_eq!(spaces.rename_current(" Work "), Some("Work".into()));
        assert_eq!(spaces.current().unwrap().name, "Work");

        // Another relay: the first one's token goes with its space.
        spaces.set_current(Space::new("B", "https://b.example", &pairing(2)), None);
        assert_eq!(spaces.spaces.len(), 1);
        assert!(spaces.tokens.is_empty());

        spaces.set_current(
            Space::new("B", "https://b.example", &pairing(2)),
            Some("u".into()),
        );
        assert_eq!(spaces.leave_current().unwrap().name, "B");
        assert_eq!(spaces, Spaces::default());
    }

    #[test]
    fn invite_links_round_trip() {
        let space = Space::new("Anna & me", "https://clip.example.com/yacs/", &pairing(1));
        let link = InviteLink::new(&space, Some("s3cret &x")).unwrap();
        let url = link.to_url();
        let secret = pairing(1).to_secret();
        assert_eq!(
            url,
            format!(
                "https://clip.example.com/yacs/#pair={secret}&token=s3cret+%26x&name=Anna+%26+me"
            )
        );
        assert_eq!(InviteLink::parse(&format!(" {url}\n")), Ok(link));

        let bare = InviteLink::parse(&format!("http://10.0.0.2:8080/#pair={secret}")).unwrap();
        assert_eq!(bare.relay, "http://10.0.0.2:8080");
        assert_eq!((bare.token, bare.name), (None, None));
        let no_extras = InviteLink {
            relay: "http://10.0.0.2:8080".into(),
            pairing: pairing(1),
            token: None,
            name: None,
        };
        assert_eq!(
            no_extras.to_url(),
            format!("http://10.0.0.2:8080/#pair={secret}")
        );

        for (bad, err) in [
            ("tundra velvet anchor", LinkError::NotALink),
            ("ftp://clip.example.com/#pair=x", LinkError::NotALink),
            ("https://clip.example.com/", LinkError::NoSpace),
            ("https://clip.example.com/#pair=v1.nope", LinkError::Damaged),
        ] {
            assert_eq!(InviteLink::parse(bad), Err(err), "{bad}");
        }
    }

    #[test]
    fn round_trips_through_json() {
        let mut spaces = Spaces::default();
        spaces.set_current(
            Space::new("Anna & me", "https://a.example", &pairing(1)),
            Some("t".into()),
        );
        let json = serde_json::to_string(&spaces).unwrap();
        assert_eq!(serde_json::from_str::<Spaces>(&json).unwrap(), spaces);
        assert_eq!(
            serde_json::from_str::<Spaces>("{}").unwrap(),
            Spaces::default()
        );
    }
}
