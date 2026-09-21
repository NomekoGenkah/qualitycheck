use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;

use crate::error::ScanError;

#[derive(Debug, Clone)]
pub struct WalkerOptions {
    pub no_ignore: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_size_kb: u64,
}

impl Default for WalkerOptions {
    fn default() -> Self {
        Self {
            no_ignore: false,
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_size_kb: 200,
        }
    }
}

pub fn collect_files(target: &Path, options: &WalkerOptions) -> Result<Vec<PathBuf>, ScanError> {
    if !target.exists() {
        return Err(ScanError::PathNotFound(target.to_path_buf()));
    }

    let include_set = build_glob_set(&options.include)?;
    let exclude_set = build_glob_set(&options.exclude)?;

    if target.is_file() {
        if matches_filters(target, target, &include_set, &exclude_set)
            && is_file_eligible(target, options.max_file_size_kb) {
                return Ok(vec![target.to_path_buf()]);
            }
        return Ok(Vec::new());
    }

    let mut builder = WalkBuilder::new(target);
    builder
        .hidden(false)
        .git_ignore(!options.no_ignore)
        .git_global(!options.no_ignore)
        .git_exclude(!options.no_ignore)
        .ignore(!options.no_ignore)
        .parents(!options.no_ignore);

    let mut files = Vec::new();

    for result in builder.build() {
        match result {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file() {
                    // Skip git internal files or .qualitycheck run/cache files
                    if is_internal_qualitycheck_path(path) {
                        continue;
                    }

                    if matches_filters(path, target, &include_set, &exclude_set)
                        && is_file_eligible(path, options.max_file_size_kb) {
                            files.push(path.to_path_buf());
                        }
                }
            }
            Err(e) => {
                eprintln!("Warning: Error walking directory entry: {}", e);
            }
        }
    }

    files.sort();
    Ok(files)
}

fn build_glob_set(patterns: &[String]) -> Result<Option<GlobSet>, ScanError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern)
            .map_err(|e| ScanError::GlobError(pattern.clone(), e))?;
        builder.add(glob);
    }
    let set = builder.build()
        .map_err(|e| ScanError::GlobError("failed to compile globset".to_string(), e))?;
    Ok(Some(set))
}

fn matches_filters(
    path: &Path,
    root: &Path,
    include_set: &Option<GlobSet>,
    exclude_set: &Option<GlobSet>,
) -> bool {
    let relative_path = path.strip_prefix(root).unwrap_or(path);

    if let Some(excludes) = exclude_set
        && (excludes.is_match(relative_path) || excludes.is_match(path)) {
            return false;
        }

    if let Some(includes) = include_set
        && !includes.is_match(relative_path) && !includes.is_match(path) {
            return false;
        }

    true
}

fn is_internal_qualitycheck_path(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if name_str == ".qualitycheck" || name_str == ".git" {
                return true;
            }
        }
    }
    false
}

fn is_file_eligible(path: &Path, max_file_size_kb: u64) -> bool {
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Warning: Could not read metadata for '{}': {}", path.display(), e);
            return false;
        }
    };

    let size_bytes = metadata.len();
    let max_bytes = max_file_size_kb * 1024;
    if size_bytes > max_bytes {
        let size_kb = size_bytes.div_ceil(1024);
        eprintln!(
            "Warning: Skipping large file '{}' ({} KB > {} KB limit)",
            path.display(),
            size_kb,
            max_file_size_kb
        );
        return false;
    }

    if size_bytes == 0 {
        return false;
    }

    // Check if binary by inspecting initial chunk for null bytes
    if is_binary_file(path) {
        return false;
    }

    true
}

pub fn is_binary_file(path: &Path) -> bool {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true,
    };

    let mut buffer = [0u8; 8192];
    let bytes_read = match file.read(&mut buffer) {
        Ok(n) => n,
        Err(_) => return true,
    };

    let slice = &buffer[..bytes_read];
    slice.contains(&0)
}
