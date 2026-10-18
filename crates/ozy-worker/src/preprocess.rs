//! Deterministic image preprocessing for OCR, independent of Tesseract.
use image::{GrayImage, ImageFormat, Luma, imageops::FilterType};
use serde::Deserialize;

pub const MAX_PIXELS: u64 = 16_000_000;

/// Images are always converted to 8-bit grayscale before thresholding and OCR.
#[derive(Debug, Clone, Deserialize)]
pub struct Options {
    /// null, 0-255, or "otsu"
    #[serde(default)]
    pub threshold: serde_json::Value,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default = "seven")]
    pub psm: u8,
    #[serde(default)]
    #[cfg_attr(not(feature = "ocr"), allow(dead_code))]
    pub whitelist: String,
}
fn one() -> f64 {
    1.0
}
fn seven() -> u8 {
    7
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threshold: serde_json::Value::Null,
            scale: 1.0,
            psm: 7,
            whitelist: String::new(),
        }
    }
}

pub fn decode(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let format = match bytes {
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [0xFF, 0xD8, 0xFF, ..] => ImageFormat::Jpeg,
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => ImageFormat::WebP,
        _ => return Err("Unsupported image format; use PNG, JPEG or WebP".into()),
    };
    let reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let (width, height) = reader.into_dimensions().map_err(|_| "Unreadable image header".to_owned())?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS || width == 0 || height == 0 {
        return Err("Image dimensions exceed 16 megapixels".into());
    }
    image::load_from_memory_with_format(bytes, format).map_err(|_| "Image could not be decoded".into())
}

/// Otsu's method: the threshold that maximises between-class variance.
pub fn otsu(image: &GrayImage) -> u8 {
    let mut histogram = [0u64; 256];
    for Luma([v]) in image.pixels() {
        histogram[*v as usize] += 1;
    }
    let total: u64 = histogram.iter().sum();
    let sum_all: f64 = histogram.iter().enumerate().map(|(i, c)| i as f64 * *c as f64).sum();
    let (mut weight_bg, mut sum_bg, mut best, mut best_variance) = (0u64, 0f64, 0u8, -1f64);
    for (t, count) in histogram.iter().enumerate() {
        weight_bg += count;
        if weight_bg == 0 {
            continue;
        }
        let weight_fg = total - weight_bg;
        if weight_fg == 0 {
            break;
        }
        sum_bg += t as f64 * *count as f64;
        let mean_bg = sum_bg / weight_bg as f64;
        let mean_fg = (sum_all - sum_bg) / weight_fg as f64;
        let variance = weight_bg as f64 * weight_fg as f64 * (mean_bg - mean_fg).powi(2);
        if variance > best_variance {
            best_variance = variance;
            best = t as u8;
        }
    }
    best
}

pub struct Prepared {
    #[cfg_attr(not(feature = "ocr"), allow(dead_code))]
    pub image: GrayImage,
    pub original: (u32, u32),
    pub threshold: Option<u8>,
}

pub fn prepare(bytes: &[u8], options: &Options) -> Result<Prepared, String> {
    let decoded = decode(bytes)?;
    let original = (decoded.width(), decoded.height());
    let mut gray = decoded.to_luma8();
    if (options.scale - 1.0).abs() > f64::EPSILON {
        if !(0.25..=4.0).contains(&options.scale) {
            return Err("scale must be between 0.25 and 4".into());
        }
        let width = ((gray.width() as f64 * options.scale).round() as u32).max(1);
        let height = ((gray.height() as f64 * options.scale).round() as u32).max(1);
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err("Scaled image exceeds 16 megapixels".into());
        }
        gray = image::imageops::resize(&gray, width, height, FilterType::CatmullRom);
    }
    let threshold = match &options.threshold {
        serde_json::Value::String(s) if s == "otsu" => Some(otsu(&gray)),
        serde_json::Value::Number(n) => Some(n.as_u64().filter(|v| *v <= 255).ok_or("threshold must be 0-255")? as u8),
        serde_json::Value::Null => None,
        _ => return Err("threshold must be 0-255 or \"otsu\"".into()),
    };
    if let Some(t) = threshold {
        for Luma([v]) in gray.pixels_mut() {
            *v = if *v > t { 255 } else { 0 };
        }
    }
    Ok(Prepared {
        image: gray,
        original,
        threshold,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png(image: &GrayImage) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }
    #[test]
    fn otsu_separates_two_tones() {
        let image = GrayImage::from_fn(20, 10, |x, _| Luma([if x < 10 { 40 } else { 210 }]));
        let t = otsu(&image);
        assert!((40..210).contains(&t), "{t}");
    }
    #[test]
    fn prepare_scales_and_binarises() {
        let image = GrayImage::from_fn(40, 20, |x, _| Luma([if x < 20 { 30 } else { 220 }]));
        let options = Options {
            threshold: serde_json::json!("otsu"),
            scale: 2.0,
            ..Default::default()
        };
        let prepared = prepare(&png(&image), &options).unwrap();
        assert_eq!(prepared.image.dimensions(), (80, 40));
        assert_eq!(prepared.original, (40, 20));
        assert!(prepared.image.pixels().all(|Luma([v])| *v == 0 || *v == 255));
    }
    #[test]
    fn rejects_unknown_formats_and_bad_options() {
        assert!(decode(b"GIF89a....").is_err());
        let image = GrayImage::new(4, 4);
        assert!(
            prepare(
                &png(&image),
                &Options {
                    scale: 9.0,
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            prepare(
                &png(&image),
                &Options {
                    threshold: serde_json::json!(999),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}
