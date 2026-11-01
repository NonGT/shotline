//! Reference images from the web, saved in the film's cache so a job can send them like a file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use shotline_core::Project;

const MAX: usize = 30 << 20;

pub fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// Downloads an image into `.shotline/cache/refs` and returns where it went.
pub fn image(p: &Project, url: &str) -> Result<PathBuf> {
    let rt = crate::plugins::runtime();
    let (bytes, kind) = rt.block_on(async {
        let r = reqwest::get(url)
            .await
            .with_context(|| format!("can't fetch {url}"))?;
        if !r.status().is_success() {
            bail!("{url} answered {}", r.status());
        }
        let kind = r
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = r.bytes().await?;
        Ok((bytes, kind))
    })?;
    if bytes.len() > MAX {
        bail!("{url} is over 30 MB");
    }
    let ext = match kind.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => match shotline_plugin::util::image_ext(&bytes) {
            Some(e) => e,
            None => bail!("{url} isn't a png, jpeg or webp image"),
        },
    };
    let dir = p.root.join(".shotline").join("cache").join("refs");
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(format!("{}.{ext}", name(url)));
    std::fs::write(&out, &bytes)?;
    Ok(out)
}

/// The URL's last part, safe for a file name, and the time so two fetches never collide.
fn name(url: &str) -> String {
    let last = url
        .split(['?', '#'])
        .next()
        .and_then(|u| u.trim_end_matches('/').rsplit('/').next())
        .map(|s| {
            Path::new(s)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default();
    let stem = shotline_core::slug::slugify(&last);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if stem.is_empty() {
        format!("ref-{t}")
    } else {
        format!("{stem}-{t}")
    }
}
