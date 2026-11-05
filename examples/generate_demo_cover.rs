//! Reproducible original demo artwork; no remote media or branding.
use image::{Rgb, RgbImage};
fn main() -> anyhow::Result<()> {
    let mut image = RgbImage::new(512, 512);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let nx = x as f32 / 512.0;
        let ny = y as f32 / 512.0;
        let glow = (-((nx - 0.56).powi(2) * 4.0 + (ny - 0.38).powi(2) * 3.0) * 5.0).exp();
        let grain = ((x.wrapping_mul(137).wrapping_add(y.wrapping_mul(211))) % 17) as f32 / 17.0;
        let mut color = [
            18.0 + glow * 90.0 + grain * 2.0,
            31.0 + glow * 88.0 + grain * 2.0,
            28.0 + glow * 24.0 + grain * 2.0,
        ];
        let sun = ((nx - 0.56).powi(2) + (ny - 0.39).powi(2)).sqrt();
        if sun < 0.14 {
            let edge = ((0.14 - sun) * 100.0).clamp(0.0, 1.0);
            for (v, t) in color.iter_mut().zip([206.0, 205.0, 121.0]) {
                *v += (t - *v) * edge;
            }
        }
        let hill1 = 0.61 + (nx * 7.0).sin() * 0.08 + (nx * 3.0).cos() * 0.03;
        let hill2 = 0.78 + (nx * 6.0 + 1.5).sin() * 0.09;
        if ny > hill1 {
            color = [23.0, 53.0, 42.0];
        }
        if ny > hill2 {
            color = [11.0, 29.0, 27.0];
        }
        let vignette = (1.0 - ((nx - 0.5).powi(2) + (ny - 0.5).powi(2)) * 0.9).max(0.5);
        *pixel = Rgb(color.map(|v| (v * vignette) as u8));
    }
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/demo-cover.png".into());
    image.save(output)?;
    Ok(())
}
