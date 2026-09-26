//! The native clipboard. Reading collects every format YACS can carry;
//! writing puts all of a clip's formats on the clipboard at once, so the app
//! you paste into picks the richest one it understands.
//!
//! Both are blocking calls: run them off the async runtime.

use std::path::{Path, PathBuf};

use clipboard_rs::common::{RustImage, RustImageData};
use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext, ContentFormat};
use yacs_core::{ClipItem, Image};

const PNG_MIME: &str = "image/png";
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The platform's name for PNG data on the clipboard.
#[cfg(target_os = "macos")]
const NATIVE_PNG: &str = "public.png";
#[cfg(target_os = "windows")]
const NATIVE_PNG: &str = "PNG";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const NATIVE_PNG: &str = "image/png";

/// What's on the clipboard.
pub enum Copied {
    Items(Vec<ClipItem>),
    /// Files copied in Finder or Explorer, not read yet: they may be huge.
    Files(Vec<LocalFile>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalFile {
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

pub fn read() -> Result<Copied, String> {
    let ctx = context()?;
    // Files copied in Finder or Explorer. Their clipboard entry also carries
    // the file names as text and the file icon as an image, which are
    // useless to the receiver: only the files are sent.
    if ctx.has(ContentFormat::Files) {
        let files = ctx
            .get_files()
            .map_err(|e| format!("can't read the copied files: {e}"))?;
        let paths: Vec<PathBuf> = files.iter().map(|f| local_path(f)).collect();
        return local_files(&paths).map(Copied::Files);
    }
    let mut items = Vec::new();
    let string = |format, get: fn(&ClipboardContext) -> clipboard_rs::Result<String>| {
        ctx.has(format)
            .then(|| get(&ctx).ok())
            .flatten()
            .filter(|s| !s.is_empty())
    };
    if let Some(text) = string(ContentFormat::Text, Clipboard::get_text) {
        items.push(ClipItem::Text(text));
    }
    if let Some(html) = string(ContentFormat::Html, Clipboard::get_html) {
        items.push(ClipItem::Html(html));
    }
    if let Some(rtf) = string(ContentFormat::Rtf, Clipboard::get_rich_text) {
        items.push(ClipItem::Rtf(rtf));
    }
    if let Some(image) = read_image(&ctx) {
        items.push(ClipItem::Image(image));
    }

    if items.is_empty() {
        return Err("The clipboard is empty, or holds nothing YACS can send.".into());
    }
    Ok(Copied::Items(items))
}

fn read_image(ctx: &ClipboardContext) -> Option<Image> {
    if !ctx.has(ContentFormat::Image) {
        return None;
    }
    // Most sources offer PNG already: pass it through instead of decoding and
    // re-encoding a possibly huge screenshot.
    if let Ok(data) = ctx.get_buffer(NATIVE_PNG) {
        if data.starts_with(PNG_MAGIC) {
            return Some(png(data));
        }
    }
    let encoded = ctx.get_image().and_then(|image| image.to_png());
    match encoded {
        Ok(buffer) => Some(png(buffer.get_bytes().to_vec())),
        Err(e) => {
            tracing::warn!(error = %e, "can't read the clipboard image");
            None
        }
    }
}

/// Linux hands out `file://` URIs; macOS and Windows plain paths.
fn local_path(file: &str) -> PathBuf {
    url::Url::parse(file)
        .ok()
        .filter(|url| url.scheme() == "file")
        .and_then(|url| url.to_file_path().ok())
        .unwrap_or_else(|| PathBuf::from(file))
}

fn local_files(paths: &[PathBuf]) -> Result<Vec<LocalFile>, String> {
    paths
        .iter()
        .map(|path| {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let metadata =
                std::fs::metadata(path).map_err(|e| format!("can't read {name}: {e}"))?;
            if metadata.is_dir() {
                return Err(format!(
                    "{name} is a folder. YACS sends files, not folders; zip it first."
                ));
            }
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Ok(LocalFile {
                path: path.clone(),
                name,
                mime: mime.essence_str().to_owned(),
                size: metadata.len(),
            })
        })
        .collect()
}

/// "report.pdf is 30 MB" or "These 3 files are 30 MB".
pub fn files_too_big(files: &[LocalFile], limit: u64) -> String {
    let total = files.iter().map(|f| f.size).sum();
    let what = match files {
        [file] => format!("{} is", file.name),
        _ => format!("These {} files are", files.len()),
    };
    format!(
        "{what} {}, but the relay takes up to {} per clip. Update the relay to send bigger files.",
        size(total),
        size(limit)
    )
}

/// Over what the space's plan takes, which no relay update changes.
pub fn files_over_plan(files: &[LocalFile], limit: u64) -> String {
    let total = files.iter().map(|f| f.size).sum();
    let what = match files {
        [file] => format!("{} is", file.name),
        _ => format!("These {} files are", files.len()),
    };
    format!(
        "{what} {}, but this space takes up to {} per clip.",
        size(total),
        size(limit)
    )
}

/// The files' contents, to send in the clip itself.
pub fn read_files(files: Vec<LocalFile>) -> Result<Vec<ClipItem>, String> {
    files
        .into_iter()
        .map(|file| {
            let data =
                std::fs::read(&file.path).map_err(|e| format!("can't read {}: {e}", file.name))?;
            Ok(ClipItem::File(yacs_core::File {
                name: file.name,
                mime: file.mime,
                data,
            }))
        })
        .collect()
}

pub fn size(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes.div_ceil(1000)),
        1_000_000..1_000_000_000 => format!("{:.0} MB", bytes as f64 / 1e6),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

/// Writes the clip's files into `dir` (reusing one that's already there with
/// the same content) and returns their paths.
pub fn save_files(items: &[ClipItem], dir: &Path) -> Result<Vec<PathBuf>, String> {
    let files = items.iter().filter_map(|item| match item {
        ClipItem::File(file) => Some(file),
        _ => None,
    });
    std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    files.map(|file| save_file(file, dir)).collect()
}

/// `name` in `dir`, then `name (1)`, `name (2)`, … keeping the extension.
fn candidates<'a>(dir: &'a Path, name: &'a str) -> impl Iterator<Item = PathBuf> + 'a {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (0..).map(move |n| match n {
        0 => dir.join(name),
        _ => dir.join(format!("{stem} ({n}){ext}")),
    })
}

/// The first of [`candidates`] that doesn't exist yet.
pub fn free_path(dir: &Path, name: &str) -> PathBuf {
    candidates(dir, name)
        .find(|path| !path.exists())
        .expect("some name is always free")
}

fn save_file(file: &yacs_core::File, dir: &Path) -> Result<PathBuf, String> {
    let name = file.safe_name();
    for candidate in candidates(dir, &name) {
        let mut options = std::fs::OpenOptions::new();
        match options.write(true).create_new(true).open(&candidate) {
            Ok(mut out) => {
                return std::io::Write::write_all(&mut out, &file.data)
                    .map(|()| candidate)
                    .map_err(|e| format!("can't save {name}: {e}"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let same = std::fs::metadata(&candidate)
                    .is_ok_and(|m| m.len() == file.data.len() as u64)
                    && std::fs::read(&candidate).is_ok_and(|d| d == file.data);
                if same {
                    return Ok(candidate);
                }
            }
            Err(e) => return Err(format!("can't save {name}: {e}")),
        }
    }
    unreachable!("some name is always free")
}

/// Puts files on the clipboard, as if they were copied in Finder or Explorer.
pub fn write_files(paths: &[PathBuf]) -> Result<(), String> {
    let paths = paths.iter().map(|p| p.display().to_string()).collect();
    context()?
        .set_files(paths)
        .map_err(|e| format!("couldn't write to the clipboard: {e}"))
}

pub fn write(items: &[ClipItem]) -> Result<(), String> {
    let ctx = context()?;
    let image = items.iter().find_map(|item| match item {
        ClipItem::Image(image) => Some(image),
        _ => None,
    });

    // clipboard-rs empties the Windows clipboard when it sets an image, which
    // would drop the other formats. An image on its own goes through its
    // image path, which also adds a bitmap for older apps like Paint; next to
    // text, it's added as plain PNG data, which rich-text apps read.
    #[cfg(target_os = "windows")]
    if let (Some(image), 1) = (image, items.len()) {
        let decoded = RustImageData::from_bytes(&image.data).map_err(|e| e.to_string())?;
        return ctx
            .set_image(decoded)
            .map_err(|e| format!("couldn't write to the clipboard: {e}"));
    }

    let mut contents = Vec::new();
    for item in items {
        match item {
            ClipItem::Text(text) => contents.push(ClipboardContent::Text(text.clone())),
            ClipItem::Html(html) => contents.push(ClipboardContent::Html(html.clone())),
            ClipItem::Rtf(rtf) => contents.push(ClipboardContent::Rtf(rtf.clone())),
            // Handled below: the clipboard holds one image.
            ClipItem::Image(_) => {}
            // See `save_files` and `write_files`.
            ClipItem::File(_) | ClipItem::Stream(_) => {}
        }
    }
    if let Some(image) = image {
        contents.push(ClipboardContent::Other(NATIVE_PNG.into(), to_png(image)?));
    }
    if contents.is_empty() {
        return Err("this clip has nothing to copy".into());
    }
    ctx.set(contents)
        .map_err(|e| format!("couldn't write to the clipboard: {e}"))
}

fn context() -> Result<ClipboardContext, String> {
    ClipboardContext::new().map_err(|e| format!("can't open the clipboard: {e}"))
}

fn png(data: Vec<u8>) -> Image {
    Image {
        mime: PNG_MIME.into(),
        data,
    }
}

/// Clips from other senders (e.g. a phone) may carry JPEG or another format.
fn to_png(image: &Image) -> Result<Vec<u8>, String> {
    if image.data.starts_with(PNG_MAGIC) {
        return Ok(image.data.clone());
    }
    RustImageData::from_bytes(&image.data)
        .and_then(|decoded| decoded.to_png())
        .map(|buffer| buffer.get_bytes().to_vec())
        .map_err(|e| format!("can't convert the {} image: {e}", image.mime))
}

#[cfg(test)]
mod tests {
    use image::ImageFormat;

    use super::*;

    fn encoded(format: image::ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(3, 2, image::Rgb([200, 40, 90]))
            .write_to(&mut out, format)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn png_passes_through_and_other_formats_are_converted() {
        let png = encoded(ImageFormat::Png);
        let as_is = to_png(&Image {
            mime: PNG_MIME.into(),
            data: png.clone(),
        });
        assert_eq!(as_is.unwrap(), png);

        let converted = to_png(&Image {
            mime: "image/jpeg".into(),
            data: encoded(ImageFormat::Jpeg),
        })
        .unwrap();
        assert!(converted.starts_with(PNG_MAGIC));
        let decoded = image::load_from_memory(&converted).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (3, 2));

        let err = to_png(&Image {
            mime: "image/heic".into(),
            data: vec![1, 2, 3],
        })
        .unwrap_err();
        assert!(err.contains("image/heic"), "{err}");
    }

    #[test]
    fn copied_files_are_listed_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, data: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, data).unwrap();
            path
        };
        let pdf = file("report.pdf", b"%PDF-1.7 hello");
        let bin = file("blob", &[0, 1, 2]);

        let files = local_files(&[pdf.clone(), bin.clone()]).unwrap();
        assert_eq!(
            (
                files[0].name.as_str(),
                files[0].mime.as_str(),
                files[0].size
            ),
            ("report.pdf", "application/pdf", 14)
        );
        assert_eq!(files[1].mime, "application/octet-stream");
        let items = read_files(files.clone()).unwrap();
        let [ClipItem::File(a), ClipItem::File(_)] = &items[..] else {
            panic!("{items:?}");
        };
        assert_eq!(a.data, b"%PDF-1.7 hello");

        assert_eq!(
            files_too_big(&files[..1], 10),
            "report.pdf is 1 KB, but the relay takes up to 1 KB per clip. Update the relay to send bigger files."
        );
        assert!(files_too_big(&files, 10).starts_with("These 2 files are"));
        let err = local_files(&[dir.path().to_owned()]).unwrap_err();
        assert!(err.contains("is a folder"), "{err}");
        let err = local_files(&[dir.path().join("gone.txt")]).unwrap_err();
        assert!(err.starts_with("can't read gone.txt"), "{err}");
        assert_eq!(size(4_700_000_000), "4.7 GB");
    }

    #[test]
    fn linux_file_uris_become_paths() {
        #[cfg(unix)]
        assert_eq!(
            local_path("file:///home/me/My%20Notes.txt"),
            PathBuf::from("/home/me/My Notes.txt")
        );
        assert_eq!(
            local_path("/Users/me/a.txt"),
            PathBuf::from("/Users/me/a.txt")
        );
        assert_eq!(
            local_path(r"C:\Users\me\a.txt"),
            PathBuf::from(r"C:\Users\me\a.txt")
        );
    }

    #[test]
    fn saved_files_get_free_names_and_are_reused() {
        let dir = tempfile::tempdir().unwrap();
        let item = |name: &str, data: &[u8]| {
            ClipItem::File(yacs_core::File {
                name: name.into(),
                mime: "text/plain".into(),
                data: data.to_vec(),
            })
        };
        let saved = save_files(&[item("../notes.txt", b"one")], dir.path()).unwrap();
        assert_eq!(saved, [dir.path().join("notes.txt")]);
        // The same file again: nothing new.
        assert_eq!(
            save_files(&[item("notes.txt", b"one")], dir.path()).unwrap(),
            saved
        );
        // Same name, other content: numbered.
        let other = save_files(
            &[item("notes.txt", b"two"), item("README", b"x")],
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            other,
            [dir.path().join("notes (1).txt"), dir.path().join("README")]
        );
        assert_eq!(std::fs::read(&other[0]).unwrap(), b"two");
    }

    /// Replaces whatever is on the clipboard, so it only runs on request:
    /// `cargo test -p yacs-desktop -- --ignored clipboard`
    #[test]
    #[ignore]
    fn round_trips_through_the_real_clipboard() {
        let items = vec![
            ClipItem::Text("hello from yacs".into()),
            ClipItem::Html("<b>hello</b> from yacs".into()),
            ClipItem::Rtf(r"{\rtf1\ansi {\b hello} from yacs}".into()),
            ClipItem::Image(png(encoded(ImageFormat::Png))),
        ];
        write(&items).unwrap();
        let Copied::Items(back) = read().unwrap() else {
            panic!("not items");
        };
        assert_eq!(back[0], items[0]);
        assert!(matches!(&back[1], ClipItem::Html(h) if h.contains("<b>hello</b>")));
        assert!(matches!(&back[2], ClipItem::Rtf(r) if r.contains("hello")));
        assert!(matches!(&back[3], ClipItem::Image(i) if i.data.starts_with(PNG_MAGIC)));

        write(&items[3..]).unwrap();
        let Copied::Items(back) = read().unwrap() else {
            panic!("not items");
        };
        assert!(
            matches!(&back[..], [ClipItem::Image(_)]),
            "{} items",
            back.len()
        );

        let dir = tempfile::tempdir().unwrap();
        let file = ClipItem::File(yacs_core::File {
            name: "yacs test.txt".into(),
            mime: "text/plain".into(),
            data: b"hello".to_vec(),
        });
        let saved = save_files(std::slice::from_ref(&file), dir.path()).unwrap();
        write_files(&saved).unwrap();
        let Copied::Files(files) = read().unwrap() else {
            panic!("not files");
        };
        assert_eq!(read_files(files).unwrap(), [file]);
    }
}
