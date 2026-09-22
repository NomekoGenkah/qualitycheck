use std::collections::HashSet;
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
    /// Score every text file, not only source code (see `is_source_file`).
    pub all_files: bool,
}

impl Default for WalkerOptions {
    fn default() -> Self {
        Self {
            no_ignore: false,
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_size_kb: 200,
            all_files: false,
        }
    }
}

/// Files selected for scoring, and how many were passed over for not being source code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectedFiles {
    pub files: Vec<PathBuf>,
    pub skipped_non_source: usize,
}

pub fn collect_files(target: &Path, options: &WalkerOptions) -> Result<Vec<PathBuf>, ScanError> {
    Ok(walk_target(target, options)?.files)
}

/// Collects files from several targets, dropping files reached through more than one target
/// (e.g. `src` and `src/main.rs`, or `a.rs` and `./a.rs`).
pub fn collect_files_from_targets(
    targets: &[PathBuf],
    options: &WalkerOptions,
) -> Result<CollectedFiles, ScanError> {
    let mut seen = HashSet::new();
    let mut collected = CollectedFiles::default();
    for target in targets {
        let walked = walk_target(target, options)?;
        collected.skipped_non_source += walked.skipped_non_source;
        for file in walked.files {
            let identity = file.canonicalize().unwrap_or_else(|_| file.clone());
            if seen.insert(identity) {
                collected.files.push(file);
            }
        }
    }
    Ok(collected)
}

/// Applies the same eligibility rules as a directory walk (internal paths, source-code filter,
/// include/exclude globs, size and binary checks) to an explicit list of paths, such as files
/// git reports as changed.
pub fn filter_candidate_files(
    paths: &[PathBuf],
    root: &Path,
    options: &WalkerOptions,
) -> Result<CollectedFiles, ScanError> {
    let include_set = build_glob_set(&options.include)?;
    let exclude_set = build_glob_set(&options.exclude)?;

    let mut collected = CollectedFiles::default();
    for path in paths {
        let relative_path = path.strip_prefix(root).unwrap_or(path);
        if is_internal_qualitycheck_path(relative_path)
            || !matches_filters(path, root, &include_set, &exclude_set)
        {
            continue;
        }
        classify(path, relative_path, options, &mut collected);
    }
    Ok(collected)
}

fn walk_target(target: &Path, options: &WalkerOptions) -> Result<CollectedFiles, ScanError> {
    if !target.exists() {
        return Err(ScanError::PathNotFound(target.to_path_buf()));
    }

    let include_set = build_glob_set(&options.include)?;
    let exclude_set = build_glob_set(&options.exclude)?;

    // A file named explicitly is scored whatever its type: the caller asked for it.
    if target.is_file() {
        let files = if matches_filters(target, target, &include_set, &exclude_set)
            && file_head(target, options.max_file_size_kb).is_some()
        {
            vec![target.to_path_buf()]
        } else {
            Vec::new()
        };
        return Ok(CollectedFiles { files, skipped_non_source: 0 });
    }

    let mut builder = WalkBuilder::new(target);
    builder
        .hidden(false)
        .git_ignore(!options.no_ignore)
        .git_global(!options.no_ignore)
        .git_exclude(!options.no_ignore)
        .ignore(!options.no_ignore)
        .parents(!options.no_ignore);

    let mut collected = CollectedFiles::default();

    for result in builder.build() {
        match result {
            Ok(entry) => {
                let path = entry.path();
                // Skip git internal files or .qualitycheck run/cache files
                if !path.is_file()
                    || is_internal_qualitycheck_path(path)
                    || !matches_filters(path, target, &include_set, &exclude_set)
                {
                    continue;
                }
                let relative_path = path.strip_prefix(target).unwrap_or(path);
                classify(path, relative_path, options, &mut collected);
            }
            Err(e) => {
                eprintln!("Warning: Error walking directory entry: {}", e);
            }
        }
    }

    collected.files.sort();
    Ok(collected)
}

/// Adds `path` to the collected files when it's eligible and (unless `all_files`) source code;
/// counts it as skipped when it's an eligible file that isn't source code.
fn classify(path: &Path, relative_path: &Path, options: &WalkerOptions, collected: &mut CollectedFiles) {
    if !options.all_files && !is_source_file(relative_path) {
        collected.skipped_non_source += 1;
        return;
    }
    match file_head(path, options.max_file_size_kb) {
        Some(head) if !options.all_files && is_generated(&head) => collected.skipped_non_source += 1,
        Some(_) => collected.files.push(path.to_path_buf()),
        None => {}
    }
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

/// The first bytes of an eligible file (non-empty, within the size limit, not binary), for the
/// generated-code check; `None` when the file isn't eligible.
fn file_head(path: &Path, max_file_size_kb: u64) -> Option<Vec<u8>> {
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Warning: Could not read metadata for '{}': {}", path.display(), e);
            return None;
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
        return None;
    }

    if size_bytes == 0 {
        return None;
    }

    let mut buffer = vec![0u8; SNIFF_BYTES];
    let bytes_read = fs::File::open(path).and_then(|mut f| f.read(&mut buffer)).ok()?;
    buffer.truncate(bytes_read);

    // Binary files contain null bytes early on.
    (!buffer.contains(&0)).then_some(buffer)
}

/// The in-memory counterpart of the size, emptiness, and binary checks applied to files on disk.
pub fn is_content_eligible(bytes: &[u8], max_file_size_kb: u64) -> bool {
    let sniff_len = bytes.len().min(SNIFF_BYTES);
    !bytes.is_empty()
        && (bytes.len() as u64) <= max_file_size_kb * 1024
        && !bytes[..sniff_len].contains(&0)
}

pub fn is_binary_file(path: &Path) -> bool {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true,
    };

    let mut buffer = [0u8; SNIFF_BYTES];
    let bytes_read = match file.read(&mut buffer) {
        Ok(n) => n,
        Err(_) => return true,
    };

    let slice = &buffer[..bytes_read];
    slice.contains(&0)
}

const SNIFF_BYTES: usize = 8192;

/// Extensions of source code worth scoring. Everything else (docs, config, data, lockfiles,
/// assets) is skipped in directory walks and `patch` unless `--all-files` is set.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "go", "py", "pyi", "rb", "php", "java", "kt", "kts", "scala", "groovy", "swift",
    "m", "mm", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "cs", "fs", "vb", "js", "jsx",
    "mjs", "cjs", "ts", "tsx", "mts", "cts", "vue", "svelte", "dart", "lua", "pl", "pm", "r",
    "jl", "ex", "exs", "erl", "hrl", "clj", "cljs", "hs", "ml", "mli", "elm", "zig", "nim",
    "sol", "sh", "bash", "zsh", "ps1", "sql",
];

/// Directories holding third-party or build-output code, skipped even when not gitignored.
const VENDORED_DIRS: &[&str] = &[
    "vendor", "node_modules", "third_party", "third-party", "bower_components", "Pods",
    ".venv", "venv", "site-packages", "dist",
];

/// File-name endings of minified bundles and common code generators' output.
const GENERATED_SUFFIXES: &[&str] = &[
    ".min.js", ".min.mjs", "-min.js", ".bundle.js", ".pb.go", "_pb2.py", "_pb2_grpc.py",
    ".g.dart", ".freezed.dart", ".designer.cs",
];

/// Whether a path looks like hand-written source code: a source extension, outside vendored
/// directories, and not named like minified or generated output.
pub fn is_source_file(relative_path: &Path) -> bool {
    let in_vendored_dir = relative_path.parent().is_some_and(|dir| {
        dir.components()
            .any(|c| VENDORED_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
    });
    if in_vendored_dir {
        return false;
    }

    let file_name = relative_path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if GENERATED_SUFFIXES.iter().any(|suffix| file_name.ends_with(suffix))
        || file_name.contains(".generated.")
        || file_name.contains("_generated.")
    {
        return false;
    }

    relative_path.extension().is_some_and(|ext| {
        SOURCE_EXTENSIONS.contains(&ext.to_string_lossy().to_lowercase().as_str())
    })
}

/// Whether a file declares itself generated in a comment near its top, following common
/// conventions (Go's "Code generated ... DO NOT EDIT.", .NET's auto-generated tag, the
/// at-generated marker, "this file is automatically generated"). Only comment lines count, so
/// a string literal mentioning a marker doesn't exclude hand-written code.
pub fn is_generated(head: &[u8]) -> bool {
    const COMMENT_PREFIXES: &[&str] = &["//", "#", "/*", "*", "<!--", "--", ";", "%", "'"];
    String::from_utf8_lossy(head)
        .lines()
        .take(20)
        .map(|line| line.trim_start().to_lowercase())
        .filter(|line| COMMENT_PREFIXES.iter().any(|prefix| line.starts_with(prefix)))
        .any(|line| {
            line.contains("@generated")
                || (line.contains("code generated") && line.contains("do not edit"))
                || line.contains("<auto-generated")
                || line.contains("automatically generated")
                || line.contains("auto-generated by")
                || line.contains("autogenerated by")
        })
}
