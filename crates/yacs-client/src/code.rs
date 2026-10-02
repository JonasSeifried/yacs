//! Typed codes over the relay's rendezvous (see `yacs_core::code` for the
//! exchange, `yacs_core::api` for the routes).
//!
//! Reads are retried on network trouble and server errors. Writes aren't:
//! one whose answer got lost would be refused the second time, as if
//! someone else had used the code.

use reqwest::{Response, Url};
use yacs_core::api::{ENVELOPE_CONTENT_TYPE, RendezvousOpened};
use yacs_core::{Code, CodeInviter, CodeJoiner, Invite};

use crate::chunks::{retry, transient};
use crate::{Client, Error, Result, checked, open_http, relay_url};

/// Seconds each read waits on the relay before asking again.
const WAIT_SECS: u64 = 25;

/// A code the relay holds open, waiting for someone to type it.
pub struct Offer {
    inviter: CodeInviter,
    nameplate: u16,
}

impl Offer {
    /// What to show, e.g. `7-tulip-apple`.
    pub fn code(&self) -> Code {
        self.inviter.code(self.nameplate)
    }

    pub fn nameplate(&self) -> u16 {
        self.nameplate
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CodeOutcome {
    /// The device that typed the code got the invite.
    Joined { device: String },
    /// Someone typed a wrong code, which used it up: show a new one.
    WrongCode,
    /// Nobody typed it in time, or the rendezvous was closed.
    Expired,
}

impl Client {
    /// Opens a rendezvous for a new code.
    pub async fn offer_code(&self) -> Result<Offer> {
        let (inviter, message) = CodeInviter::start()?;
        let req = self
            .http
            .post(self.rendezvous_url.clone())
            .header(reqwest::header::CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
            .body(message);
        let res = match self.send(req).await {
            Ok(res) => res,
            Err(Error::Server {
                status: 404 | 405, ..
            }) => return Err(Error::NoInvites),
            Err(e) => return Err(e),
        };
        let opened: RendezvousOpened = res.json().await.map_err(|_| Error::BadResponse)?;
        Ok(Offer {
            inviter,
            nameplate: opened.nameplate,
        })
    }

    /// Waits until someone types the code, then hands them an invite to this
    /// space. Cancel by dropping the future and
    /// calling [`close_code`](Self::close_code).
    pub async fn complete_code(
        &self,
        offer: Offer,
        space_name: &str,
        inviter_name: &str,
    ) -> Result<CodeOutcome> {
        let nameplate = offer.nameplate;
        let answer = self.rendezvous(&format!("{nameplate}/b/0"));
        let answer = loop {
            match retry(|| async { wait(self.send(self.http.get(answer.clone())).await).await })
                .await?
            {
                Waited::Message(bytes) => break bytes,
                Waited::Nothing => continue,
                Waited::Gone => return Ok(CodeOutcome::Expired),
            }
        };
        let Ok((device, key)) = offer.inviter.finish(nameplate, &answer) else {
            self.close_code(nameplate).await;
            return Ok(CodeOutcome::WrongCode);
        };
        let invite = Invite::new(
            space_name,
            inviter_name,
            self.invite_token().await?,
            &self.pairing,
        );
        let req = self
            .http
            .put(self.rendezvous(&format!("{nameplate}/a/1")))
            .header(reqwest::header::CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
            .body(key.seal_invite(&invite)?);
        match self.send(req).await {
            Ok(_) => Ok(CodeOutcome::Joined { device }),
            Err(Error::Server { status: 404, .. }) => Ok(CodeOutcome::Expired),
            Err(e) => Err(e),
        }
    }

    /// Takes the code off the relay. Best effort: it expires anyway.
    pub async fn close_code(&self, nameplate: u16) {
        let url = self.rendezvous(&nameplate.to_string());
        let _ = self.send(self.http.delete(url)).await;
    }

    fn rendezvous(&self, path: &str) -> Url {
        let mut url = self.rendezvous_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .expect("http(s) URLs have path segments");
            segments.extend(path.split('/'));
        }
        url.set_query(Some(&format!("wait={WAIT_SECS}")));
        url
    }
}

/// Types `code` for the space its inviter shows it for, on `relay`, and
/// returns the invite. `device_name` is shown to the inviter.
pub async fn join_with_code(relay: &str, code: &Code, device_name: &str) -> Result<Invite> {
    let http = open_http()?;
    let nameplate = code.nameplate();
    let url = |path: &str| -> Result<Url> {
        let mut url = relay_url(relay, &format!("rendezvous/{nameplate}/{path}"))?;
        url.set_query(Some(&format!("wait={WAIT_SECS}")));
        Ok(url)
    };

    // The relay closes the exchange as it hands the joiner `a/1`, so when
    // that answer got lost, asking again finds it gone: the failure is the
    // network's, not a wrong code. Reading `a/0` closes nothing, so gone
    // there means the code is.
    let read = |path: &'static str, closed_by_reading: bool| {
        let (http, url) = (&http, &url);
        let once =
            move || async move { wait(checked(http.get(url(path)?).send().await?).await).await };
        async move {
            match once().await {
                Err(e) if transient(&e) => match retry(once).await {
                    Ok(Waited::Gone) if closed_by_reading => Err(e),
                    retried => retried,
                },
                first => first,
            }
        }
    };

    let message = loop {
        match read("a/0", false).await? {
            Waited::Message(bytes) => break bytes,
            Waited::Nothing => continue,
            Waited::Gone => return Err(Error::CodeNotFound),
        }
    };
    let (answer, key) = CodeJoiner::start(code)
        .answer(&message, device_name)
        .map_err(|_| Error::WrongCode)?;
    let res = http
        .put(url("b/0")?)
        .header(reqwest::header::CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
        .body(answer)
        .send()
        .await?;
    match checked(res).await {
        Ok(_) => {}
        Err(Error::Server { status: 409, .. }) => return Err(Error::CodeTaken),
        Err(Error::Server { status: 404, .. }) => return Err(Error::CodeNotFound),
        Err(e) => return Err(e),
    }

    let sealed = loop {
        match read("a/1", true).await? {
            Waited::Message(bytes) => break bytes,
            Waited::Nothing => continue,
            // The inviter couldn't open the answer and gave the code up.
            Waited::Gone => return Err(Error::WrongCode),
        }
    };
    key.open_invite(&sealed).map_err(|_| Error::WrongCode)
}

enum Waited {
    Message(Vec<u8>),
    /// Nothing yet: ask again.
    Nothing,
    Gone,
}

async fn wait(res: Result<Response>) -> Result<Waited> {
    match res {
        Ok(res) if res.status() == reqwest::StatusCode::NO_CONTENT => Ok(Waited::Nothing),
        Ok(res) => Ok(Waited::Message(res.bytes().await?.to_vec())),
        Err(Error::Server { status: 404, .. }) => Ok(Waited::Gone),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use axum::http::{Method, StatusCode, Uri};
    use axum::response::IntoResponse;

    use super::*;

    /// A rendezvous that answers each read in `flaky` with a 502 once, as a
    /// proxy in front of a relay might. Reads don't wait: nothing yet is 204,
    /// and a nameplate other than 7 is 404.
    /// A read in `lost` takes the message once it's there, like the relay
    /// does with `a/1`, but its answer comes back as a 502.
    async fn flaky_rendezvous(flaky: &[&str], lost: &[&str]) -> String {
        let slots = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
        let set = |slots: &[&str]| {
            Arc::new(Mutex::new(
                slots.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
            ))
        };
        let (flaky, lost, gone) = (set(flaky), set(lost), set(&[]));
        let app = axum::Router::new().fallback(
            move |method: Method, uri: Uri, body: axum::body::Bytes| {
                let (slots, flaky) = (slots.clone(), flaky.clone());
                let (lost, gone) = (lost.clone(), gone.clone());
                async move {
                    let path = uri.path();
                    if method == Method::POST && path.ends_with("/rendezvous") {
                        slots.lock().unwrap().insert("7/a/0".into(), body.to_vec());
                        let opened = RendezvousOpened { nameplate: 7 };
                        return (StatusCode::CREATED, axum::Json(opened)).into_response();
                    }
                    let slot = path.split("/rendezvous/").nth(1).unwrap_or("").to_owned();
                    match method {
                        Method::PUT => {
                            slots.lock().unwrap().insert(slot, body.to_vec());
                            StatusCode::NO_CONTENT.into_response()
                        }
                        Method::GET if flaky.lock().unwrap().remove(&slot) => {
                            StatusCode::BAD_GATEWAY.into_response()
                        }
                        // Only nameplate 7 is open; others are codes nobody shows.
                        Method::GET
                            if gone.lock().unwrap().contains(&slot) || !slot.starts_with("7/") =>
                        {
                            StatusCode::NOT_FOUND.into_response()
                        }
                        Method::GET
                            if lost.lock().unwrap().contains(&slot)
                                && slots.lock().unwrap().remove(&slot).is_some() =>
                        {
                            gone.lock().unwrap().insert(slot);
                            StatusCode::BAD_GATEWAY.into_response()
                        }
                        Method::GET => match slots.lock().unwrap().get(&slot) {
                            Some(bytes) => bytes.clone().into_response(),
                            None => StatusCode::NO_CONTENT.into_response(),
                        },
                        _ => StatusCode::NO_CONTENT.into_response(),
                    }
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// One bad gateway used to end `yacs join` with an error, and the code
    /// typed again then said someone else had used it.
    #[tokio::test]
    async fn a_code_exchange_rides_out_a_bad_gateway() {
        let relay = flaky_rendezvous(&["7/a/0", "7/b/0", "7/a/1"], &[]).await;
        let pairing = yacs_core::Pairing {
            channel_id: yacs_core::ChannelId::from_bytes([1; 32]),
            key: yacs_core::ChannelKey::from_bytes([2; 32]),
        };
        let client = Client::new(&relay, None, pairing).unwrap();
        let offer = client.offer_code().await.unwrap();
        let code = offer.code();

        let (outcome, invite) = tokio::join!(
            client.complete_code(offer, "My devices", "Mac"),
            join_with_code(&relay, &code, "Phone"),
        );
        let device = "Phone".to_owned();
        assert_eq!(outcome.unwrap(), CodeOutcome::Joined { device });
        let invite = invite.unwrap();
        assert_eq!(
            (invite.space_name.as_str(), invite.inviter.as_str()),
            ("My devices", "Mac")
        );
    }

    /// The invite was handed over but its answer lost: asking again finds
    /// the exchange closed, which is no reason to call the code wrong.
    #[tokio::test]
    async fn a_lost_invite_isnt_a_wrong_code() {
        let relay = flaky_rendezvous(&[], &["7/a/1"]).await;
        let pairing = yacs_core::Pairing {
            channel_id: yacs_core::ChannelId::from_bytes([1; 32]),
            key: yacs_core::ChannelKey::from_bytes([2; 32]),
        };
        let client = Client::new(&relay, None, pairing).unwrap();
        let offer = client.offer_code().await.unwrap();
        let code = offer.code();

        let (outcome, invite) = tokio::join!(
            client.complete_code(offer, "My devices", "Mac"),
            join_with_code(&relay, &code, "Phone"),
        );
        assert!(matches!(outcome, Ok(CodeOutcome::Joined { .. })));
        let err = invite.unwrap_err();
        assert!(matches!(err, Error::Server { status: 502, .. }), "{err}");
    }

    /// A code nobody shows is a wrong code, even when the first lookup hit
    /// a bad gateway: reading `a/0` doesn't close anything.
    #[tokio::test]
    async fn a_wrong_code_after_a_bad_gateway_is_still_wrong() {
        let relay = flaky_rendezvous(&["9/a/0"], &[]).await;
        let code = Code::generate(9).unwrap();
        let err = join_with_code(&relay, &code, "Phone").await.unwrap_err();
        assert!(matches!(err, Error::CodeNotFound), "{err}");
    }
}
