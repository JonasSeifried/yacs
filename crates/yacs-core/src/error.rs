/// Everything that can go wrong in the YACS protocol layer.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("pairing phrase is empty")]
    EmptyPhrase,
    #[error("invalid channel id")]
    InvalidChannelId,
    #[error("envelope is too short")]
    Truncated,
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed: wrong key or tampered data")]
    Decrypt,
    #[error("malformed payload")]
    Malformed,
    #[error("system random number generator failed")]
    Rng,
}

pub type Result<T> = core::result::Result<T, Error>;
