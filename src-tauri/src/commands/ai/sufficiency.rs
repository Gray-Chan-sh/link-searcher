//! Deterministic evidence-sufficiency checks for progressive retrieval.
//!
//! The retrieval loop must decide whether the current evidence can answer the
//! question *before* spending another LLM round. This module keeps that
//! decision pure and testable: callers derive [`SufficiencySignals`] from the
//! retrieval/verification layer, and this module turns them into a
//! [`SufficiencyReport`] with concrete expansion suggestions.
//!
//! Deliberately no LLM and no DB access here. `min_rerank_score` is supplied by
//! the caller (calibrated per model), and coverage gaps are supplied by the
//! scan ledger, so this logic stays deterministic and cheap.

/// Why the current evidence is not sufficient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GapReason {
    /// No candidate documents at all.
    NoHits,
    /// Top rerank score is below the calibrated threshold and no lexical or
    /// vector channel is strong.
    LowScore,
    /// A query entity does not appear in any candidate evidence.
    EntityMissing(String),
    /// A required slot (amount, date, report number, ...) is absent.
    SlotMissing(String),
    /// A comparison question has evidence for only one side.
    CompareSideMissing(String),
    /// Full-coverage task has not processed every document in scope.
    CoverageGap { processed: usize, total: usize },
    /// The generated answer cites evidence that cannot be verified.
    CitationUnsupported,
}

/// A bounded expansion the caller may apply in the next retrieval round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expansion {
    /// Add deterministic query variants (aliases, normalized forms, synonyms).
    QueryVariant,
    /// Increase BM25/vector candidate depth.
    IncreaseTopK,
    /// Add neighbouring chunks around the current hits.
    NeighborChunks,
    /// Run an exact/regex match inside the current scope.
    ExactMatch,
    /// Retry outside the current scope. The caller must only use this for
    /// inferred/global scopes; explicit scope is a hard boundary.
    GlobalFallback,
    /// Ask the user for the missing scope/object/slot.
    Clarify,
}

/// Inputs derived by the retrieval layer.
#[derive(Debug, Clone)]
pub(crate) struct SufficiencySignals {
    pub hit_count: usize,
    pub top_rerank_score: Option<f32>,
    /// Calibrated per model; `0.0` disables the low-score check.
    pub min_rerank_score: f32,
    pub has_strong_lexical: bool,
    pub has_strong_vector: bool,
    pub missing_entities: Vec<String>,
    pub missing_slots: Vec<String>,
    /// A human-readable side name for a one-sided comparison.
    pub compare_side_missing: Option<String>,
    pub coverage: Option<(usize, usize)>,
    pub citation_unsupported: bool,
}

impl Default for SufficiencySignals {
    fn default() -> Self {
        Self {
            hit_count: 0,
            top_rerank_score: None,
            min_rerank_score: 0.0,
            has_strong_lexical: false,
            has_strong_vector: false,
            missing_entities: Vec::new(),
            missing_slots: Vec::new(),
            compare_side_missing: None,
            coverage: None,
            citation_unsupported: false,
        }
    }
}

/// Result of one sufficiency check.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SufficiencyReport {
    pub sufficient: bool,
    pub reasons: Vec<GapReason>,
    pub suggested: Vec<Expansion>,
}

/// Evaluate the signals and suggest at most one expansion per gap class.
///
/// The caller owns budget/scope policy: this function never decides whether a
/// global fallback is allowed, it only reports whether it would help.
pub(crate) fn check_sufficiency(signals: &SufficiencySignals) -> SufficiencyReport {
    let mut reasons = Vec::new();
    let mut suggested = Vec::new();

    if signals.hit_count == 0 {
        reasons.push(GapReason::NoHits);
        suggested.push(Expansion::QueryVariant);
        suggested.push(Expansion::IncreaseTopK);
    } else if signals.min_rerank_score > 0.0
        && signals
            .top_rerank_score
            .is_some_and(|s| s < signals.min_rerank_score)
        && !signals.has_strong_lexical
        && !signals.has_strong_vector
    {
        reasons.push(GapReason::LowScore);
        suggested.push(Expansion::QueryVariant);
        suggested.push(Expansion::IncreaseTopK);
    }

    for entity in &signals.missing_entities {
        let entity = entity.trim();
        if entity.is_empty() {
            continue;
        }
        reasons.push(GapReason::EntityMissing(entity.to_string()));
        suggested.push(Expansion::QueryVariant);
        suggested.push(Expansion::ExactMatch);
    }

    for slot in &signals.missing_slots {
        let slot = slot.trim();
        if slot.is_empty() {
            continue;
        }
        reasons.push(GapReason::SlotMissing(slot.to_string()));
        suggested.push(Expansion::QueryVariant);
        suggested.push(Expansion::NeighborChunks);
    }

    if let Some(side) = signals.compare_side_missing.as_deref() {
        let side = side.trim();
        if !side.is_empty() {
            reasons.push(GapReason::CompareSideMissing(side.to_string()));
            suggested.push(Expansion::QueryVariant);
            suggested.push(Expansion::IncreaseTopK);
        }
    }

    if let Some((processed, total)) = signals.coverage
        && total > 0
        && processed < total
    {
        reasons.push(GapReason::CoverageGap { processed, total });
        suggested.push(Expansion::ExactMatch);
        suggested.push(Expansion::NeighborChunks);
    }

    if signals.citation_unsupported {
        reasons.push(GapReason::CitationUnsupported);
        suggested.push(Expansion::QueryVariant);
        suggested.push(Expansion::NeighborChunks);
    }

    // Preserve first-seen order while removing duplicates that are not
    // adjacent (e.g. QueryVariant, ExactMatch, QueryVariant, ExactMatch).
    let mut seen = Vec::new();
    suggested.retain(|e| {
        if seen.contains(e) {
            false
        } else {
            seen.push(*e);
            true
        }
    });
    SufficiencyReport {
        sufficient: reasons.is_empty(),
        reasons,
        suggested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hits_is_insufficient() {
        let report = check_sufficiency(&SufficiencySignals::default());
        assert!(!report.sufficient);
        assert_eq!(report.reasons, vec![GapReason::NoHits]);
        assert!(report.suggested.contains(&Expansion::QueryVariant));
        assert!(report.suggested.contains(&Expansion::IncreaseTopK));
    }

    #[test]
    fn low_score_without_strong_channel_is_insufficient() {
        let signals = SufficiencySignals {
            hit_count: 3,
            top_rerank_score: Some(0.1),
            min_rerank_score: 0.2,
            ..Default::default()
        };
        let report = check_sufficiency(&signals);
        assert!(!report.sufficient);
        assert_eq!(report.reasons, vec![GapReason::LowScore]);
    }

    #[test]
    fn low_score_with_strong_lexical_is_sufficient() {
        let signals = SufficiencySignals {
            hit_count: 3,
            top_rerank_score: Some(0.1),
            min_rerank_score: 0.2,
            has_strong_lexical: true,
            ..Default::default()
        };
        let report = check_sufficiency(&signals);
        assert!(report.sufficient);
        assert!(report.reasons.is_empty());
    }

    #[test]
    fn missing_entity_suggests_query_variant_and_exact_match() {
        let signals = SufficiencySignals {
            hit_count: 5,
            top_rerank_score: Some(0.9),
            missing_entities: vec!["深蓝公司".into()],
            ..Default::default()
        };
        let report = check_sufficiency(&signals);
        assert!(!report.sufficient);
        assert_eq!(
            report.reasons,
            vec![GapReason::EntityMissing("深蓝公司".into())]
        );
        assert!(report.suggested.contains(&Expansion::QueryVariant));
        assert!(report.suggested.contains(&Expansion::ExactMatch));
    }

    #[test]
    fn missing_slot_suggests_neighbor_chunks() {
        let signals = SufficiencySignals {
            hit_count: 2,
            top_rerank_score: Some(0.8),
            missing_slots: vec!["律师费金额".into()],
            ..Default::default()
        };
        let report = check_sufficiency(&signals);
        assert!(!report.sufficient);
        assert!(report.suggested.contains(&Expansion::NeighborChunks));
    }

    #[test]
    fn coverage_gap_is_reported() {
        let signals = SufficiencySignals {
            hit_count: 10,
            top_rerank_score: Some(0.8),
            coverage: Some((7, 23)),
            ..Default::default()
        };
        let report = check_sufficiency(&signals);
        assert!(!report.sufficient);
        assert_eq!(
            report.reasons,
            vec![GapReason::CoverageGap {
                processed: 7,
                total: 23
            }]
        );
    }

    #[test]
    fn full_coverage_is_sufficient() {
        let signals = SufficiencySignals {
            hit_count: 10,
            top_rerank_score: Some(0.8),
            coverage: Some((23, 23)),
            ..Default::default()
        };
        let report = check_sufficiency(&signals);
        assert!(report.sufficient);
    }

    #[test]
    fn suggestions_are_deduplicated() {
        let signals = SufficiencySignals {
            hit_count: 1,
            top_rerank_score: Some(0.8),
            missing_entities: vec!["A".into(), "B".into()],
            ..Default::default()
        };
        let report = check_sufficiency(&signals);
        let mut sorted = report.suggested.clone();
        sorted.sort_by_key(|e| format!("{e:?}"));
        sorted.dedup();
        assert_eq!(sorted.len(), report.suggested.len());
    }
}
