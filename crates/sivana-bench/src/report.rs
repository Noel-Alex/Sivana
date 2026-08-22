//! Report emission: JSON (machine) + Markdown (human).

use crate::runner::RunSummary;
use std::collections::BTreeMap;
use std::path::Path;

pub fn write_json(summary: &RunSummary, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(summary).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Per-degradation-cell breakdown for the markdown table.
#[derive(Default)]
struct Cell {
    n: usize,
    track_hits: usize,
    offset_hits: usize,
    gated_hits: usize,
    fingerprint_ms_sum: f64,
    match_ms_sum: f64,
}

fn render_markdown(summary: &RunSummary) -> String {
    let agg = summary.aggregate();
    let mut cells: BTreeMap<String, Cell> = BTreeMap::new();
    for c in &summary.cases {
        let cell = cells.entry(c.degradation.clone()).or_default();
        cell.n += 1;
        cell.track_hits += c.track_hit as usize;
        cell.offset_hits += c.offset_hit as usize;
        cell.gated_hits += c.gated_hit as usize;
        cell.fingerprint_ms_sum += c.fingerprint_us as f64 / 1000.0;
        cell.match_ms_sum += c.match_us as f64 / 1000.0;
    }

    let mut out = String::new();
    out.push_str("# Sivana baseline benchmark\n\n");
    out.push_str(&format!(
        "- engine: `{}` / fingerprint version {}\n- corpus: {} tracks x {:.0}s @ {} Hz (seed {})\n- query excerpt: {:.1}s\n\n",
        summary.engine,
        summary.fingerprint_version,
        summary.n_tracks,
        summary.track_seconds,
        summary.sample_rate_hz,
        summary.seed,
        summary.excerpt_seconds,
    ));

    out.push_str("## Overall\n\n| metric | value |\n|---|---|\n");
    out.push_str(&format!("| cases | {} |\n", agg.total_cases));
    out.push_str(&format!(
        "| recall@1 (track identity) | {:.1}% |\n",
        agg.recall_track * 100.0
    ));
    out.push_str(&format!(
        "| recall@1 (+offset within 2 frames) | {:.1}% |\n",
        agg.recall_offset * 100.0
    ));
    out.push_str(&format!(
        "| recall via legacy score gate | {:.1}% |\n",
        agg.recall_gated * 100.0
    ));
    out.push_str(&format!(
        "| mean fingerprint time | {:.2} ms |\n",
        agg.mean_fingerprint_ms
    ));
    out.push_str(&format!(
        "| mean match time | {:.2} ms |\n",
        agg.mean_match_ms
    ));
    out.push_str(&format!(
        "| p95 total latency | {:.2} ms |\n",
        agg.p95_total_ms
    ));
    out.push_str(&format!(
        "| out-of-catalog false accepts | {}/{} |\n\n",
        agg.false_accepts, agg.rejection_cases
    ));

    out.push_str("## By degradation\n\n");
    out.push_str("| degradation | n | recall(track) | recall(offset) | gated | mean fp ms | mean match ms |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|---:|\n");
    for (id, cell) in &cells {
        let pct = |x: usize, n: usize| {
            if n == 0 {
                "-".into()
            } else {
                format!("{:.0}%", x as f64 * 100.0 / n as f64)
            }
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.2} | {:.2} |\n",
            id,
            cell.n,
            pct(cell.track_hits, cell.n),
            pct(cell.offset_hits, cell.n),
            pct(cell.gated_hits, cell.n),
            cell.fingerprint_ms_sum / cell.n.max(1) as f64,
            cell.match_ms_sum / cell.n.max(1) as f64,
        ));
    }

    out.push_str("\n## Out-of-catalog rejection\n\n| degradation | accepted (should be no) | best score |\n|---|---|---:|\n");
    for r in &summary.rejection_cases {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            r.degradation,
            if r.accepted_by_gate { "**YES**" } else { "no" },
            r.best_score
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".into())
        ));
    }

    out
}

pub fn write_markdown(summary: &RunSummary, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, render_markdown(summary)).map_err(|e| e.to_string())
}
