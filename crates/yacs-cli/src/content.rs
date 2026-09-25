//! What `yacs send` sends: a file, an argument or stdin, as one clip item.
//! Receiving machines put text and images on the clipboard, and save files.

use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use yacs_core::{ClipItem, File, Image};

/// The item, and a short description for the "sent …" line. Text and
/// images are sent as such unless `as_file`; everything else as a file.
pub fn from_file(path: &Path, as_file: bool) -> Result<(ClipItem, String)> {
    if path == Path::new("-") {
        return from_stdin();
    }
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "no such file: {}\n(to send text, use --text \"…\" or pipe it in)",
            path.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let label = format!("{name} ({})", crate::human_size(data.len() as u64));
    if as_file {
        return Ok((file(path, name, data), label));
    }
    if let Some(mime) = image_type(&data) {
        let size = data.len();
        return Ok((
            ClipItem::Image(Image {
                mime: mime.into(),
                data,
            }),
            format!("{name} ({})", crate::human_size(size as u64)),
        ));
    }
    let text = match String::from_utf8(data) {
        Ok(text) => text,
        Err(e) => return Ok((file(path, name, e.into_bytes()), label)),
    };
    let text = without_final_newline(text);
    let label = format!("{name} ({})", crate::human_size(text.len() as u64));
    Ok((ClipItem::Text(text), label))
}

fn file(path: &Path, name: String, data: Vec<u8>) -> ClipItem {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    ClipItem::File(File {
        name,
        mime: mime.essence_str().to_owned(),
        data,
    })
}

pub fn from_stdin() -> Result<(ClipItem, String)> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!("nothing to send: pass a file, --text \"…\", or pipe something in");
    }
    let mut data = Vec::new();
    stdin.read_to_end(&mut data).context("reading stdin")?;
    if let Some(mime) = image_type(&data) {
        let label = format!("image ({})", crate::human_size(data.len() as u64));
        return Ok((
            ClipItem::Image(Image {
                mime: mime.into(),
                data,
            }),
            label,
        ));
    }
    let text = String::from_utf8(data).context("stdin isn't text or an image")?;
    let text = without_final_newline(text);
    let label = format!("text ({})", crate::human_size(text.len() as u64));
    Ok((ClipItem::Text(text), label))
}

pub fn from_text(text: String) -> (ClipItem, String) {
    let label = format!("text ({})", crate::human_size(text.len() as u64));
    (ClipItem::Text(text), label)
}

/// Files and command output end in a newline that nobody means to paste (in
/// a terminal it would run the line), so drop one, like `$(…)` does.
fn without_final_newline(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

/// Recognized by content, not by file name.
fn image_type(data: &[u8]) -> Option<&'static str> {
    match data {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, rest @ ..] if rest.starts_with(b"WEBP") => {
            Some("image/webp")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_one_final_newline() {
        assert_eq!(without_final_newline("key\n".into()), "key");
        assert_eq!(without_final_newline("key\r\n".into()), "key");
        assert_eq!(without_final_newline("a\n\n".into()), "a\n");
        assert_eq!(without_final_newline("key".into()), "key");
    }

    #[test]
    fn sends_text_images_and_everything_else_as_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str, data: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, data).unwrap();
            path
        };
        let notes = path("notes.txt", b"hi\n");
        assert_eq!(
            from_file(&notes, false).unwrap().0,
            ClipItem::Text("hi".into())
        );
        let png = path("shot.png", b"\x89PNG\r\n\x1a\n");
        assert!(matches!(
            from_file(&png, false).unwrap().0,
            ClipItem::Image(_)
        ));

        let (item, label) = from_file(&path("report.pdf", b"%PDF\xff"), false).unwrap();
        let ClipItem::File(file) = item else {
            panic!("{item:?}")
        };
        assert_eq!(
            (file.name.as_str(), file.mime.as_str()),
            ("report.pdf", "application/pdf")
        );
        assert_eq!(file.data, b"%PDF\xff");
        assert_eq!(label, "report.pdf (5 B)");

        let ClipItem::File(file) = from_file(&notes, true).unwrap().0 else {
            panic!()
        };
        assert_eq!(
            (file.mime.as_str(), &file.data[..]),
            ("text/plain", &b"hi\n"[..])
        );
    }

    #[test]
    fn recognizes_images_by_content() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(image_type(b"\xFF\xD8\xFF\xE0"), Some("image/jpeg"));
        assert_eq!(image_type(b"GIF89a"), Some("image/gif"));
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_type(b"ssh-ed25519 AAAA"), None);
        assert_eq!(image_type(b""), None);
    }
}
