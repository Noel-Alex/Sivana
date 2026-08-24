//! Gate calibration (PLAN §26, experiment E4).
//!
//! The runner records raw matcher evidence (inlier count, offset
//! concentration) for every case and every out-of-catalog probe. This
//! module sweeps acceptance thresholds *offline* over that recorded
//! evidence and finds the operating points that maximize gated recall at
//! zero false accepts — replacing hand-picked gates with measured ones.

use crate::runner::RunSummary;
use serde::Serialize;

/// One evaluated acceptance gate.
#[derive(Debug, Clone, Serialize)]
pub struct CalibPoint {
    pub min_inliers: usize,
    pub min_concentration: f32,
    /// Gated recall over in-catalog cases (correct + accepted).
    pub gated_recall: f64,
    /// Out-of-catalog probes accepted under this gate.
    pub false_accepts: usize,
    pub rejection_cases: usize,
}

/// Sweep `(min_inliers, min_concentration)` gates against one run's
/// recorded evidence.
///
/// A case is accepted when its rank-1 outcome exists and passes both
/// thresholds; out-of-catalog probes use their best-outcome features the
/// same way, so recall and rejection are scored on identical semantics.
pub fn sweep(
    summary: &RunSummary,
    inliers_range: std::ops::RangeInclusive<usize>,
    conc_steps: &[f32],
) -> Vec<CalibPoint> {
    let mut points = Vec::new();
    for a in inliers_range {
        for &b in conc_steps {
            let mut hits = 0usize;
            let mut fa = 0usize;
            for c in &summary.cases {
                let accepted = c.score.is_some_and(|inl| {
                    inl >= a && c.offset_concentration.is_some_and(|conc| conc >= b)
                });
                if accepted && c.track_hit {
                    hits += 1;
                }
            }
            for r in &summary.rejection_cases {
                let accepted = r.best_inliers.is_some_and(|inl| {
                    inl >= a && r.best_concentration.is_some_and(|conc| conc >= b)
                });
                if accepted {
                    fa += 1;
                }
            }
            let total = summary.cases.len();
            points.push(CalibPoint {
                min_inliers: a,
                min_concentration: b,
                gated_recall: if total == 0 {
                    0.0
                } else {
                    hits as f64 / total as f64
                },
                false_accepts: fa,
                rejection_cases: summary.rejection_cases.len(),
            });
        }
    }
    points
}

/// Operating-point selection rule: among zero-false-accept points return
/// the one with maximum gated recall (ties -> fewest required inliers,
/// then loosest concentration).
pub fn recommend(points: &[CalibPoint]) -> Option<&CalibPoint> {
    points
        .iter()
        .filter(|p| p.false_accepts == 0)
        .max_by(|a, b| {
            a.gated_recall
                .partial_cmp(&b.gated_recall)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.min_inliers.cmp(&a.min_inliers))
        })
}

/// Render the sweep as markdown: full grid plus the recommended row.
pub fn to_markdown(engine: &str, points: &[CalibPoint]) -> String {
    let mut out = String::new();
    out.push_str(&format!("## Calibration — {engine}\n\n"));
    out.push_str("Gate: accept iff `inliers >= a AND concentration >= b`.\n\n");
    out.push_str("| a (min inliers) | b (min conc) | gated recall | false accepts |\n|---:|---:|---:|---:|\n");
    for p in points {
        out.push_str(&format!(
            "| {} | {:.2} | {:.1}% | {}/{} |\n",
            p.min_inliers,
            p.min_concentration,
            p.gated_recall * 100.0,
            p.false_accepts,
            p.rejection_cases
        ));
    }
    match recommend(points) {
        Some(p) => out.push_str(&format!(
            "\n**Recommended (FA = 0, max recall):** a = {}, b = {:.2}, \
             gated recall {:.1}%.\n",
            p.min_inliers,
            p.min_concentration,
            p.gated_recall * 100.0
        )),
        None => out.push_str("\nNo zero-false-accept operating point exists on this grid — the feature space does not separate correct matches from out-of-catalog audio yet.\n"),
    }
    out.push('\n');
    out
}
