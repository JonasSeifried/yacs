//! Typed invite codes like `7-tulip-apple`, for when a link is awkward (a
//! second computer): the number picks a rendezvous on the relay, the words are
//! the password of a SPAKE2 exchange that ends with the invite.
//!
//! ```text
//! inviter (in the space)                        joiner (typed the code)
//! a/0: SPAKE2 message       ──── relay ────▶
//!                           ◀──── relay ────   b/0: SPAKE2 message + its device name,
//!                                                    sealed with the agreed key
//! a/1: the invite, sealed   ──── relay ────▶
//!      with the agreed key
//! ```
//!
//! The relay (or anyone) gets one guess per code: the relay takes one answer
//! per rendezvous, and an inviter that can't open the answer gives the code
//! up and shows a new one. Guessing offline isn't possible, since SPAKE2
//! messages reveal nothing about the password.
//!
//! Words come from the EFF Short Wordlist 1 (CC BY 3.0 US,
//! <https://www.eff.org/dice>), without "yo-yo", whose hyphen would clash
//! with the separators: two words are log2(1295²) ≈ 20.7 bits.

use core::fmt;
use core::str::FromStr;
use std::sync::LazyLock;

use hkdf::Hkdf;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroize;

use crate::error::{Error, Result};
use crate::invite::{Invite, open_invite, seal_invite};
use crate::sealed;

/// Bound into the SPAKE2 exchange, so these messages can't be replayed elsewhere.
const IDENTITY: &[u8] = b"yacs/v2/code";
const HKDF_INFO_JOINER: &[u8] = b"yacs/v2/code/joiner";
const HKDF_INFO_INVITER: &[u8] = b"yacs/v2/code/inviter";
/// How long a relay keeps a rendezvous: the inviter shows a new code after that.
pub const CODE_TTL_SECS: u64 = 10 * 60;
/// Nameplates go from 1 up to this.
pub const MAX_NAMEPLATE: u16 = 9999;

static WORDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    include_str!("eff_short_wordlist.txt")
        .lines()
        .filter(|w| !w.contains('-'))
        .collect()
});

/// `nameplate-word-word`. The nameplate isn't secret; the words are.
#[derive(Clone, PartialEq, Eq)]
pub struct Code {
    nameplate: u16,
    words: [&'static str; 2],
}

impl Code {
    /// Two random words for the rendezvous the relay opened at `nameplate`.
    pub fn generate(nameplate: u16) -> Result<Self> {
        Ok(Self {
            nameplate,
            words: [random_word()?, random_word()?],
        })
    }

    pub fn nameplate(&self) -> u16 {
        self.nameplate
    }

    /// Just the words: the relay only picks the nameplate once it has the
    /// inviter's first message. The nameplate is bound into the sealed parts.
    fn password(&self) -> Password {
        Password::new(format!("{}-{}", self.words[0], self.words[1]))
    }
}

fn random_word() -> Result<&'static str> {
    let n = WORDS.len() as u32;
    let limit = u32::MAX - u32::MAX % n;
    loop {
        let x = getrandom::u32().map_err(|_| Error::Rng)?;
        if x < limit {
            return Ok(WORDS[(x % n) as usize]);
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}-{}", self.nameplate, self.words[0], self.words[1])
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Code({}-<redacted>)", self.nameplate)
    }
}

/// Forgiving, since it's typed: any case, spaces or hyphens between the parts.
impl FromStr for Code {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim().to_lowercase();
        let parts: Vec<&str> = s
            .split(|c: char| c == '-' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        let [nameplate, first, second] = parts[..] else {
            return Err(Error::InvalidCode);
        };
        let nameplate: u16 = nameplate.parse().map_err(|_| Error::InvalidCode)?;
        if !(1..=MAX_NAMEPLATE).contains(&nameplate) {
            return Err(Error::InvalidCode);
        }
        let word = |w: &str| {
            WORDS
                .iter()
                .find(|known| **known == w)
                .copied()
                .ok_or(Error::InvalidCode)
        };
        Ok(Self {
            nameplate,
            words: [word(first)?, word(second)?],
        })
    }
}

/// Could this be a code rather than a link? For fields that take either.
pub fn looks_like_code(input: &str) -> bool {
    let input = input.trim();
    input.chars().next().is_some_and(|c| c.is_ascii_digit()) && !input.contains("://")
}

/// The key both sides agree on when the code matches.
struct Agreed([u8; 32]);

impl Agreed {
    fn new(spake_key: &[u8]) -> Self {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::new(None, spake_key)
            .expand(b"yacs/v2/code/key", &mut out)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        Self(out)
    }

    fn part(&self, info: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.0)
            .expand(info, &mut out)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    }
}

impl Drop for Agreed {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The side that shows the code.
pub struct CodeInviter {
    spake: Spake2<Ed25519Group>,
    words: [&'static str; 2],
}

impl CodeInviter {
    /// Picks the code's words. Returns the message for `a/0`, which opens the
    /// rendezvous; its nameplate completes the code (see [`code`](Self::code)).
    pub fn start() -> Result<(Self, Vec<u8>)> {
        Ok(Self::start_with(&Code::generate(1)?))
    }

    fn start_with(code: &Code) -> (Self, Vec<u8>) {
        let (spake, message) = Spake2::start_symmetric(&code.password(), &Identity::new(IDENTITY));
        let inviter = Self {
            spake,
            words: code.words,
        };
        (inviter, message)
    }

    /// The code to show, once the relay opened the rendezvous at `nameplate`.
    pub fn code(&self, nameplate: u16) -> Code {
        Code {
            nameplate,
            words: self.words,
        }
    }

    /// Like [`start`](Self::start), with a fixed RNG. Only for test vectors.
    #[doc(hidden)]
    pub fn start_with_rng(code: &Code, rng: impl rand_core::CryptoRng) -> (Self, Vec<u8>) {
        let (spake, message) =
            Spake2::start_symmetric_with_rng(&code.password(), &Identity::new(IDENTITY), rng);
        let inviter = Self {
            spake,
            words: code.words,
        };
        (inviter, message)
    }

    /// The joiner's answer from `b/0`. If they typed the right code, returns
    /// their device name and the key to seal the invite (for `a/1`) with.
    /// [`Error::Decrypt`] means the wrong code: give it up and show a new one.
    pub fn finish(self, nameplate: u16, answer: &[u8]) -> Result<(String, CodeKey)> {
        let (&len, rest) = answer.split_first().ok_or(Error::Truncated)?;
        if rest.len() < usize::from(len) {
            return Err(Error::Truncated);
        }
        let (message, sealed_name) = rest.split_at(usize::from(len));
        let spake_key = self.spake.finish(message).map_err(|_| Error::Decrypt)?;
        let key = CodeKey {
            agreed: Agreed::new(&spake_key),
            nameplate,
        };
        let name = sealed::open(&key.agreed.part(HKDF_INFO_JOINER), &key.aad(), sealed_name)?;
        let name = String::from_utf8(name).map_err(|_| Error::Malformed)?;
        Ok((name, key))
    }
}

/// The side that typed the code.
pub struct CodeJoiner {
    spake: Spake2<Ed25519Group>,
    message: Vec<u8>,
    nameplate: u16,
}

impl CodeJoiner {
    pub fn start(code: &Code) -> Self {
        let (spake, message) = Spake2::start_symmetric(&code.password(), &Identity::new(IDENTITY));
        Self {
            spake,
            message,
            nameplate: code.nameplate,
        }
    }

    /// Like [`start`](Self::start), with a fixed RNG. Only for test vectors.
    #[doc(hidden)]
    pub fn start_with_rng(code: &Code, rng: impl rand_core::CryptoRng) -> Self {
        let (spake, message) =
            Spake2::start_symmetric_with_rng(&code.password(), &Identity::new(IDENTITY), rng);
        Self {
            spake,
            message,
            nameplate: code.nameplate,
        }
    }

    /// From the inviter's message in `a/0`: the answer for `b/0`, and the key
    /// to open the invite in `a/1` with. It opens only if the code matched.
    pub fn answer(self, message: &[u8], device_name: &str) -> Result<(Vec<u8>, CodeKey)> {
        let spake_key = self.spake.finish(message).map_err(|_| Error::Decrypt)?;
        let key = CodeKey {
            agreed: Agreed::new(&spake_key),
            nameplate: self.nameplate,
        };
        let sealed_name = sealed::seal(
            &key.agreed.part(HKDF_INFO_JOINER),
            &key.aad(),
            device_name.as_bytes(),
        )?;
        let len = u8::try_from(self.message.len()).expect("SPAKE2 messages are 33 bytes");
        let mut answer = Vec::with_capacity(1 + self.message.len() + sealed_name.len());
        answer.push(len);
        answer.extend_from_slice(&self.message);
        answer.extend_from_slice(&sealed_name);
        Ok((answer, key))
    }
}

/// What a code exchange agreed on: the invite goes from inviter to joiner under it.
pub struct CodeKey {
    agreed: Agreed,
    nameplate: u16,
}

impl CodeKey {
    pub fn seal_invite(&self, invite: &Invite) -> Result<Vec<u8>> {
        seal_invite(&self.agreed.part(HKDF_INFO_INVITER), &self.aad(), invite)
    }

    pub fn open_invite(&self, sealed: &[u8]) -> Result<Invite> {
        open_invite(&self.agreed.part(HKDF_INFO_INVITER), &self.aad(), sealed)
    }

    fn aad(&self) -> Vec<u8> {
        [IDENTITY, &self.nameplate.to_be_bytes()].concat()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::Pairing;

    fn invite() -> Invite {
        Invite::new("Home", "MacBook", None, &Pairing::from_root(&[5; 32]))
    }

    #[test]
    fn wordlist_is_complete() {
        assert_eq!(WORDS.len(), 1295);
        assert_eq!(WORDS.iter().collect::<HashSet<_>>().len(), 1295);
        assert_eq!((WORDS[0], WORDS[1294]), ("acid", "zoom"));
    }

    #[test]
    fn codes_print_and_parse_forgivingly() {
        let code = Code::generate(7).unwrap();
        let s = code.to_string();
        assert!(s.starts_with("7-"), "{s}");
        assert_eq!(s.parse::<Code>(), Ok(code.clone()));
        let sloppy = format!("  {} ", s.to_uppercase().replace('-', " "));
        assert_eq!(sloppy.parse::<Code>(), Ok(code.clone()));
        assert_eq!(format!("{code:?}"), "Code(7-<redacted>)");

        for bad in [
            "",
            "7",
            "7-acid",
            "acid-acorn",
            "0-acid-acorn",
            "10000-acid-acorn",
            "7-acid-notaword",
            "7-acid-acorn-acre",
        ] {
            assert_eq!(bad.parse::<Code>(), Err(Error::InvalidCode), "{bad}");
        }
        assert!(looks_like_code(" 7-tulip-apple"));
        assert!(!looks_like_code("https://clip.example.com/#join=v2.x"));
        assert!(!looks_like_code("guitar"));
    }

    #[test]
    fn the_right_code_hands_the_invite_over() {
        let (inviter, message) = CodeInviter::start().unwrap();
        let code = inviter.code(42);
        let joiner = CodeJoiner::start(&code.to_string().parse().unwrap());
        let (answer, joiner_key) = joiner.answer(&message, "Anna's iPhone").unwrap();
        let (name, inviter_key) = inviter.finish(42, &answer).unwrap();
        assert_eq!(name, "Anna's iPhone");
        let sealed = inviter_key.seal_invite(&invite()).unwrap();
        assert_eq!(joiner_key.open_invite(&sealed).unwrap(), invite());
    }

    #[test]
    fn a_wrong_code_fails_on_both_sides() {
        let code = Code::generate(42).unwrap();
        let mut wrong = Code::generate(42).unwrap();
        while wrong == code {
            wrong = Code::generate(42).unwrap();
        }
        let (inviter, message) = CodeInviter::start_with(&code);
        let (answer, joiner_key) = CodeJoiner::start(&wrong)
            .answer(&message, "Mallory")
            .unwrap();
        assert!(matches!(inviter.finish(42, &answer), Err(Error::Decrypt)));

        // A right code for another nameplate fails too.
        let (inviter, message) = CodeInviter::start_with(&code);
        let (answer, _) = CodeJoiner::start(&code).answer(&message, "Anna").unwrap();
        assert!(matches!(inviter.finish(43, &answer), Err(Error::Decrypt)));

        // Nor does the joiner accept an invite sealed under another key.
        let (inviter, message) = CodeInviter::start_with(&code);
        let (answer, _) = CodeJoiner::start(&code).answer(&message, "Anna").unwrap();
        let (_, right_key) = inviter.finish(42, &answer).unwrap();
        let sealed = right_key.seal_invite(&invite()).unwrap();
        assert!(matches!(
            joiner_key.open_invite(&sealed),
            Err(Error::Decrypt)
        ));
    }

    #[test]
    fn rejects_malformed_answers() {
        for answer in [&[][..], &[33, 1, 2], &[0; 40]] {
            let (inviter, _) = CodeInviter::start().unwrap();
            assert!(inviter.finish(1, answer).is_err(), "{answer:?}");
        }
    }
}
