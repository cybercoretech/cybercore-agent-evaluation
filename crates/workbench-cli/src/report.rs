//! `workbench report` (ADR-0026): one shareable page for an evaluation or a
//! comparison, in Markdown or self-contained HTML.
//!
//! The numbers are exactly what `workbench summary` and `workbench compare`
//! compute (and record). The page is built as a small document model and
//! rendered twice, so both formats always say the same thing.

use crate::summary::{amount, open, summarise};
use cybercore_evaluation_store::analysis::{
    compare, compare_resources, record_comparison, record_resource_comparisons, resource_summaries,
    Comparison, EvaluationOutcomes, PassRate, ResourceComparison, ResourceMetric, ResourceSummary,
    SummaryRecords, TrialOutcome,
};
use cybercore_evaluation_store::LocalStore;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One block of the page.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading(u8, String),
    Paragraph(String),
    List(Vec<String>),
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

struct Evaluated {
    id: String,
    outcomes: EvaluationOutcomes,
    rate: PassRate,
    records: SummaryRecords,
    resources: Vec<ResourceSummary>,
}

struct Compared {
    baseline: usize,
    treatment: usize,
    comparison: Comparison,
    comparison_id: String,
    resources: Vec<ResourceComparison>,
    resource_ids: Vec<String>,
}

pub fn report(store_dir: &Path, evaluation_ids: &[String], format: &str) -> Result<(), String> {
    let store = open(store_dir)?;
    let blocks = build(&store, evaluation_ids, &now())?;
    let page = match format {
        "html" => html(&blocks),
        _ => markdown(&blocks),
    };
    say!("{}", page.trim_end());
    Ok(())
}

/// The current UTC time as `YYYY-MM-DD HH:MM UTC`, from the standard library.
fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc(secs)
}

/// Seconds since the Unix epoch → `YYYY-MM-DD HH:MM UTC` (civil-from-days).
fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let (hour, minute) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

fn build(store: &LocalStore, ids: &[String], generated: &str) -> Result<Vec<Block>, String> {
    let mut evaluated = Vec::new();
    for id in ids {
        let (outcomes, rate, records) = summarise(store, id)?;
        let resources = resource_summaries(&outcomes);
        evaluated.push(Evaluated {
            id: id.clone(),
            outcomes,
            rate,
            records,
            resources,
        });
    }
    let mut compared = Vec::new();
    for treatment in 1..evaluated.len() {
        let (b, t) = (&evaluated[0], &evaluated[treatment]);
        let comparison = compare(&b.outcomes, &b.rate, &t.outcomes, &t.rate);
        let comparison_id = record_comparison(
            store,
            (&b.outcomes, &b.records),
            (&t.outcomes, &t.records),
            &comparison,
        )
        .map_err(|e| e.to_string())?;
        let resources = compare_resources(&b.outcomes, &t.outcomes);
        let resource_ids = record_resource_comparisons(
            store,
            (&b.outcomes, &b.records),
            (&t.outcomes, &t.records),
            &resources,
        )
        .map_err(|e| e.to_string())?;
        compared.push(Compared {
            baseline: 0,
            treatment,
            comparison,
            comparison_id,
            resources,
            resource_ids,
        });
    }
    let families = task_labels(store, &evaluated);
    Ok(document(&evaluated, &compared, &families, generated))
}

/// Task revision ID → (task key, family), from the stored task revisions.
fn task_labels(store: &LocalStore, evaluated: &[Evaluated]) -> BTreeMap<String, (String, String)> {
    let mut labels = BTreeMap::new();
    for trial in evaluated.iter().flat_map(|e| &e.outcomes.trials) {
        if labels.contains_key(&trial.task_id) {
            continue;
        }
        let record = store.metadata().get_record(&trial.task_id).ok().flatten();
        let key = record
            .as_ref()
            .and_then(|r| r["task_key"].as_str())
            .unwrap_or(&trial.task_id)
            .to_string();
        let family = record
            .as_ref()
            .and_then(|r| r["task_family"].as_str())
            .unwrap_or("—")
            .to_string();
        labels.insert(trial.task_id.clone(), (key, family));
    }
    labels
}

fn metric(resources: &[ResourceSummary], which: ResourceMetric) -> Option<&ResourceSummary> {
    resources.iter().find(|s| s.metric == which)
}

fn resource_cell(resources: &[ResourceSummary], which: ResourceMetric) -> String {
    match metric(resources, which) {
        Some(s) => match s.estimate {
            Some(v) => amount(which, v, s.currency.as_deref()),
            None => "—".into(),
        },
        None => "—".into(),
    }
}

fn rate_cell(rate: &PassRate) -> String {
    match rate.estimate {
        None => "no data".into(),
        Some(estimate) => {
            let interval = rate
                .interval
                .map(|(lower, upper, _)| format!("; 95% {lower:.3}–{upper:.3}"))
                .unwrap_or_default();
            format!(
                "{estimate:.3} ({} of {}{interval})",
                rate.passes, rate.denominator
            )
        }
    }
}

fn words(text: &str) -> String {
    text.replace('_', " ")
}

fn comparison_items(c: &Compared, evaluated: &[Evaluated]) -> Vec<String> {
    let (base, treat) = (&evaluated[c.baseline].id, &evaluated[c.treatment].id);
    let comparison = &c.comparison;
    let mut items = Vec::new();
    match comparison
        .difference
        .filter(|_| comparison.status != "not_comparable")
    {
        None => items.push(format!(
            "Pass rate: {}{}.",
            words(comparison.status),
            if comparison.mismatches.is_empty() {
                String::new()
            } else {
                format!(" ({})", words(&comparison.mismatches.join(", ")))
            }
        )),
        Some(difference) => {
            let interval = comparison
                .interval
                .map(|(lower, upper)| {
                    format!(
                        " ({:.1}% interval {lower:+.3} to {upper:+.3})",
                        comparison.confidence_level * 100.0
                    )
                })
                .unwrap_or_default();
            let direction = match comparison.direction {
                "favors_treatment" => format!("favors {treat}"),
                "favors_baseline" => format!("favors {base}"),
                other => words(other),
            };
            let p = comparison
                .p_value
                .map(|p| format!("; McNemar p = {p:.4}"))
                .unwrap_or_default();
            items.push(format!(
                "Pass rate: {difference:+.3}{interval} over {} paired task(s); {direction}{p}.",
                comparison.matched_tasks
            ));
        }
    }
    let inference = match &comparison.declared_comparison_id {
        Some(id) if comparison.inferential_status == "confirmatory" => {
            format!("confirmatory (predeclared as {id})")
        }
        _ => words(comparison.inferential_status),
    };
    items.push(format!(
        "Inference: {inference}; status {}.",
        words(comparison.status)
    ));
    if comparison.practical_significance != "not_assessed" {
        items.push(format!(
            "Practical significance: {}{}.",
            words(comparison.practical_significance),
            comparison
                .practical_significance_threshold
                .map(|t| format!(" (threshold ±{t})"))
                .unwrap_or_default()
        ));
    }
    let currency = metric(&evaluated[c.baseline].resources, ResourceMetric::Cost)
        .and_then(|s| s.currency.clone());
    for r in &c.resources {
        let name = match r.metric {
            ResourceMetric::WallTime => "Wall time",
            ResourceMetric::Tokens => "Tokens",
            ResourceMetric::Cost => "Cost",
        };
        let line = match r.difference.filter(|_| r.status != "not_comparable") {
            None => format!(
                "{name}: {}{}.",
                words(r.status),
                if r.mismatches.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", words(&r.mismatches.join(", ")))
                }
            ),
            Some(difference) => {
                let money = currency.as_deref();
                let relative = r
                    .relative
                    .map(|x| format!(" ({:+.1}%)", x * 100.0))
                    .unwrap_or_default();
                let interval = r
                    .interval
                    .map(|(lower, upper)| {
                        format!(
                            ", 95% interval {} to {}",
                            amount(r.metric, lower, money),
                            amount(r.metric, upper, money)
                        )
                    })
                    .unwrap_or_default();
                let direction = match r.direction {
                    "favors_treatment" => format!("favors {treat}"),
                    "favors_baseline" => format!("favors {base}"),
                    other => words(other),
                };
                format!(
                    "{name}: {}{} per trial{relative}{interval}; {direction} ({}).",
                    if difference > 0.0 { "+" } else { "" },
                    amount(r.metric, difference, money),
                    words(r.inferential_status)
                )
            }
        };
        items.push(line);
    }
    items
}

fn document(
    evaluated: &[Evaluated],
    compared: &[Compared],
    labels: &BTreeMap<String, (String, String)>,
    generated: &str,
) -> Vec<Block> {
    let ids: Vec<&str> = evaluated.iter().map(|e| e.id.as_str()).collect();
    let title = match ids.as_slice() {
        [only] => format!("Evaluation report: {only}"),
        [base, rest @ ..] => format!("Evaluation report: {} vs {base}", rest.join(", ")),
        [] => "Evaluation report".into(),
    };
    let mut tasks: Vec<&str> = Vec::new();
    for trial in evaluated.iter().flat_map(|e| &e.outcomes.trials) {
        if !tasks.contains(&trial.task_id.as_str()) {
            tasks.push(&trial.task_id);
        }
    }
    let planned: usize = evaluated.iter().map(|e| e.outcomes.trials.len()).sum();
    let mut blocks = vec![
        Block::Heading(1, title),
        Block::Paragraph(format!(
            "Generated {generated} by workbench {}. {} evaluation(s), {} task(s), {planned} planned trial(s).{}",
            env!("CARGO_PKG_VERSION"),
            evaluated.len(),
            tasks.len(),
            if evaluated.len() > 1 {
                format!(" Baseline: {}.", evaluated[0].id)
            } else {
                String::new()
            }
        )),
        Block::Heading(2, "Results".into()),
        Block::Table {
            header: [
                "Evaluation",
                "Pass rate",
                "Status",
                "Median time",
                "Tokens per trial",
                "Cost per trial",
            ]
            .map(String::from)
            .to_vec(),
            rows: evaluated
                .iter()
                .map(|e| {
                    vec![
                        e.id.clone(),
                        rate_cell(&e.rate),
                        words(e.rate.status),
                        resource_cell(&e.resources, ResourceMetric::WallTime),
                        resource_cell(&e.resources, ResourceMetric::Tokens),
                        resource_cell(&e.resources, ResourceMetric::Cost),
                    ]
                })
                .collect(),
        },
    ];
    for c in compared {
        blocks.push(Block::Heading(
            2,
            format!(
                "{} vs {} (baseline)",
                evaluated[c.treatment].id, evaluated[c.baseline].id
            ),
        ));
        blocks.push(Block::List(comparison_items(c, evaluated)));
    }

    blocks.push(Block::Heading(2, "By task".into()));
    let mut header = vec!["Task".to_string(), "Family".to_string()];
    header.extend(evaluated.iter().map(|e| e.id.clone()));
    let rows = tasks
        .iter()
        .map(|task| {
            let (key, family) = labels
                .get(*task)
                .cloned()
                .unwrap_or_else(|| (task.to_string(), "—".into()));
            let mut row = vec![key, family];
            for e in evaluated {
                row.push(task_cell(&e.outcomes, task));
            }
            row
        })
        .collect();
    blocks.push(Block::Table { header, rows });
    blocks.push(Block::Paragraph(
        "Each cell is passes out of trials with an outcome; ✗ marks a task with a failure.".into(),
    ));

    let mut seen = BTreeSet::new();
    let caveats: Vec<String> = evaluated
        .iter()
        .flat_map(|e| {
            e.rate
                .limitations
                .iter()
                .chain(e.resources.iter().flat_map(|s| &s.limitations))
        })
        .chain(compared.iter().flat_map(|c| {
            c.comparison
                .limitations
                .iter()
                .chain(c.resources.iter().flat_map(|r| &r.limitations))
        }))
        .filter(|note| seen.insert(note.as_str()))
        .cloned()
        .collect();
    if !caveats.is_empty() {
        blocks.push(Block::Heading(2, "Caveats".into()));
        blocks.push(Block::List(caveats));
    }

    blocks.push(Block::Heading(2, "Provenance".into()));
    blocks.push(Block::Table {
        header: [
            "Evaluation",
            "Metric result",
            "Analysis result",
            "Frozen plan",
        ]
        .map(String::from)
        .to_vec(),
        rows: evaluated
            .iter()
            .map(|e| {
                vec![
                    e.id.clone(),
                    e.records.metric_result_id.clone(),
                    e.records.analysis_result_id.clone(),
                    e.outcomes
                        .frozen_plan
                        .as_ref()
                        .and_then(|p| p["content_digest"].as_str())
                        .map(|d| {
                            format!("{} (sha256 {}…)", e.outcomes.plan_id, &d[..12.min(d.len())])
                        })
                        .unwrap_or_else(|| "none".into()),
                ]
            })
            .collect(),
    });
    if !compared.is_empty() {
        blocks.push(Block::List(
            compared
                .iter()
                .map(|c| {
                    let labelled: Vec<String> = c
                        .resource_ids
                        .iter()
                        .zip(ResourceMetric::ALL)
                        .map(|(id, m)| {
                            format!("{id} ({})", m.id().trim_start_matches("workbench-"))
                        })
                        .collect();
                    format!(
                        "{} vs {}: {} (pass rate), {}",
                        evaluated[c.treatment].id,
                        evaluated[c.baseline].id,
                        c.comparison_id,
                        labelled.join(", ")
                    )
                })
                .collect(),
        ));
    }
    blocks
}

fn task_cell(outcomes: &EvaluationOutcomes, task: &str) -> String {
    let (mut passes, mut counted) = (0, 0);
    for trial in outcomes.trials.iter().filter(|t| t.task_id == task) {
        match trial.outcome {
            TrialOutcome::Pass => {
                passes += 1;
                counted += 1;
            }
            TrialOutcome::Fail | TrialOutcome::NoVerdict => counted += 1,
            TrialOutcome::Invalid | TrialOutcome::NotRun => {}
        }
    }
    match counted {
        0 => "—".into(),
        n if passes < n => format!("{passes}/{n} ✗"),
        n => format!("{passes}/{n}"),
    }
}

fn md_cell(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

pub fn markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            Block::Heading(level, text) => {
                out.push_str(&format!("{} {}\n\n", "#".repeat(usize::from(*level)), text))
            }
            Block::Paragraph(text) => out.push_str(&format!("{text}\n\n")),
            Block::List(items) => {
                for item in items {
                    out.push_str(&format!("- {}\n", item.replace('\n', " ")));
                }
                out.push('\n');
            }
            Block::Table { header, rows } => {
                let line = |cells: &[String]| {
                    format!(
                        "| {} |\n",
                        cells
                            .iter()
                            .map(|c| md_cell(c))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    )
                };
                out.push_str(&line(header));
                out.push_str(&format!("|{}\n", "---|".repeat(header.len())));
                for row in rows {
                    out.push_str(&line(row));
                }
                out.push('\n');
            }
        }
    }
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const STYLE: &str = r#"
:root { --bg: #f7f7f9; --fg: #1d1d24; --muted: #5d5d6e; --line: #d9d9e3; --card: #ffffff; --accent: #6d4aff; --bad: #b42318; }
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) { --bg: #121218; --fg: #e8e8f0; --muted: #a0a0b4; --line: #2c2c3a; --card: #1a1a24; --accent: #a68bff; --bad: #ff8a80; }
}
:root[data-theme="dark"] { --bg: #121218; --fg: #e8e8f0; --muted: #a0a0b4; --line: #2c2c3a; --card: #1a1a24; --accent: #a68bff; --bad: #ff8a80; }
* { box-sizing: border-box; scrollbar-color: var(--accent) var(--bg); }
::-webkit-scrollbar { width: 10px; height: 10px; }
::-webkit-scrollbar-track { background: var(--bg); }
::-webkit-scrollbar-thumb { background: var(--accent); border-radius: 6px; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 15px/1.55 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 1040px; margin: 0 auto; padding: 32px 16px 64px; }
h1 { font-size: 1.6rem; margin: 0 0 8px; }
h2 { font-size: 1.15rem; margin: 32px 0 10px; color: var(--accent); }
p, li { color: var(--fg); }
p.meta { color: var(--muted); }
ul { padding-left: 20px; }
.scroll { overflow-x: auto; border: 1px solid var(--line); border-radius: 8px; background: var(--card); }
table { border-collapse: collapse; width: 100%; font-variant-numeric: tabular-nums; }
th, td { text-align: left; padding: 8px 12px; border-bottom: 1px solid var(--line); white-space: nowrap; }
th { color: var(--muted); font-weight: 600; }
tr:last-child td { border-bottom: none; }
td.bad { color: var(--bad); }
"#;

pub fn html(blocks: &[Block]) -> String {
    let title = blocks
        .iter()
        .find_map(|b| match b {
            Block::Heading(1, text) => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("Evaluation report");
    let mut body = String::new();
    let mut first_paragraph = true;
    for block in blocks {
        match block {
            Block::Heading(level, text) => {
                body.push_str(&format!("<h{level}>{}</h{level}>\n", escape(text)))
            }
            Block::Paragraph(text) => {
                let class = if first_paragraph {
                    " class=\"meta\""
                } else {
                    ""
                };
                first_paragraph = false;
                body.push_str(&format!("<p{class}>{}</p>\n", escape(text)));
            }
            Block::List(items) => {
                body.push_str("<ul>\n");
                for item in items {
                    body.push_str(&format!("<li>{}</li>\n", escape(item)));
                }
                body.push_str("</ul>\n");
            }
            Block::Table { header, rows } => {
                body.push_str("<div class=\"scroll\"><table>\n<thead><tr>");
                for cell in header {
                    body.push_str(&format!("<th>{}</th>", escape(cell)));
                }
                body.push_str("</tr></thead>\n<tbody>\n");
                for row in rows {
                    body.push_str("<tr>");
                    for cell in row {
                        let class = if cell.ends_with('✗') {
                            " class=\"bad\""
                        } else {
                            ""
                        };
                        body.push_str(&format!("<td{class}>{}</td>", escape(cell)));
                    }
                    body.push_str("</tr>\n");
                }
                body.push_str("</tbody></table></div>\n");
            }
        }
    }
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n",
        escape(title)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Block> {
        vec![
            Block::Heading(1, "Evaluation report: <B> vs A".into()),
            Block::Paragraph("Generated now.".into()),
            Block::Table {
                header: vec!["Task".into(), "A|B".into()],
                rows: vec![
                    vec!["T1".into(), "0/2 ✗".into()],
                    vec!["T2".into(), "2/2".into()],
                ],
            },
            Block::List(vec![
                "note with <script>alert(1)</script> & \"quotes\"".into()
            ]),
        ]
    }

    #[test]
    fn utc_formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(utc(1_795_128_000), "2026-11-19 22:40 UTC");
    }

    #[test]
    fn markdown_escapes_table_cells() {
        let md = markdown(&sample());
        assert!(md.starts_with("# Evaluation report: <B> vs A\n\nGenerated now.\n\n"));
        assert!(md.contains("| Task | A\\|B |\n|---|---|\n| T1 | 0/2 ✗ |\n"));
        assert!(md.contains("- note with <script>"));
    }

    #[test]
    fn html_is_escaped_and_self_contained() {
        let page = html(&sample());
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<title>Evaluation report: &lt;B&gt; vs A</title>"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;quotes&quot;"));
        assert!(!page.contains("<script"));
        assert!(!page.contains("http://") && !page.contains("https://") && !page.contains("src="));
        assert!(page.contains("<td class=\"bad\">0/2 ✗</td>"));
        assert!(page.contains("prefers-color-scheme: dark"));
        assert!(page.contains("scrollbar-color"));
    }
}
