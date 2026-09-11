use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
struct FixtureEntry {
    image: String,
    expected_text: String,
    max_cer: f64,
}

fn render_image(text: &str, out_path: &Path) {
    use ab_glyph::{FontArc, PxScale};

    let char_count = text.chars().count();
    let width = 30u32 + (char_count as u32) * 40;
    let height = 120u32;
    let mut img = image::RgbaImage::new(width, height);

    for pixel in img.pixels_mut() {
        *pixel = image::Rgba([255u8, 255u8, 255u8, 255u8]);
    }

    let font_data: &[u8] = include_bytes!("../../assets/Arial Unicode.ttf");
    if let Ok(font) = FontArc::try_from_slice(font_data) {
        let scale = PxScale::from(32.0);
        imageproc::drawing::draw_text_mut(
            &mut img,
            image::Rgba([0u8, 0u8, 0u8, 255u8]),
            10,
            40,
            scale,
            &font,
            text,
        );
    }

    let dyn_img = image::DynamicImage::from(img);
    dyn_img
        .write_to(
            &mut std::io::BufWriter::new(std::fs::File::create(out_path).unwrap()),
            image::ImageFormat::Png,
        )
        .unwrap();
}

#[test]
fn ocr_eval_synthetic() {
    let fixtures_dir: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scripts/eval/ocr_fixtures/synthetic");

    let manifest_path = fixtures_dir.join("manifest.jsonl");
    if !manifest_path.exists() {
        eprintln!("manifest.jsonl not found at {:?}, skipping", manifest_path);
        return;
    }

    let contents = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
        eprintln!("failed to read manifest.jsonl: {e}, skipping");
        return String::new();
    });

    let entries: Vec<FixtureEntry> = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    if entries.is_empty() {
        eprintln!("no valid entries in manifest.jsonl, skipping");
        return;
    }

    let engine = link_searcher_lib::extractor::ocr::preferred_engine(None);
    let image_path = fixtures_dir.join("ocr_eval_probe.png");
    render_image("probe", &image_path);
    match link_searcher_lib::extractor::ocr::ocr_image_with_engine(&image_path, &engine, "chi_sim") {
        Ok(_) => {}
        Err(e) => {
            eprintln!("OCR engine probe failed ({e}), skipping ocr_eval_synthetic");
            let _ = std::fs::remove_file(&image_path);
            return;
        }
    }
    let _ = std::fs::remove_file(&image_path);

    let mut total_cer = 0.0f64;
    let mut count = 0u32;

    for entry in &entries {
        let img_path = fixtures_dir.join(&entry.image);
        if !img_path.exists() {
            render_image(&entry.expected_text, &img_path);
        }

        let actual = link_searcher_lib::extractor::ocr::ocr_image_with_engine(
            &img_path,
            &engine,
            "chi_sim",
        )
        .unwrap_or_default();

        let item_cer = link_searcher_lib::extractor::quality::cer(&entry.expected_text, &actual);
        total_cer += item_cer;
        count += 1;

        let status = if item_cer <= entry.max_cer {
            "PASS"
        } else {
            "FAIL"
        };
        eprintln!(
            "[{status}] image={image} expected=\"{expected}\" actual=\"{actual}\" cer={cer:.4} max_cer={max}",
            image = entry.image,
            expected = entry.expected_text,
            actual = actual.trim(),
            cer = item_cer,
            max = entry.max_cer
        );

        assert!(
            item_cer <= entry.max_cer,
            "CER {} > {} for image '{}' (expected: {:?}, actual: {:?})",
            item_cer,
            entry.max_cer,
            entry.image,
            entry.expected_text,
            actual.trim()
        );
    }

    let avg_cer = if count > 0 {
        total_cer / count as f64
    } else {
        0.0
    };
    eprintln!("============================================");
    eprintln!("  OCR CER Evaluation — {count} items");
    eprintln!("  Average CER: {avg_cer:.4}");
    eprintln!("============================================");
}
