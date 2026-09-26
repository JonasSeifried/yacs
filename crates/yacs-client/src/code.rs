//! Typed codes over the relay's rendezvous (see `yacs_core::code` for the
//! exchange, `yacs_core::api` for the routes).

use reqwest::{Response, Url};
use yacs_core::api::{ENVELOPE_CONTENT_TYPE, RendezvousOpened};
use yacs_core::{Code, CodeInviter, CodeJoiner, Invite};

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
            match wait(self.send(self.http.get(answer.clone())).await).await? {
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

    let message = loop {
        match wait(checked(http.get(url("a/0")?).send().await?).await).await? {
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
        match wait(checked(http.get(url("a/1")?).send().await?).await).await? {
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
