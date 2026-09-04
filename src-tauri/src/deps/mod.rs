//! Runtime dependency detection and installation.
//!
//! Link-Searcher keeps the release bundle small: heavy models (PaddleOCR,
//! BGE, FunASR) are NOT embedded. The first-run wizard (`get_setup_status` /
//! `install_dep`) downloads what the user picks from mirrors into
//! `<data_dir>/models/`, with progress events emitted to the frontend.
//!
//! Download strategy: canonical source is GitHub Releases (see `catalog`),
//! and the download layer can prepend a China-friendly mirror (e.g. ghproxy)
//! so the default path works without a VPN.
//!
//! Sources are plain URL prefixes. The exact files are pinned by name + sha256
//! in [`crate::deps::catalog`] so a truncated/corrupt download is never
//! mistaken for a ready model.

pub mod catalog;
pub mod commands;
pub mod download;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::state::AppState;

/// A dependency that the first-run wizard (or the in-app dependency center)
/// can install. `available` means "ready to use right now on this machine".
#[derive(Debug, Clone, Serialize)]
pub struct DepStatus {
    /// Stable id: `paddleocr` / `bge-small` / `funasr` / `ffmpeg` / `poppler` / `tesseract`.
    pub id: String,
    pub name: String,
    pub available: bool,
    /// Whether this dep blocks entering the main UI when missing.
    pub recommended: bool,
    /// Approximate download size in bytes (0 = system-provided, not downloaded).
    pub size_bytes: u64,
    /// Short human hint shown in the wizard.
    pub hint: String,
    /// Files that must exist for this dep to count as ready.
    pub required_files: Vec<String>,
}

/// A snapshot of every tracked dependency, plus a global "all recommended
/// deps satisfied" flag used by the frontend to decide whether to show the
/// setup wizard gate.
#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub deps: Vec<DepStatus>,
    pub all_recommended_ready: bool,
    pub data_dir: String,
}

/// Aggregate a per-dep "is it ready" check. Kept here (not in `catalog`) so
/// the catalog stays pure data + easy to unit-test.
pub fn current_status(state: &AppState) -> SetupStatus {
    let data_dir = state.data_dir.clone();
    let mut deps = Vec::new();
    let mut all_recommended_ready = true;

    for def in catalog::all() {
        let available = catalog::is_ready(&def, &data_dir);
        if def.recommended && !available {
            all_recommended_ready = false;
        }
        deps.push(DepStatus {
            id: def.id.to_string(),
            name: def.name.to_string(),
            available,
            recommended: def.recommended,
            size_bytes: def.size_bytes,
            hint: def.hint.to_string(),
            required_files: def
                .files
                .iter()
                .map(|f| f.local.to_string())
                .collect(),
        });
    }

    SetupStatus {
        deps,
        all_recommended_ready,
        data_dir: data_dir.display().to_string(),
    }
}

/// Convenience wrapper around [`catalog::is_ready`] for code that only has the
/// data dir.
pub fn is_ready(id: &str, data_dir: &Path) -> bool {
    catalog::all()
        .into_iter()
        .find(|d| d.id == id)
        .map(|d| catalog::is_ready(&d, data_dir))
        .unwrap_or(false)
}

/// Locate the dev checkout model dir (`src-tauri/models`) when running from a
/// source tree (used as a fast-path so `tauri dev` needs no download).
pub fn dev_models_dir() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR is `.../src-tauri`; models live in `src-tauri/models`.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
    dir.is_dir().then_some(dir)
}
