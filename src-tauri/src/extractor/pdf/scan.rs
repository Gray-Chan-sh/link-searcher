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

/// Minimum `image-size / page-size` ratio, in both dimensions, for a full-page image.
const FULL_PAGE_IMAGE_COVERAGE: f32 = 0.8;

/// 1-based pages holding an image covering ≥ `FULL_PAGE_IMAGE_COVERAGE` of the
/// page in both dimensions. `listing` is `pdfimages -list` output.
pub(super) fn full_page_image_pages(
    listing: &str,
    page_size: impl Fn(u32) -> Option<(f32, f32)>,
) -> HashSet<u32> {
    let mut pages = HashSet::new();
    for line in listing.lines() {
        // Columns: page num type width height color comp bpc enc interp object id x-ppi y-ppi size ratio
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 || !matches!(f[2], "image" | "stencil" | "smask") {
            continue;
        }
        let (Ok(page), Ok(w), Ok(h), Ok(xppi), Ok(yppi)) = (
            f[0].parse::<u32>(),
            f[3].parse::<u32>(),
            f[4].parse::<u32>(),
            f[12].parse::<f32>(),
            f[13].parse::<f32>(),
        ) else {
            continue;
        };
        if xppi <= 0.0 || yppi <= 0.0 {
            continue;
        }
        let Some((pw, ph)) = page_size(page) else {
            continue;
        };
        let (w_pt, h_pt) = (w as f32 / xppi * 72.0, h as f32 / yppi * 72.0);
        if w_pt >= FULL_PAGE_IMAGE_COVERAGE * pw && h_pt >= FULL_PAGE_IMAGE_COVERAGE * ph {
            pages.insert(page);
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
        // Columns: page num type width height color comp bpc enc interp object ID x-ppi y-ppi size ratio
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 || !matches!(f[2], "image" | "stencil" | "smask") {
            continue;
        }
        let Ok(page) = f[0].parse::<u32>() else {
            continue;
        };
        enc_by_page.insert(page, f[8].to_string());
        max_page = max_page.max(page as usize);
    }
    Some(ImageListInfo {
        total_pages: max_page,
        pages_with_image: enc_by_page.len(),
        enc_by_page,
    })
}
