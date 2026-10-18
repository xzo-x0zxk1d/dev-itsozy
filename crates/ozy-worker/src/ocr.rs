//! Tesseract recognition through the `tesseract` Rust bindings (feature `ocr`).
use crate::preprocess::{Options, Prepared};
use tesseract::{PageSegMode, Tesseract};

fn page_seg_mode(psm: u8) -> PageSegMode {
    match psm {
        3 => PageSegMode::PsmAuto,
        6 => PageSegMode::PsmSingleBlock,
        8 => PageSegMode::PsmSingleWord,
        11 => PageSegMode::PsmSparseText,
        13 => PageSegMode::PsmRawLine,
        _ => PageSegMode::PsmSingleLine,
    }
}

/// Blocking: call from `spawn_blocking`.
pub fn recognize(prepared: &Prepared, options: &Options) -> Result<(String, f32), String> {
    let image = &prepared.image;
    let datapath = std::env::var("TESSDATA_PREFIX").ok();
    let mut engine =
        Tesseract::new(datapath.as_deref(), Some("eng")).map_err(|_| "Tesseract could not initialise (is eng.traineddata installed?)")?;
    if !options.whitelist.is_empty() {
        engine = engine
            .set_variable("tessedit_char_whitelist", &options.whitelist)
            .map_err(|_| "Invalid whitelist")?;
    }
    engine.set_page_seg_mode(page_seg_mode(options.psm));
    let width = image.width() as i32;
    let mut engine = engine
        .set_frame(image.as_raw(), width, image.height() as i32, 1, width)
        .map_err(|_| "Image frame rejected")?
        .set_source_resolution(300)
        .recognize()
        .map_err(|_| "Recognition failed")?;
    let text = engine.get_text().map_err(|_| "Recognised text was not UTF-8")?;
    Ok((text.trim().to_owned(), engine.mean_text_conf() as f32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preprocess::prepare;

    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/ocr-sample.png");

    #[test]
    fn recognises_fixture_text_after_otsu() {
        let options = Options {
            threshold: serde_json::json!("otsu"),
            scale: 2.0,
            psm: 7,
            ..Default::default()
        };
        let prepared = prepare(SAMPLE, &options).unwrap();
        let (text, confidence) = recognize(&prepared, &options).unwrap();
        assert!(text.contains("4821"), "recognised {text:?}");
        assert!(text.to_uppercase().contains("OZY"), "recognised {text:?}");
        assert!(confidence > 50.0, "confidence {confidence}");
    }

    #[test]
    fn whitelist_restricts_characters() {
        let options = Options {
            threshold: serde_json::json!("otsu"),
            scale: 2.0,
            psm: 7,
            whitelist: "0123456789".into(),
        };
        let prepared = prepare(SAMPLE, &options).unwrap();
        let (text, _) = recognize(&prepared, &options).unwrap();
        assert!(text.chars().all(|c| c.is_ascii_digit() || c.is_whitespace()), "{text:?}");
        assert!(text.contains("4821"), "{text:?}");
    }
}
