use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Semaphore;

use crate::cache::{
    compute_cache_key, get_cached_result, put_cached_result, CachedFileResult,
};
use crate::error::{QualityCheckError, ScanError};
use crate::jev_client::JevClient;
use crate::profile::{compute_active_metrics_hash, Metric, Profile};
use crate::scorer::{evaluate_file_with_metrics, FileEvaluation, ScanRunResult};

/// Orquestación concurrente y cacheada del pipeline de evaluación de archivos.
pub async fn run_scan_pipeline(
    target_path: &Path,
    file_paths: &[PathBuf],
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
    let mut tasks = Vec::with_capacity(file_paths.len());

    for file_path in file_paths {
        let path = file_path.clone();
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
                &path,
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
    path: &Path,
    target_root: &Path,
    project_root: &Path,
    metrics_hash: &str,
    metrics: &[Metric],
    profiles: &[Profile],
    client: &JevClient,
) -> Result<FileEvaluation, QualityCheckError> {
    let file_bytes = std::fs::read(path)
        .map_err(|e| QualityCheckError::Scan(ScanError::FileReadError(path.to_path_buf(), e)))?;

    let (cache_key, file_hash) = compute_cache_key(&file_bytes, metrics_hash);

    let (metric_results, served_from_cache, usage) = if let Some(cached) = get_cached_result(project_root, &cache_key) {
        (cached.metrics, true, cached.usage)
    } else {
        let content_str = match String::from_utf8(file_bytes.clone()) {
            Ok(s) => s,
            Err(_) => String::from_utf8_lossy(&file_bytes).into_owned(),
        };

        let eval_res = client
            .evaluate_file(&content_str, metrics)
            .await
            .map_err(QualityCheckError::Jev)?;

        let usage = eval_res.usage.clone();
        let cached_entry = CachedFileResult {
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
    pub files: Vec<FilePreview>,
}

pub fn estimate_file_tokens(file_bytes_len: usize, metric_count: usize) -> u64 {
    let baseline_system_prompt = 250u64;
    let questions_tokens = (metric_count as u64) * 55;
    let content_tokens = (file_bytes_len as f64 / 3.5).ceil() as u64;
    baseline_system_prompt + questions_tokens + content_tokens
}

pub fn run_preview_pipeline(
    target_path: &Path,
    file_paths: &[PathBuf],
    profiles: &[Profile],
    project_root: &Path,
) -> Result<ScanPreviewResult, QualityCheckError> {
    let metrics_hash = compute_active_metrics_hash(profiles);
    let all_metrics = collect_unique_metrics(profiles);
    let total_metrics = all_metrics.len();

    let mut file_previews = Vec::with_capacity(file_paths.len());
    let mut cached_count = 0;
    let mut total_tokens = 0u64;

    for path in file_paths {
        let file_bytes = std::fs::read(path)
            .map_err(|e| QualityCheckError::Scan(ScanError::FileReadError(path.clone(), e)))?;

        let (cache_key, _) = compute_cache_key(&file_bytes, &metrics_hash);
        let in_cache = get_cached_result(project_root, &cache_key).is_some();

        let (est_tokens, est_cost) = if in_cache {
            cached_count += 1;
            (0u64, 0.0f64)
        } else {
            let tokens = estimate_file_tokens(file_bytes.len(), total_metrics);
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
        });
    }

    file_previews.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    let total_cost = (total_tokens as f64 * 0.042) / 1_000_000.0;
    let profiles_used = profiles.iter().map(|p| p.name.clone()).collect();

    Ok(ScanPreviewResult {
        target_path: target_path.to_string_lossy().to_string(),
        total_files: file_previews.len(),
        cached_files: cached_count,
        uncached_files: file_previews.len() - cached_count,
        profiles: profiles_used,
        total_metrics,
        total_estimated_tokens: total_tokens,
        total_estimated_cost_usd: total_cost,
        files: file_previews,
    })
}
