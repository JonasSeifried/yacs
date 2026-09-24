//! `yacs update`: replace this binary with the one from the latest release.
//! Release binaries are signed with the desktop updater's key, and nothing
//! is replaced unless the signature checks out.

use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;

const REPO: &str = "JonasSeifried/yacs";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// From tauri.conf.json, see build.rs.
const PUBKEY: &str = env!("YACS_UPDATER_PUBKEY");

/// The release asset built for this platform.
const ASSET: Option<&str> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("yacs-cli-linux-x86_64")
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    Some("yacs-cli-linux-aarch64")
} else if cfg!(target_os = "macos") {
    Some("yacs-cli-macos")
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    Some("yacs-cli-windows-x86_64.exe")
} else {
    None
};

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

pub async fn run(check_only: bool) -> Result<()> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("yacs-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()?;
    let release: Release = http
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("couldn't look up the latest release")?
        .json()
        .await
        .context("couldn't look up the latest release")?;
    let latest = release.tag_name.trim_start_matches('v');
    if !is_newer(latest, CURRENT) {
        eprintln!("yacs {CURRENT} is up to date.");
        return Ok(());
    }
    if check_only {
        println!("yacs {latest} is available (this is {CURRENT}); run `yacs update`.");
        return Ok(());
    }
    let Some(asset) = ASSET else {
        bail!("yacs {latest} is out, but there's no prebuilt binary for this platform");
    };

    eprintln!("Downloading yacs {latest}…");
    let url = format!(
        "https://github.com/{REPO}/releases/download/{}/{asset}",
        release.tag_name
    );
    let signature = download(&http, &format!("{url}.sig")).await.context(
        "couldn't download the signature (releases before 0.2.2 have none; install those by hand)",
    )?;
    let binary = download(&http, &url).await?;
    verify(PUBKEY, &binary, &signature, asset)?;

    let mut file = tempfile::NamedTempFile::new().context("saving the download")?;
    file.write_all(&binary).context("saving the download")?;
    self_replace::self_replace(file.path()).context(
        "couldn't replace yacs; if it's installed system-wide, run the update as that user (e.g. with sudo)",
    )?;
    eprintln!("Updated yacs {CURRENT} → {latest}.");
    Ok(())
}

async fn download(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let res = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    if !res.status().is_success() {
        bail!("downloading {url}: {}", res.status());
    }
    Ok(res
        .bytes()
        .await
        .with_context(|| format!("downloading {url}"))?
        .to_vec())
}

/// Tauri's key and `.sig` files are base64 of minisign's text format. The
/// signed comment names the file, so a different signed binary (another
/// platform's) doesn't pass either.
fn verify(pubkey: &str, data: &[u8], signature: &[u8], asset: &str) -> Result<()> {
    let key = PublicKey::decode(&unbase64(pubkey.as_bytes())?).context("bad updater key")?;
    let signature =
        Signature::decode(&unbase64(signature)?).context("the signature file is damaged")?;
    key.verify(data, &signature, false)
        .context("the download's signature doesn't match; not installing it")?;
    let file = signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("file:"));
    if file != Some(asset) {
        bail!("the download is signed, but for another file; not installing it");
    }
    Ok(())
}

fn unbase64(text: &[u8]) -> Result<String> {
    let bytes = STANDARD
        .decode(text.trim_ascii())
        .context("expected base64")?;
    String::from_utf8(bytes).context("expected text")
}

/// Plain `major.minor.patch` comparison; `latest` only has full releases.
fn is_newer(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let v = v.split(['-', '+']).next()?;
        let mut parts = v.split('.').map(|p| p.parse().ok());
        Some((parts.next()??, parts.next()??, parts.next()??))
    }
    match (parse(latest), parse(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made with a throwaway key: `tauri signer generate` + `tauri signer sign`.
    const TEST_KEY: &str = include_str!("../tests/fixtures/update-test.pub");
    const TEST_DATA: &[u8] = include_bytes!("../tests/fixtures/update-test.bin");
    const TEST_SIG: &[u8] = include_bytes!("../tests/fixtures/update-test.bin.sig");

    #[test]
    fn accepts_only_the_signed_file() {
        verify(TEST_KEY, TEST_DATA, TEST_SIG, "data.bin").unwrap();

        let mut tampered = TEST_DATA.to_vec();
        tampered[0] ^= 1;
        let err = verify(TEST_KEY, &tampered, TEST_SIG, "data.bin").unwrap_err();
        assert!(err.to_string().contains("doesn't match"), "{err:#}");

        let err = verify(TEST_KEY, TEST_DATA, TEST_SIG, "yacs-cli-macos").unwrap_err();
        assert!(err.to_string().contains("another file"), "{err:#}");

        // Signed by a different key than the one that's trusted.
        assert!(verify(PUBKEY, TEST_DATA, TEST_SIG, "data.bin").is_err());
        assert!(verify(TEST_KEY, TEST_DATA, b"garbage", "data.bin").is_err());
    }

    #[test]
    fn the_real_updater_key_decodes() {
        PublicKey::decode(&unbase64(PUBKEY.as_bytes()).unwrap()).unwrap();
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.2", "0.2.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
        assert!(!is_newer("nonsense", "0.2.1"));
    }
}
