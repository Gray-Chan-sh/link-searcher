//! Scan detection: page geometry from `/MediaBox` and full-page image coverage
//! via `pdfimages -list`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::poppler::{pdfimages_path, run_with_timeout};
fn object_number(o: &lopdf::Object) -> Option<f32> {
    o.as_float().ok().or_else(|| o.as_i64().ok().map(|v| v as f32))
}

/// Page size in points from `/MediaBox`, following `Parent` for inherited values.
pub(super) fn page_media_size(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> Option<(f32, f32)> {
    let mut id = page_id;
    for _ in 0..64 {
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(mb_obj) = dict.get(b"MediaBox")
            && let Ok((_, mb_obj)) = doc.dereference(mb_obj)
            && let Ok(mb) = mb_obj.as_array()
            && mb.len() == 4
            && let (Some(w), Some(h)) = (
                object_number(&mb[2]).zip(object_number(&mb[0])).map(|(a, b)| (a - b).abs()),
                object_number(&mb[3]).zip(object_number(&mb[1])).map(|(a, b)| (a - b).abs()),
            )
            && w > 0.0
            && h > 0.0
        {
            return Some((w, h));
        }
        match dict.get(b"Parent").and_then(lopdf::Object::as_reference) {
            Ok(parent) => id = parent,
            Err(_) => return None,
        }
    }
    None
}

/// Page `/Rotate` in degrees, normalized to `{0, 90, 180, 270}`, following
/// `Parent` for inherited values. `0` when absent/unparseable.
pub(super) fn page_rotate(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> i64 {
    let mut id = page_id;
    for _ in 0..64 {
        let Ok(dict) = doc.get_dictionary(id) else { break };
        if let Ok(rotate) = dict.get(b"Rotate").and_then(lopdf::Object::as_i64) {
            return rotate.rem_euclid(360);
        }
        match dict.get(b"Parent").and_then(lopdf::Object::as_reference) {
            Ok(parent) => id = parent,
            Err(_) => break,
        }
    }
    0
}

/// Whether any page is rotated 90°/270° — i.e. its content is stored sideways
/// relative to the page box (typical of copier scans fed in landscape).
///
/// `pdfimages` copies the raw embedded image and does **not** apply `/Rotate`,
/// so such pages reach the OCR engine on their side and come back as garbage
/// (measured on a real library: 89 of 92 low-quality PDFs are exactly these
/// rotated scans — judgments, rulings, complaints). `pdftoppm` bakes the
/// rotation in, so rotated documents must be rendered with it instead.
pub(super) fn pages_rotated(doc: &lopdf::Document) -> bool {
    doc.get_pages()
        .values()
        .any(|&id| matches!(page_rotate(doc, id), 90 | 270))
}

/// Parsed row of one `pdfimages -list` data line.
///
/// The listing's right-most four columns are always `x-ppi y-ppi size ratio`, so
/// the scan reads them from the **right** rather than hard-coding `f[12]/f[13]`
/// (the `object ID` + `interp` fields shift the left side across poppler builds).
/// Measured on a real library, `x-ppi` is legitimately `0` for whole archives
/// produced by IntSig/Foxit — the value is missing upstream, not mis-parsed.
struct ImageListRow<'a> {
    page: u32,
    #[allow(dead_code)] // 保留 `type` 列以便将来区分 image/stencil/smask
    kind: &'a str,
    width: u32,
    height: u32,
    enc: &'a str,
    x_ppi: f32,
    y_ppi: f32,
}

/// Fixed left columns before the trailing `x-ppi y-ppi size ratio` quartet:
/// `page num type width height color comp bpc enc interp` — `interp` is present
/// in poppler ≥ 0.65; the parser tolerates its absence by checking the shape.
const IMAGE_LIST_MIN_COLS: usize = 14;

/// Minimum `image-size / page-size` ratio, in both dimensions, for a full-page image.
const FULL_PAGE_IMAGE_COVERAGE: f32 = 0.8;

fn parse_image_list_row(line: &str) -> Option<ImageListRow<'_>> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < IMAGE_LIST_MIN_COLS {
        return None;
    }
    if !matches!(f[2], "image" | "stencil" | "smask") {
        return None;
    }
    let page = f[0].parse::<u32>().ok()?;
    let width = f[3].parse::<u32>().ok()?;
    let height = f[4].parse::<u32>().ok()?;
    let n = f.len();
    // Right-anchored: … x-ppi y-ppi size ratio
    let x_ppi = f[n - 4].parse::<f32>().unwrap_or(0.0);
    let y_ppi = f[n - 3].parse::<f32>().unwrap_or(0.0);
    Some(ImageListRow {
        page,
        kind: f[2],
        width,
        height,
        enc: f[8],
        x_ppi,
        y_ppi,
    })
}

/// 1-based pages holding an image covering ≥ `FULL_PAGE_IMAGE_COVERAGE` of the
/// page in both dimensions. `listing` is `pdfimages -list` output.
///
/// Coverage uses the image's physical size (`pixels / ppi`), and **falls back to
/// the raw pixel count against the page box** when the listing reports a
/// non-positive ppi (seen on IntSig/Foxit-produced scans, where the object id
/// absorbs the ppi column). Without the fallback those files look like "no
/// full-page image at all" and never reach OCR.
pub(super) fn full_page_image_pages(
    listing: &str,
    page_size: impl Fn(u32) -> Option<(f32, f32)>,
) -> HashSet<u32> {
    let mut pages = HashSet::new();
    for line in listing.lines() {
        let Some(row) = parse_image_list_row(line) else {
            continue;
        };
        let Some((pw, ph)) = page_size(row.page) else {
            continue;
        };
        if pw <= 0.0 || ph <= 0.0 {
            continue;
        }
        let (w_pt, h_pt) = if row.x_ppi > 0.0 && row.y_ppi > 0.0 {
            (row.width as f32 / row.x_ppi * 72.0, row.height as f32 / row.y_ppi * 72.0)
        } else {
            // ppi 不可用：按 1px = 1pt（72dpi）粗估。宁可误判为扫描件
            // （多跑一次 OCR）也不要漏掉整份扫描文档（正文全丢）。
            (row.width as f32, row.height as f32)
        };
        if w_pt >= FULL_PAGE_IMAGE_COVERAGE * pw && h_pt >= FULL_PAGE_IMAGE_COVERAGE * ph {
            pages.insert(row.page);
        }
    }
    pages
}

/// True when most pages carry a full-page image — i.e. a scan. Scans often ship
/// a synthetic text layer whose font-fallback glyphs are split into separate
/// runs, destroying reading order for lopdf/poppler, so they must be OCR'd.
pub(super) fn is_image_based_scan(path: &Path, doc: &lopdf::Document) -> bool {
    let Some(bin) = pdfimages_path() else {
        return false;
    };
    let mut cmd = crate::process::new(bin);
    cmd.arg("-list").arg(path);
    let Ok((Some(status), stdout)) = run_with_timeout(cmd, Duration::from_secs(60)) else {
        return false;
    };
    if !status.success() {
        return false;
    }
    let page_ids = doc.get_pages();
    if page_ids.is_empty() {
        return false;
    }
    let listing = String::from_utf8_lossy(&stdout);
    let full = full_page_image_pages(&listing, |p| {
        page_ids.get(&p).and_then(|&id| page_media_size(doc, id))
    });
    !full.is_empty() && full.len() * 2 >= page_ids.len()
}

/// Test-only projection of [`parse_image_list_row`] so the pdf.rs test module
/// (which cannot see the private struct) can assert the right-anchored columns.
#[cfg(test)]
pub(super) fn scan_tests_row(line: &str) -> Option<(u32, u32, f32, f32)> {
    parse_image_list_row(line).map(|r| (r.page, r.width, r.x_ppi, r.y_ppi))
}

/// Per-page image inventory from one `pdfimages -list` run.
///
/// `pages_with_image` counts pages carrying any image object (any size);
/// `enc_by_page` maps page → image encoding (`jpeg`, `ccitt`, `jbig2`, …) so
/// the OCR pass can extract JPEGs natively (`pdfimages -j`, ~0.1s/page) instead
/// of always re-encoding to PNG (~26s/page on a 3000×4000 scan).
#[derive(Debug)]
pub(super) struct ImageListInfo {
    /// Highest page number seen in the listing (0 when there are no images).
    pub total_pages: usize,
    pub pages_with_image: usize,
    pub enc_by_page: HashMap<u32, String>,
}

/// Cheap scan-coverage probe: one `pdfimages -list` (fast even on 100MB+ files —
/// 0.1–0.2s measured). `None` when pdfimages is unavailable or the listing
/// fails. Used to skip the futile per-page `pdfimages` OCR pass on digital/vector
/// PDFs that merely lack a text layer but carry almost no page images.
pub(super) fn image_list_info(path: &Path) -> Option<ImageListInfo> {
    let bin = pdfimages_path()?;
    let mut cmd = crate::process::new(bin);
    cmd.arg("-list").arg(path);
    let (Some(status), stdout) = run_with_timeout(cmd, Duration::from_secs(30)).ok()? else {
        return None;
    };
    if !status.success() {
        return None;
    }
    let listing = String::from_utf8_lossy(&stdout);
    let mut enc_by_page: HashMap<u32, String> = HashMap::new();
    let mut max_page = 0usize;
    for line in listing.lines() {
        let Some(row) = parse_image_list_row(line) else {
            continue;
        };
        enc_by_page.insert(row.page, row.enc.to_string());
        max_page = max_page.max(row.page as usize);
    }
    Some(ImageListInfo {
        total_pages: max_page,
        pages_with_image: enc_by_page.len(),
        enc_by_page,
    })
}
