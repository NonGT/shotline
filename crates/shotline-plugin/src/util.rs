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
    fn data_urls() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.JPG");
        std::fs::write(&p, b"hi").unwrap();
        assert_eq!(data_url(&p, "x").unwrap(), "data:image/jpeg;base64,aGk=");
        let e = data_url(&dir.path().join("nope.png"), "references[3]").unwrap_err();
        assert_eq!(e.field.as_deref(), Some("references[3]"));
    }
}
