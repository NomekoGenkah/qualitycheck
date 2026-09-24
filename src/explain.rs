//! `--explain`: locating where in a file a failing metric's finding comes from.
//!
//! Jev answers with typed judgments, not text, so it can't cite lines itself. Instead the file is
//! split into regions here, deterministically, and Jev is asked of each region whether it is one
//! of the places responsible for the finding. Regions that stand out from the rest of the file
//! are reported as line ranges.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::profile::Metric;
use crate::scorer::MetricEvaluation;

/// Bumped whenever the regions or questions change, so cached explanations are asked again.
pub const EXPLAIN_VERSION: u32 = 1;

/// Regions aim for this many lines, cutting at the shallowest blank-line boundary nearby.
const TARGET_REGION_LINES: usize = 40;
const MIN_REGION_LINES: usize = 10;
/// Past this, a region is cut even without a blank-line boundary.
const MAX_REGION_LINES: usize = 80;
/// Longer files get proportionally longer regions, so a request stays within this many regions.
const MAX_REGIONS: usize = 40;

/// A region stands out when Jev judges it responsible with at least this probability...
const HOTSPOT_MIN_PROBABILITY: f64 = 0.5;
/// ...and this much above the file's median region, so a problem present everywhere isn't
/// pinned on whichever region scored highest.
const HOTSPOT_MARGIN: f64 = 0.25;
const MAX_HOTSPOTS: usize = 3;

/// Consecutive 1-based, inclusive line ranges covering the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start_line: usize,
    pub end_line: usize,
}

/// Splits `text` into consecutive regions of roughly `TARGET_REGION_LINES` lines. A region ends
/// before a non-blank line that follows a blank line, preferring the least indented such line (a
/// top-level item or a member of one) and then the one nearest the target length, so regions
/// follow functions and methods in most languages without parsing them.
pub fn split_regions(text: &str) -> Vec<Region> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let scale = lines.len().div_ceil(MAX_REGIONS * TARGET_REGION_LINES).max(1);
    let (target, min, max) = (TARGET_REGION_LINES * scale, MIN_REGION_LINES * scale, MAX_REGION_LINES * scale);

    let indent = |line: &str| line.len() - line.trim_start().len();
    let cuts: Vec<usize> = (1..lines.len())
        .filter(|&i| !lines[i].trim().is_empty() && lines[i - 1].trim().is_empty())
        .collect();

    let mut regions = Vec::new();
    let mut start = 0;
    while lines.len() - start > max {
        let end = cuts
            .iter()
            .copied()
            .filter(|&cut| cut >= start + min && cut <= start + max)
            .min_by_key(|&cut| (indent(lines[cut]), cut.abs_diff(start + target)))
            .unwrap_or(start + max);
        regions.push(Region { start_line: start + 1, end_line: end });
        start = end;
    }
    regions.push(Region { start_line: start + 1, end_line: lines.len() });
    regions
}

/// A metric to explain, with the finding to locate.
#[derive(Debug, Clone)]
pub struct ExplainTarget {
    pub metric_id: String,
    pub question: String,
    pub finding: String,
}

impl ExplainTarget {
    /// The finding is the rubric situation of Jev's answer, or for metrics without a rubric,
    /// the answer itself.
    pub fn new(metric: &Metric, evaluation: &MetricEvaluation) -> Self {
        let finding = evaluation.matched_rubric.clone().unwrap_or_else(|| {
            format!("The answer for this file was \"{}\".", evaluation.raw_value)
        });
        Self { metric_id: metric.id.clone(), question: metric.question.clone(), finding }
    }

    /// Identifies what is asked, for caching: the finding and its question, not the file.
    pub fn scope(&self) -> String {
        let hash = blake3::hash(format!("{}\n{}\n{}", self.metric_id, self.question, self.finding).as_bytes());
        format!("explain:v{EXPLAIN_VERSION}:{}", hash.to_hex())
    }
}

/// What Jev sees: the file as regions, and every finding to locate, stated once.
pub fn explain_state(path: &str, text: &str, regions: &[Region], targets: &[ExplainTarget]) -> Value {
    let lines: Vec<&str> = text.lines().collect();
    let regions: Vec<Value> = regions
        .iter()
        .map(|r| {
            serde_json::json!({
                "lines": format!("{}-{}", r.start_line, r.end_line),
                "code": lines[r.start_line - 1..r.end_line].join("\n"),
            })
        })
        .collect();
    let findings: serde_json::Map<String, Value> = targets
        .iter()
        .map(|t| (t.metric_id.clone(), serde_json::json!({ "question": t.question, "finding": t.finding })))
        .collect();
    serde_json::json!({ "path": path, "regions": regions, "findings": findings })
}

/// Id of the question asking whether region `index` is responsible for `metric_id`'s finding.
pub fn question_id(metric_id: &str, index: usize) -> String {
    format!("{metric_id}#{index}")
}

/// One yes/no question per target and region.
pub fn explain_questions(regions: &[Region], targets: &[ExplainTarget]) -> Vec<(String, String)> {
    let mut questions = Vec::with_capacity(regions.len() * targets.len());
    for target in targets {
        for (index, region) in regions.iter().enumerate() {
            questions.push((
                question_id(&target.metric_id, index),
                format!(
                    "The state has `path`, `regions` (the file split into consecutive regions of lines), and \
                     `findings`: conclusions reached by evaluating the whole file, each with the question it \
                     answers. Is the code in `regions[{index}]` (lines {}-{}) one of the places responsible for \
                     `findings.{}`? Answer yes only if that region's own code shows the problem; the other \
                     regions are shown for context.",
                    region.start_line, region.end_line, target.metric_id
                ),
            ));
        }
    }
    questions
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegionAnswer {
    pub start_line: usize,
    pub end_line: usize,
    /// Jev's probability that this region is one of the places responsible for the finding.
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    /// Regions that stand out as responsible, most likely first. Empty when the finding is
    /// spread across the file or no region shows it.
    pub hotspots: Vec<RegionAnswer>,
    /// Every region asked about, in file order.
    pub regions: Vec<RegionAnswer>,
}

impl Evidence {
    pub fn from_answers(regions: &[Region], probabilities: &[f64]) -> Self {
        let regions: Vec<RegionAnswer> = regions
            .iter()
            .zip(probabilities)
            .map(|(r, &probability)| RegionAnswer { start_line: r.start_line, end_line: r.end_line, probability })
            .collect();

        let mut sorted: Vec<f64> = probabilities.to_vec();
        sorted.sort_by(f64::total_cmp);
        let median = match sorted.len() {
            0 => 0.0,
            n if n % 2 == 1 => sorted[n / 2],
            n => (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0,
        };

        let mut hotspots: Vec<RegionAnswer> = regions
            .iter()
            .filter(|r| r.probability >= HOTSPOT_MIN_PROBABILITY && r.probability - median >= HOTSPOT_MARGIN)
            .cloned()
            .collect();
        hotspots.sort_by(|a, b| b.probability.total_cmp(&a.probability));
        hotspots.truncate(MAX_HOTSPOTS);
        Self { hotspots, regions }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(count: usize, blank_every: usize) -> String {
        (1..=count)
            .map(|i| if i % blank_every == 0 { String::new() } else { format!("line {i}") })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn regions_cover_the_file_contiguously() {
        let text = numbered(500, 17);
        let regions = split_regions(&text);
        assert_eq!(regions[0].start_line, 1);
        assert_eq!(regions.last().unwrap().end_line, 500);
        for pair in regions.windows(2) {
            assert_eq!(pair[1].start_line, pair[0].end_line + 1);
        }
        assert!(regions.iter().all(|r| r.end_line - r.start_line < MAX_REGION_LINES));
    }

    #[test]
    fn regions_cut_before_the_least_indented_item() {
        // Two 30-line functions, each with an indented blank-line break inside.
        let function = |name: &str| {
            let mut lines = vec![format!("fn {name}() {{")];
            lines.extend((0..14).map(|i| format!("    a{i};")));
            lines.push(String::new());
            lines.extend((0..14).map(|i| format!("    b{i};")));
            lines.push("}".to_string());
            lines.join("\n")
        };
        let text = [function("one"), function("two"), function("three")].join("\n\n");
        let regions = split_regions(&text);
        // Cut before `fn two` (line 33), not at the indented break inside `fn one`.
        assert_eq!(regions[0], Region { start_line: 1, end_line: 32 });
        assert_eq!(text.lines().nth(regions[1].start_line - 1), Some("fn two() {"));
    }

    #[test]
    fn short_files_are_one_region_and_long_ones_stay_under_the_cap() {
        assert_eq!(split_regions(&numbered(60, 7)), vec![Region { start_line: 1, end_line: 60 }]);
        assert!(split_regions(&numbered(5000, 9)).len() <= MAX_REGIONS + 1);
        assert!(split_regions("").is_empty());
    }

    #[test]
    fn hotspots_stand_out_from_the_rest_of_the_file() {
        let regions: Vec<Region> = (0..8).map(|i| Region { start_line: i * 10 + 1, end_line: i * 10 + 10 }).collect();
        // Measured on a TS service: the filter-string building (region 3) for injection_risk...
        let located = Evidence::from_answers(&regions, &[0.03, 0.07, 0.09, 0.89, 0.34, 0.11, 0.07, 0.06]);
        assert_eq!(located.hotspots.len(), 1);
        assert_eq!(located.hotspots[0].start_line, 31);
        // ...and testability, judged moderately everywhere: nothing stands out.
        let spread = Evidence::from_answers(&regions, &[0.09, 0.39, 0.52, 0.36, 0.54, 0.28, 0.29, 0.53]);
        assert!(spread.hotspots.is_empty());
        assert_eq!(spread.regions.len(), 8);
    }
}
