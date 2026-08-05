use std::io::Cursor;

use quick_xml::Reader;
use quick_xml::events::Event;

const MAX_INLINE_SVG_BYTES: usize = 1_000_000;
const MAX_AVATAR_BYTES: usize = 1_000_000;

/// A verified media format that the web UI can embed inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerifiedMedia {
    /// A passive image format that uses an image element.
    Image(&'static str),
    /// A passive video format that uses a video element.
    Video(&'static str),
}

impl VerifiedMedia {
    /// Return the canonical MIME type detected from the verified bytes.
    pub(crate) const fn content_type(self) -> &'static str {
        match self {
            Self::Image(content_type) | Self::Video(content_type) => content_type,
        }
    }
}

/// Verify that media bytes match a MIME type the web UI embeds inline.
///
/// The accepted image formats are AVIF, BMP, GIF, ICO, JPEG, PNG, SVG, TIFF,
/// and WebP. The accepted video formats are MP4 and WebM.
pub(crate) fn verify_browser_media(declared_mime: &str, data: &[u8]) -> Option<VerifiedMedia> {
    let detected = detect_browser_media(data)?;
    let declared_mime = declared_mime.parse::<mime::Mime>().ok()?;
    if !mime_types_match(declared_mime.essence_str(), detected.content_type()) {
        return None;
    }
    Some(detected)
}

/// Verify that retained avatar bytes match their declared browser image MIME
/// type.
pub(crate) fn verify_avatar(declared_mime: &str, data: &[u8]) -> Option<VerifiedMedia> {
    if MAX_AVATAR_BYTES < data.len() {
        return None;
    }
    verify_browser_media(declared_mime, data)
        .filter(|media| matches!(media, VerifiedMedia::Image(_)))
}

fn detect_browser_media(data: &[u8]) -> Option<VerifiedMedia> {
    if is_svg(data) {
        return Some(VerifiedMedia::Image("image/svg+xml"));
    }

    infer::get(data).and_then(|kind| match kind.mime_type() {
        "image/avif" => Some(VerifiedMedia::Image("image/avif")),
        "image/bmp" => Some(VerifiedMedia::Image("image/bmp")),
        "image/gif" => Some(VerifiedMedia::Image("image/gif")),
        "image/jpeg" => Some(VerifiedMedia::Image("image/jpeg")),
        "image/png" => Some(VerifiedMedia::Image("image/png")),
        "image/tiff" => Some(VerifiedMedia::Image("image/tiff")),
        "image/vnd.microsoft.icon" => Some(VerifiedMedia::Image("image/vnd.microsoft.icon")),
        "image/webp" => Some(VerifiedMedia::Image("image/webp")),
        "video/mp4" => Some(VerifiedMedia::Video("video/mp4")),
        "video/webm" => Some(VerifiedMedia::Video("video/webm")),
        _ => None,
    })
}

fn mime_types_match(declared_mime: &str, detected_mime: &str) -> bool {
    declared_mime == detected_mime
        || (declared_mime == "image/x-icon" && detected_mime == "image/vnd.microsoft.icon")
}

fn is_svg(data: &[u8]) -> bool {
    if MAX_INLINE_SVG_BYTES < data.len() {
        return false;
    }

    let mut reader = Reader::from_reader(Cursor::new(data));
    let mut buffer = Vec::new();
    let mut is_svg_root = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::DocType(_)) | Err(_) => return false,
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) if is_svg_root.is_none() => {
                is_svg_root = Some(element.local_name().as_ref() == b"svg");
            }
            Ok(Event::Eof) => return is_svg_root == Some(true),
            Ok(_) => {}
        }
        buffer.clear();
    }
}

#[cfg(test)]
mod tests;
