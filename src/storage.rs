use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::scorer::ScanRunResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunListItem {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub file_count: usize,
    pub passed: bool,
    pub path: PathBuf,
}

pub const STATE_DIR_NAME: &str = ".qualitycheck";

pub fn get_state_dir(project_root: &Path) -> PathBuf {
    project_root.join(STATE_DIR_NAME)
}

/// Creates `.qualitycheck/` with a `*` gitignore inside it, so git never reports the cache or
/// run reports as untracked files (and `patch` never picks them up), regardless of whether the
/// user's own `.gitignore` lists the directory.
pub fn ensure_state_dir(project_root: &Path) -> io::Result<PathBuf> {
    let state_dir = get_state_dir(project_root);
    fs::create_dir_all(&state_dir)?;

    let gitignore = state_dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "# Created by qualitycheck automatically.\n*\n")?;
    }
    Ok(state_dir)
}

pub fn get_runs_dir(project_root: &Path) -> PathBuf {
    get_state_dir(project_root).join("runs")
}

pub fn persist_run_result(
    result: &ScanRunResult,
    custom_save_path: Option<&Path>,
    no_persist: bool,
    project_root: &Path,
) -> io::Result<Option<PathBuf>> {
    if no_persist {
        return Ok(None);
    }

    let json = serde_json::to_string_pretty(result)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    if let Some(save_path) = custom_save_path {
        if let Some(parent) = save_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(save_path, json)?;
        return Ok(Some(save_path.to_path_buf()));
    }

    let runs_dir = ensure_state_dir(project_root)?.join("runs");
    fs::create_dir_all(&runs_dir)?;

    let run_file = runs_dir.join(format!("{}.json", result.run_id));
    fs::write(&run_file, &json)?;

    let latest_file = runs_dir.join("latest.json");
    fs::write(&latest_file, &json)?;

    Ok(Some(run_file))
}

pub fn load_saved_run(run_id_or_path: &str, project_root: &Path) -> io::Result<ScanRunResult> {
    let direct_path = Path::new(run_id_or_path);
    if direct_path.is_file() {
        let content = fs::read_to_string(direct_path)?;
        return serde_json::from_str(&content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
    }

    let runs_dir = get_runs_dir(project_root);
    let run_path = if run_id_or_path == "latest" || run_id_or_path == "latest.json" {
        runs_dir.join("latest.json")
    } else if run_id_or_path.ends_with(".json") {
        runs_dir.join(run_id_or_path)
    } else {
        runs_dir.join(format!("{}.json", run_id_or_path))
    };

    if !run_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Run file '{}' not found in {}", run_id_or_path, runs_dir.display()),
        ));
    }

    let content = fs::read_to_string(&run_path)?;
    serde_json::from_str(&content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn list_saved_runs(project_root: &Path) -> io::Result<Vec<RunListItem>> {
    let runs_dir = get_runs_dir(project_root);
    if !runs_dir.exists() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    for entry in fs::read_dir(&runs_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "json") {
            let filename = path.file_name().unwrap_or_default().to_string_lossy();
            if filename == "latest.json" {
                continue;
            }

            if let Ok(content) = fs::read_to_string(&path)
                && let Ok(run) = serde_json::from_str::<ScanRunResult>(&content) {
                    items.push(RunListItem {
                        run_id: run.run_id,
                        timestamp: run.timestamp,
                        file_count: run.total_files,
                        passed: run.passed,
                        path: path.clone(),
                    });
                }
        }
    }

    items.sort_by_key(|a| std::cmp::Reverse(a.timestamp));
    Ok(items)
}
