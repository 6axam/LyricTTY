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
        let key = format!("{:x}", Sha256::digest(url.as_bytes()));
        let path = self.cache.join(key);
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(art) = decode(&bytes) {
                return Ok(art);
            }
            let _ = fs::remove_file(&path);
        }
        let parsed = Url::parse(url)?;
        let bytes = match parsed.scheme() {
            "file" => {
                let p = parsed
                    .to_file_path()
                    .map_err(|_| anyhow::anyhow!("Invalid artwork file URL"))?;
                read_bounded_file(&p)?
            }
            "http" | "https" => {
                let response = self.client.get(parsed).send()?.error_for_status()?;
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
    let image = reader
        .decode()
        .context("Unsupported artwork image")?
        .thumbnail(512, 512)
        .to_rgb8();
    let palette = dominant_palette(&image);
    Ok(Artwork { image, palette })
}
pub fn dominant_palette(image: &RgbImage) -> Palette {
    let mut bins = [(0u64, 0u64, 0u64, 0u64); 512];
    for pixel in image.pixels() {
        let [r, g, b] = pixel.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        if max < 20 || min > 240 {
            continue;
        }
        let weight = 1 + u64::from(max - min) / 32;
        let index = ((r as usize / 32) * 64) + ((g as usize / 32) * 8) + (b as usize / 32);
        let bin = &mut bins[index];
        bin.0 += u64::from(r) * weight;
        bin.1 += u64::from(g) * weight;
        bin.2 += u64::from(b) * weight;
        bin.3 += weight;
    }
    let &(r, g, b, n) = bins.iter().max_by_key(|p| p.3).unwrap();
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
