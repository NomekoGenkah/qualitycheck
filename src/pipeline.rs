use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;
use tokio::sync::Semaphore;

use crate::cache::{
    compute_cache_key, get_cached_result, put_cached_result, upgrade_cached_result,
    CachedFileResult, CACHE_FORMAT_VERSION,
};
use crate::error::{QualityCheckError, ScanError};
use crate::context::RelatedFile;
use crate::jev_client::{JevClient, CONTEXT_PREAMBLE};
use crate::profile::{compute_active_metrics_hash, Metric, Profile, Rubric};
use crate::scorer::{evaluate_file_with_metrics, FileEvaluation, ScanRunResult, SCORING_VERSION};

/// A file to evaluate. `content`, when set, is evaluated instead of reading `path` — e.g. a
/// file as it was at a base revision, reported under its current path.
#[derive(Debug, Clone)]
pub struct ScanInput {
    pub path: PathBuf,
    pub content: Option<Vec<u8>>,
    /// Other files shown to Jev as context (`--context`).
    pub related: Vec<RelatedFile>,
}

impl ScanInput {
    pub fn from_disk(path: PathBuf) -> Self {
        Self { path, content: None, related: Vec::new() }
    }

    pub fn in_memory(path: PathBuf, content: Vec<u8>) -> Self {
        Self { path, content: Some(content), related: Vec::new() }
    }
}

fn read_input_bytes(path: &Path, content: Option<Vec<u8>>) -> Result<Vec<u8>, QualityCheckError> {
    match content {
        Some(bytes) => Ok(bytes),
        None => std::fs::read(path)
            .map_err(|e| QualityCheckError::Scan(ScanError::FileReadError(path.to_path_buf(), e))),
    }
}

/// What Jev evaluates: the file's text or, when it has related files, an object with the file
/// under evaluation (`file`) and its `related_files`. Also returns the key under which answers
/// for this exact state are cached: plain files keep the metrics hash alone, so their cache is
/// shared with runs without `--context`.
fn evaluation_state(
    file_bytes: &[u8],
    path: &Path,
    project_root: &Path,
    related: &[RelatedFile],
    metrics_hash: &str,
) -> (Value, String) {
    let content = String::from_utf8_lossy(file_bytes).into_owned();
    if related.is_empty() {
        return (Value::String(content), metrics_hash.to_string());
    }
    let state = serde_json::json!({
        "file": {
            "path": compute_relative_display_path(path, project_root),
            "content": content,
        },
        "related_files": related,
    });
    let state_hash = blake3::hash(state.to_string().as_bytes()).to_hex();
    (state, format!("{metrics_hash}:context:{state_hash}"))
}

/// Orquestación concurrente y cacheada del pipeline de evaluación de archivos.
pub async fn run_scan_pipeline(
    target_path: &Path,
    file_paths: &[PathBuf],
    profiles: &[Profile],
    client: Arc<JevClient>,
    concurrency: usize,
    project_root: &Path,
) -> Result<ScanRunResult, QualityCheckError> {
    let inputs = file_paths.iter().cloned().map(ScanInput::from_disk).collect();
    run_scan_pipeline_on_inputs(target_path, inputs, profiles, client, concurrency, project_root)
        .await
}

pub async fn run_scan_pipeline_on_inputs(
    target_path: &Path,
    inputs: Vec<ScanInput>,
    profiles: &[Profile],
    client: Arc<JevClient>,
    concurrency: usize,
    project_root: &Path,
) -> Result<ScanRunResult, QualityCheckError> {
    let run_id = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let timestamp = Utc::now();
    let metrics_hash = compute_active_metrics_hash(profiles);
    let all_metrics = collect_unique_metrics(profiles);

    let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut tasks = Vec::with_capacity(inputs.len());

    for input in inputs {
        let target_root = target_path.to_path_buf();
        let p_root = project_root.to_path_buf();
        let m_hash = metrics_hash.clone();
        let metrics = all_metrics.clone();
        let profs = profiles.to_vec();
        let client_clone = Arc::clone(&client);
        let sem = Arc::clone(&semaphore);

        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            evaluate_single_file(
                input,
                &target_root,
                &p_root,
                &m_hash,
                &metrics,
                &profs,
                &client_clone,
            )
            .await
        }));
    }

    let mut file_evaluations = Vec::with_capacity(tasks.len());
    let mut cached_count = 0;
    let mut run_input_tokens = 0u64;
    let mut run_output_tokens = 0u64;

    for task in tasks {
        match task.await {
            Ok(Ok(eval)) => {
                if eval.served_from_cache {
                    cached_count += 1;
                } else if let Some(u) = &eval.usage {
                    run_input_tokens += u.input_tokens;
                    run_output_tokens += u.output_tokens;
                }
                file_evaluations.push(eval);
            }
            Ok(Err(err)) => return Err(err),
            Err(join_err) => {
                return Err(QualityCheckError::Usage(format!("Task execution error: {join_err}")));
            }
        }
    }

    file_evaluations.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    let overall_passed = file_evaluations
        .iter()
        .all(|file| file.profiles.iter().all(|prof| prof.passed));

    let profiles_used: Vec<String> = profiles.iter().map(|p| p.name.clone()).collect();
    let exit_reason = if overall_passed {
        None
    } else {
        Some("One or more files failed profile threshold criteria".to_string())
    };

    let estimated_cost_usd = (run_input_tokens as f64 * 0.042) / 1_000_000.0;
    let usage = crate::scorer::RunUsage {
        input_tokens: run_input_tokens,
        output_tokens: run_output_tokens,
        total_tokens: run_input_tokens + run_output_tokens,
        estimated_cost_usd,
    };

    Ok(ScanRunResult {
        scoring_version: SCORING_VERSION,
        run_id,
        timestamp,
        target_path: target_path.to_string_lossy().to_string(),
        total_files: file_evaluations.len(),
        cached_files: cached_count,
        profiles_used,
        files: file_evaluations,
        usage,
        passed: overall_passed,
        exit_reason,
    })
}

fn collect_unique_metrics(profiles: &[Profile]) -> Vec<Metric> {
    let mut map: HashMap<String, Metric> = HashMap::new();
    for profile in profiles {
        for metric in &profile.metrics {
            map.insert(metric.id.clone(), metric.clone());
        }
    }
    map.into_values().collect()
}

async fn evaluate_single_file(
    input: ScanInput,
    target_root: &Path,
    project_root: &Path,
    metrics_hash: &str,
    metrics: &[Metric],
    profiles: &[Profile],
    client: &JevClient,
) -> Result<FileEvaluation, QualityCheckError> {
    let ScanInput { path, content, related } = input;
    let path = path.as_path();
    let file_bytes = read_input_bytes(path, content)?;
    let (state, cache_scope) = evaluation_state(&file_bytes, path, project_root, &related, metrics_hash);
    let (cache_key, file_hash) = compute_cache_key(&file_bytes, &cache_scope);

    let (metric_results, served_from_cache, usage) = if let Some(mut cached) = get_cached_result(project_root, &cache_key) {
        upgrade_cached_result(&mut cached, metrics);
        (cached.metrics, true, cached.usage)
    } else {
        let eval_res = client
            .evaluate_file(&state, metrics)
            .await
            .map_err(QualityCheckError::Jev)?;

        let usage = eval_res.usage.clone();
        let cached_entry = CachedFileResult {
            format_version: CACHE_FORMAT_VERSION,
            file_hash,
            metrics_hash: metrics_hash.to_string(),
            timestamp: Utc::now(),
            metrics: eval_res.metrics.clone(),
            usage: Some(usage.clone()),
        };
        let _ = put_cached_result(project_root, &cache_key, &cached_entry);
        (eval_res.metrics, false, Some(usage))
    };

    let profile_evals = evaluate_file_with_metrics(profiles, &metric_results);
    let relative_path = compute_relative_display_path(path, target_root);

    Ok(FileEvaluation {
        path: path.to_path_buf(),
        relative_path,
        served_from_cache,
        usage,
        context_files: related.into_iter().map(|r| r.path).collect(),
        profiles: profile_evals,
    })
}

fn compute_relative_display_path(path: &Path, target_root: &Path) -> String {
    if target_root.is_file() {
        path.to_string_lossy().to_string()
    } else {
        let stripped = path
            .strip_prefix(target_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if stripped.is_empty() {
            path.to_string_lossy().to_string()
        } else {
            stripped
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct FilePreview {
    pub path: PathBuf,
    pub relative_path: String,
    pub served_from_cache: bool,
    pub estimated_input_tokens: u64,
    pub estimated_cost_usd: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_files: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ScanPreviewResult {
    pub target_path: String,
    pub total_files: usize,
    pub cached_files: usize,
    pub uncached_files: usize,
    pub profiles: Vec<String>,
    pub total_metrics: usize,
    pub total_estimated_tokens: u64,
    pub total_estimated_cost_usd: f64,
    pub is_full: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_uncached: Vec<FilePreview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FilePreview>,
}

pub fn estimate_file_tokens(state_len: usize, metrics: &[Metric], with_context: bool) -> u64 {
    let baseline_system_prompt = 250u64;
    let questions_tokens: u64 = metrics
        .iter()
        .map(|metric| estimate_question_tokens(metric, with_context))
        .sum();
    baseline_system_prompt + questions_tokens + chars_to_tokens(state_len)
}

/// Question text (instructions and rubric) plus per-question framing, and the same again for
/// the `applies_when` yes/no question when the metric has one. With context, every question
/// also carries the context preamble.
fn estimate_question_tokens(metric: &Metric, with_context: bool) -> u64 {
    let framing_tokens =
        30 + if with_context { chars_to_tokens(CONTEXT_PREAMBLE.len()) } else { 0 };
    let rubric_len: usize = match &metric.rubric {
        Some(Rubric::Levels(levels)) => levels.iter().map(String::len).sum(),
        Some(Rubric::Descriptions(map)) => map.iter().map(|(k, v)| k.len() + v.len()).sum(),
        None => 0,
    };
    let main_question = framing_tokens + chars_to_tokens(metric.question.len() + rubric_len);
    let applicability_question = metric
        .applies_when
        .as_ref()
        .map_or(0, |condition| framing_tokens + chars_to_tokens(condition.len()));
    main_question + applicability_question
}

fn chars_to_tokens(len: usize) -> u64 {
    (len as f64 / 3.5).ceil() as u64
}

pub fn run_preview_pipeline(
    target_path: &Path,
    file_paths: &[PathBuf],
    profiles: &[Profile],
    project_root: &Path,
    full: bool,
) -> Result<ScanPreviewResult, QualityCheckError> {
    let inputs: Vec<ScanInput> = file_paths.iter().cloned().map(ScanInput::from_disk).collect();
    run_preview_pipeline_on_inputs(target_path, &inputs, profiles, project_root, full)
}

pub fn run_preview_pipeline_on_inputs(
    target_path: &Path,
    inputs: &[ScanInput],
    profiles: &[Profile],
    project_root: &Path,
    full: bool,
) -> Result<ScanPreviewResult, QualityCheckError> {
    let metrics_hash = compute_active_metrics_hash(profiles);
    let all_metrics = collect_unique_metrics(profiles);
    let total_metrics = all_metrics.len();

    let mut file_previews = Vec::with_capacity(inputs.len());
    let mut cached_count = 0;
    let mut total_tokens = 0u64;

    for input in inputs {
        let path = &input.path;
        let read_bytes;
        let file_bytes: &[u8] = match &input.content {
            Some(bytes) => bytes,
            None => {
                read_bytes = std::fs::read(path).map_err(|e| {
                    QualityCheckError::Scan(ScanError::FileReadError(path.clone(), e))
                })?;
                &read_bytes
            }
        };

        let (state, cache_scope) =
            evaluation_state(file_bytes, path, project_root, &input.related, &metrics_hash);
        let (cache_key, _) = compute_cache_key(file_bytes, &cache_scope);
        let in_cache = get_cached_result(project_root, &cache_key).is_some();
        let state_len = match &state {
            Value::String(text) => text.len(),
            other => other.to_string().len(),
        };

        let (est_tokens, est_cost) = if in_cache {
            cached_count += 1;
            (0u64, 0.0f64)
        } else {
            let tokens = estimate_file_tokens(state_len, &all_metrics, !input.related.is_empty());
            let cost = (tokens as f64 * 0.042) / 1_000_000.0;
            total_tokens += tokens;
            (tokens, cost)
        };

        let relative_path = compute_relative_display_path(path, target_path);

        file_previews.push(FilePreview {
            path: path.clone(),
            relative_path,
            served_from_cache: in_cache,
            estimated_input_tokens: est_tokens,
            estimated_cost_usd: est_cost,
            context_files: input.related.iter().map(|r| r.path.clone()).collect(),
        });
    }

    file_previews.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    let total_files = file_previews.len();
    let uncached_files = total_files - cached_count;

    let mut uncached_sorted: Vec<FilePreview> = file_previews
        .iter()
        .filter(|f| !f.served_from_cache)
        .cloned()
        .collect();
    uncached_sorted.sort_by_key(|f| std::cmp::Reverse(f.estimated_input_tokens));

    let top_uncached = if !full && total_files > 5 {
        uncached_sorted.into_iter().take(5).collect()
    } else {
        Vec::new()
    };

    let files_to_include = if full || total_files <= 5 {
        file_previews
    } else {
        Vec::new()
    };

    let total_cost = (total_tokens as f64 * 0.042) / 1_000_000.0;
    let profiles_used = profiles.iter().map(|p| p.name.clone()).collect();

    Ok(ScanPreviewResult {
        target_path: target_path.to_string_lossy().to_string(),
        total_files,
        cached_files: cached_count,
        uncached_files,
        profiles: profiles_used,
        total_metrics,
        total_estimated_tokens: total_tokens,
        total_estimated_cost_usd: total_cost,
        is_full: full,
        top_uncached,
        files: files_to_include,
    })
}
