//! The native clipboard. Reading collects every format YACS can carry;
//! writing puts all of a clip's formats on the clipboard at once, so the app
//! you paste into picks the richest one it understands.
//!
//! Both are blocking calls: run them off the async runtime.

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

pub fn read() -> Result<Vec<ClipItem>, String> {
    let ctx = context()?;
    if ctx.has(ContentFormat::Files) {
        return Err("Copied files can't be sent yet, only text, rich text and images.".into());
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
    Ok(items)
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
        let png = encoded(image::ImageFormat::Png);
        let as_is = to_png(&Image {
            mime: PNG_MIME.into(),
            data: png.clone(),
        });
        assert_eq!(as_is.unwrap(), png);

        let converted = to_png(&Image {
            mime: "image/jpeg".into(),
            data: encoded(image::ImageFormat::Jpeg),
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

    /// Replaces whatever is on the clipboard, so it only runs on request:
    /// `cargo test -p yacs-desktop -- --ignored clipboard`
    #[test]
    #[ignore]
    fn round_trips_through_the_real_clipboard() {
        let items = vec![
            ClipItem::Text("hello from yacs".into()),
            ClipItem::Html("<b>hello</b> from yacs".into()),
            ClipItem::Rtf(r"{\rtf1\ansi {\b hello} from yacs}".into()),
            ClipItem::Image(png(encoded(image::ImageFormat::Png))),
        ];
        write(&items).unwrap();
        let back = read().unwrap();
        assert_eq!(back[0], items[0]);
        assert!(matches!(&back[1], ClipItem::Html(h) if h.contains("<b>hello</b>")));
        assert!(matches!(&back[2], ClipItem::Rtf(r) if r.contains("hello")));
        assert!(matches!(&back[3], ClipItem::Image(i) if i.data.starts_with(PNG_MAGIC)));

        write(&items[3..]).unwrap();
        let back = read().unwrap();
        assert!(
            matches!(&back[..], [ClipItem::Image(_)]),
            "{} items",
            back.len()
        );
    }
}
