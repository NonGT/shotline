//! Small helpers plugins tend to need.

use std::path::Path;

use base64::Engine;
use shotline_protocol::{ErrorKind, PluginError};

pub fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// The extension image bytes really have, whatever the file was called: providers send JPEG or
/// WebP as often as PNG. None if it isn't one of those.
pub fn image_ext(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG") {
        Some("png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// A file as a `data:` URL, which most APIs take in place of an upload. `field` is where it goes
/// in the request, for the error if the file can't be read.
pub fn data_url(path: &Path, field: &str) -> Result<String, PluginError> {
    let bytes = std::fs::read(path).map_err(|e| {
        PluginError::new(ErrorKind::Validation, format!("{}: {e}", path.display())).field(field)
    })?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{};base64,{b64}", mime(path)))
}

/// A picture as a `data:` URL, shrunk to `side` px on its long edge and sent as JPEG when it's
/// big. Venice drops the connection on large video requests, and a video model doesn't need
/// more than its own frame size anyway.
pub fn picture_url(path: &Path, field: &str, side: u32) -> Result<String, PluginError> {
    let bytes = std::fs::read(path).map_err(|e| {
        PluginError::new(ErrorKind::Validation, format!("{}: {e}", path.display())).field(field)
    })?;
    let Ok(img) = image::load_from_memory(&bytes) else {
        return data_url(path, field);
    };
    if bytes.len() < 400_000 && img.width().max(img.height()) <= side {
        return data_url(path, field);
    }
    let img = if img.width().max(img.height()) > side {
        img.resize(side, side, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut jpg = vec![];
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 90)
        .encode_image(&img.to_rgb8())
        .map_err(|e| {
            PluginError::new(ErrorKind::Other, format!("{}: {e}", path.display())).field(field)
        })?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(jpg);
    Ok(format!("data:image/jpeg;base64,{b64}"))
}

/// An error with what caused it, since reqwest's "error sending request" alone says nothing.
pub fn why(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut at = e.source();
    while let Some(c) = at {
        let c_s = c.to_string();
        if !out.contains(&c_s) {
            out = format!("{out}: {c_s}");
        }
        at = c.source();
    }
    out
}

/// Splits ElevenLabs-style "[shout]" tags from the words.
pub fn delivery(text: &str) -> (Vec<&str>, String) {
    let mut how = vec![];
    let mut words = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        words.push_str(&rest[..open]);
        let tag = rest[open + 1..open + close].trim();
        if !tag.is_empty() {
            how.push(tag);
        }
        rest = &rest[open + close + 1..];
    }
    words.push_str(rest);
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    (how, words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_pictures_go_small() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image::RgbImage::from_fn(3000, 1500, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 7])
        })
        .save(&p)
        .unwrap();
        let url = picture_url(&p, "x", 1920).unwrap();
        let b64 = url.strip_prefix("data:image/jpeg;base64,").unwrap();
        let back = image::load_from_memory(
            &base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap(),
        )
        .unwrap();
        assert_eq!((back.width(), back.height()), (1920, 960));

        let small = dir.path().join("b.png");
        image::RgbImage::new(8, 8).save(&small).unwrap();
        assert!(
            picture_url(&small, "x", 1920)
                .unwrap()
                .starts_with("data:image/png;")
        );
    }

    #[test]
    fn why_says_the_cause() {
        let inner = std::io::Error::other("connection reset by peer");
        let outer = std::io::Error::other(Wrap(inner));
        assert_eq!(why(&outer), "sending: connection reset by peer");
    }

    #[derive(Debug)]
    struct Wrap(std::io::Error);
    impl std::fmt::Display for Wrap {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("sending")
        }
    }
    impl std::error::Error for Wrap {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn data_urls() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.JPG");
        std::fs::write(&p, b"hi").unwrap();
        assert_eq!(data_url(&p, "x").unwrap(), "data:image/jpeg;base64,aGk=");
        let e = data_url(&dir.path().join("nope.png"), "references[3]").unwrap_err();
        assert_eq!(e.field.as_deref(), Some("references[3]"));
    }
}
