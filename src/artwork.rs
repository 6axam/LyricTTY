use crate::theme::{from_artwork, Palette};
use anyhow::{bail, Context, Result};
use image::{ImageReader, RgbImage};
use reqwest::{blocking::Client, Url};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Clone)]
pub struct Artwork {
    pub image: RgbImage,
    pub palette: Palette,
}
impl Artwork {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        decode(bytes)
    }
}
pub struct ArtworkProvider {
    client: Client,
    cache: PathBuf,
}
impl ArtworkProvider {
    pub fn new(cache: PathBuf) -> Result<Self> {
        fs::create_dir_all(&cache)?;
        Ok(Self {
            client: Client::builder()
                .user_agent("LyricTTY/0.1 (https://github.com/6axam/LyricTTY)")
                .timeout(Duration::from_secs(12))
                .build()?,
            cache,
        })
    }
    pub fn load(&self, url: &str) -> Result<Artwork> {
        let parsed = Url::parse(url)?;
        if let Some(better) = higher_quality_url(&parsed) {
            if let Ok(art) = self.load_url(&better) {
                // Some CDNs return a tiny placeholder for unavailable large images.
                if art.image.width().min(art.image.height()) >= 300 {
                    return Ok(art);
                }
            }
        }
        self.load_url(&parsed)
    }
    fn load_url(&self, parsed: &Url) -> Result<Artwork> {
        let url = parsed.as_str();
        let key = format!("{:x}", Sha256::digest(url.as_bytes()));
        let path = self.cache.join(key);
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(art) = decode(&bytes) {
                return Ok(art);
            }
            let _ = fs::remove_file(&path);
        }
        let bytes = match parsed.scheme() {
            "file" => {
                let p = parsed
                    .to_file_path()
                    .map_err(|_| anyhow::anyhow!("Invalid artwork file URL"))?;
                read_bounded_file(&p)?
            }
            "http" | "https" => {
                let response = self.client.get(parsed.clone()).send()?.error_for_status()?;
                if response.content_length().is_some_and(|s| s > MAX_BYTES) {
                    bail!("Artwork too large");
                }
                let mut b = Vec::new();
                response.take(MAX_BYTES + 1).read_to_end(&mut b)?;
                b
            }
            _ => bail!("Unsupported artwork scheme"),
        };
        let art = decode(&bytes)?;
        if let Ok(mut tmp) = tempfile::NamedTempFile::new_in(&self.cache) {
            use std::io::Write;
            if tmp.write_all(&bytes).is_ok() {
                let _ = tmp.persist(&path);
            }
        }
        Ok(art)
    }
}
fn higher_quality_url(url: &Url) -> Option<Url> {
    let host = url.host_str()?;
    let path = url.path();
    let mut better = url.clone();
    if host == "googleusercontent.com"
        || host.ends_with(".googleusercontent.com")
        || host == "ggpht.com"
        || host.ends_with(".ggpht.com")
    {
        let (base, options) = path.rsplit_once('=')?;
        if !options.starts_with(['w', 'h', 's']) {
            return None;
        }
        better.set_path(&format!("{base}=w1200-h1200-l90-rj"));
    } else if host == "i.scdn.co" {
        let suffix = path
            .strip_prefix("/image/ab67616d00004851")
            .or_else(|| path.strip_prefix("/image/ab67616d00001e02"))?;
        better.set_path(&format!("/image/ab67616d0000b273{suffix}"));
    } else if (host == "ytimg.com" || host.ends_with(".ytimg.com")) && path.starts_with("/vi/") {
        let (base, name) = path.rsplit_once('/')?;
        if !matches!(
            name,
            "default.jpg" | "mqdefault.jpg" | "hqdefault.jpg" | "sddefault.jpg"
        ) {
            return None;
        }
        better.set_path(&format!("{base}/maxresdefault.jpg"));
    } else {
        return None;
    }
    (better != *url).then_some(better)
}
fn read_bounded_file(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<Artwork> {
    if bytes.len() as u64 > MAX_BYTES {
        bail!("Artwork too large");
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().context("Unsupported artwork image")?;
    let image = if image.width() > 1536 || image.height() > 1536 {
        image.resize(1536, 1536, image::imageops::FilterType::Lanczos3)
    } else {
        image
    }
    .to_rgb8();
    let palette = dominant_palette(&image);
    Ok(Artwork { image, palette })
}
pub fn dominant_palette(image: &RgbImage) -> Palette {
    let mut sums = (0u64, 0u64, 0u64, 0u64);
    for pixel in image.pixels() {
        let [r, g, b] = pixel.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let weight = 1 + u64::from(max - min) / 32;
        sums.0 += u64::from(r) * weight;
        sums.1 += u64::from(g) * weight;
        sums.2 += u64::from(b) * weight;
        sums.3 += weight;
    }
    let (r, g, b, n) = sums;
    match (r.checked_div(n), g.checked_div(n), b.checked_div(n)) {
        (Some(r), Some(g), Some(b)) => from_artwork((r as u8, g as u8, b as u8)),
        _ => Palette::default(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dominant_red_generates_dark_readable_red_theme() {
        let im = RgbImage::from_pixel(10, 10, image::Rgb([240, 20, 20]));
        let p = dominant_palette(&im);
        assert!(crate::theme::contrast(p.fg, p.bg) > 7.0);
        assert!(matches!(p.accent,ratatui::style::Color::Rgb(r,g,_) if r>g));
    }
    #[test]
    fn caches_file_artwork_and_handles_corrupt_cache() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("cover.png");
        RgbImage::from_pixel(4, 4, image::Rgb([20, 100, 20]))
            .save(&f)
            .unwrap();
        let url = Url::from_file_path(&f).unwrap().to_string();
        let p = ArtworkProvider::new(dir.path().join("cache")).unwrap();
        assert!(p.load(&url).is_ok());
        fs::remove_file(f).unwrap();
        assert!(p.load(&url).is_ok());
    }
}
