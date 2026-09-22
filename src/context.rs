//! Cross-file context: which other files to show Jev next to the file being scored, so a file
//! that delegates work (validation to a service, logic tested elsewhere) isn't judged as if that
//! work were missing. Selection is deterministic file-name matching; it makes no model calls.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use aho_corasick::AhoCorasick;
use serde::{Deserialize, Serialize};

use crate::git::ChangedFile;
use crate::pipeline::ScanInput;
use crate::walker::{collect_files, is_content_eligible, WalkerOptions};

/// Upper bound on related-file content sent with one file (roughly 7k tokens).
pub const MAX_CONTEXT_CHARS: usize = 24_000;
/// Upper bound per related file; longer files are cut and marked `truncated`.
pub const MAX_RELATED_FILE_CHARS: usize = 8_000;
/// Remaining budget below which no further related file is added.
const MIN_USEFUL_CHARS: usize = 500;

/// Names shorter than this, or listed in `GENERIC_NAMES`, occur in too much unrelated text to
/// signal that one file refers to another.
const MIN_NAME_LEN: usize = 4;
const GENERIC_NAMES: &[&str] = &[
    "main", "index", "init", "utils", "util", "types", "common", "helpers", "constants", "config",
];
const TEST_WORDS: &[&str] = &["test", "tests", "spec", "specs"];

/// Extensions that can refer to one another's code. Related files must share the scored file's
/// extension or one of its groups, so a mention of "quality" doesn't pull in `quality.json`.
const LANGUAGE_GROUPS: &[&[&str]] = &[
    &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "vue", "svelte"],
    &["java", "kt", "kts", "scala", "groovy"],
    &["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "m", "mm"],
    &["cs", "fs", "vb"],
    &["py", "pyi"],
    &["rb", "erb"],
    &["php", "phtml"],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Relation {
    #[serde(rename = "tests this file")]
    TestsThisFile,
    #[serde(rename = "referenced by this file")]
    ReferencedByThisFile,
    #[serde(rename = "references this file")]
    ReferencesThisFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedFile {
    /// Path relative to the repository root.
    pub path: String,
    pub relation: Relation,
    pub content: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Finds which file reference names a text mentions, and how often.
struct NameMatcher {
    matcher: Option<AhoCorasick>,
    names: Vec<String>,
}

impl NameMatcher {
    fn new(names: impl IntoIterator<Item = String>) -> Self {
        let names: Vec<String> = names.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        let matcher = (!names.is_empty()).then(|| AhoCorasick::new(&names).expect("literal patterns"));
        Self { matcher, names }
    }

    fn mentions(&self, text: &str) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        if let Some(matcher) = &self.matcher {
            for m in matcher.find_overlapping_iter(&normalize(text)) {
                *counts.entry(self.names[m.pattern().as_usize()].clone()).or_insert(0) += 1;
            }
        }
        counts
    }
}

/// Candidate context files for one side of a run, indexed by which file names each mentions.
pub struct ContextPool {
    /// Repository-relative path → content.
    files: BTreeMap<String, String>,
    matcher: NameMatcher,
    /// Repository-relative path → reference names its content mentions, with counts.
    mentions: HashMap<String, HashMap<String, usize>>,
}

impl ContextPool {
    pub fn new(files: BTreeMap<String, String>) -> Self {
        let matcher = NameMatcher::new(files.keys().filter_map(|rel| reference_name(rel)));
        let mentions = files
            .iter()
            .map(|(rel, content)| (rel.clone(), matcher.mentions(content)))
            .collect();
        Self { files, matcher, mentions }
    }

    /// Files to show alongside `target_rel`: its tests, then files it refers to by name, then
    /// files that refer to it. Within each group, files mentioned more often come first, then
    /// files closer in the directory tree. Stops at `MAX_CONTEXT_CHARS`.
    pub fn related_to(&self, target_rel: &str, target_content: &str) -> Vec<RelatedFile> {
        let target_name = reference_name(target_rel);
        let target_is_test = is_test_file(target_rel);
        let target_mentions = self.matcher.mentions(target_content);

        let mut picked: Vec<(Relation, Reverse<usize>, usize, &String)> = Vec::new();
        for rel in self
            .files
            .keys()
            .filter(|rel| rel.as_str() != target_rel && same_language(target_rel, rel))
        {
            let name = reference_name(rel);
            let referenced = name.as_ref().and_then(|n| target_mentions.get(n)).copied();
            // Code refers to the code under test, not to its tests, so a test file (whose name
            // is the tested file's name) is never "referenced".
            let references = target_name
                .as_ref()
                .filter(|_| !target_is_test)
                .and_then(|n| self.mentions.get(rel)?.get(n))
                .copied();

            let (relation, count) = if is_test_file(rel) {
                if target_is_test || name.is_none() || name != target_name {
                    continue;
                }
                (Relation::TestsThisFile, 0)
            } else if let Some(count) = referenced {
                (Relation::ReferencedByThisFile, count)
            } else if let Some(count) = references {
                (Relation::ReferencesThisFile, count)
            } else {
                continue;
            };
            picked.push((relation, Reverse(count), directory_distance(target_rel, rel), rel));
        }
        picked.sort();

        let mut remaining = MAX_CONTEXT_CHARS;
        let mut related = Vec::new();
        for (relation, _, _, rel) in picked {
            if remaining < MIN_USEFUL_CHARS {
                break;
            }
            let (content, truncated) =
                truncate_chars(&self.files[rel], MAX_RELATED_FILE_CHARS.min(remaining));
            remaining = remaining.saturating_sub(content.chars().count());
            related.push(RelatedFile {
                path: rel.clone(),
                relation,
                content,
                truncated,
            });
        }
        related
    }
}

/// Context candidates for one side of a run, as repository-relative path → content: the files
/// being scored (`targets`, with their content on this side), the other files in their
/// directories, files anywhere in the repository whose name a target mentions, and test files
/// named after a target. `overrides` supplies this side's content where it differs from the
/// working tree — for the base side of a delta, each changed file's base version, or `None`
/// where it didn't exist.
pub fn gather_candidates(
    project_root: &Path,
    targets: &[(String, String)],
    overrides: &HashMap<String, Option<String>>,
    max_file_size_kb: u64,
) -> BTreeMap<String, String> {
    let walker_opts = WalkerOptions {
        max_file_size_kb,
        ..WalkerOptions::default()
    };
    let repo_files: Vec<(String, PathBuf)> = collect_files(project_root, &walker_opts)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| Some((to_slash(path.strip_prefix(project_root).ok()?), path)))
        .collect();

    let target_dirs: HashSet<PathBuf> = targets
        .iter()
        .filter_map(|(rel, _)| Path::new(rel).parent().map(Path::to_path_buf))
        .collect();
    let target_names: HashSet<String> = targets.iter().filter_map(|(rel, _)| reference_name(rel)).collect();
    let repo_names = NameMatcher::new(repo_files.iter().filter_map(|(rel, _)| reference_name(rel)));
    let mentioned_names: HashSet<String> = targets
        .iter()
        .flat_map(|(_, content)| repo_names.mentions(content).into_keys())
        .collect();

    let mut candidates: BTreeMap<String, String> = targets.iter().cloned().collect();
    for (rel, path) in repo_files {
        if candidates.contains_key(&rel) {
            continue;
        }
        let name = reference_name(&rel);
        let in_scope = Path::new(&rel).parent().is_some_and(|dir| target_dirs.contains(dir))
            || name.as_ref().is_some_and(|n| mentioned_names.contains(n))
            || (is_test_file(&rel) && name.as_ref().is_some_and(|n| target_names.contains(n)));
        if !in_scope {
            continue;
        }
        let content = match overrides.get(&rel) {
            Some(content) => content.clone(),
            None => fs::read(&path)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        };
        if let Some(content) = content {
            candidates.insert(rel, content);
        }
    }
    candidates
}

/// Attaches related files to every input, drawing on one pool gathered for this side of the run.
pub fn attach_related_files(
    inputs: &mut [ScanInput],
    project_root: &Path,
    overrides: &HashMap<String, Option<String>>,
    max_file_size_kb: u64,
) {
    let root = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    let targets: Vec<(String, String)> = inputs
        .iter()
        .map(|input| {
            let bytes = match &input.content {
                Some(bytes) => bytes.clone(),
                None => fs::read(&input.path).unwrap_or_default(),
            };
            (repo_relative(&input.path, &root), String::from_utf8_lossy(&bytes).into_owned())
        })
        .collect();
    let pool = ContextPool::new(gather_candidates(&root, &targets, overrides, max_file_size_kb));

    for (input, (rel, content)) in inputs.iter_mut().zip(&targets) {
        input.related = pool.related_to(rel, content);
    }
}

fn same_language(a: &str, b: &str) -> bool {
    let extension = |rel: &str| {
        Path::new(rel)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
    };
    match (extension(a), extension(b)) {
        (Some(a), Some(b)) => {
            a == b
                || LANGUAGE_GROUPS
                    .iter()
                    .any(|group| group.contains(&a.as_str()) && group.contains(&b.as_str()))
        }
        _ => false,
    }
}

/// Number of directory steps between two files' directories.
fn directory_distance(a: &str, b: &str) -> usize {
    let dirs = |rel: &str| -> Vec<String> {
        Path::new(rel)
            .parent()
            .map(|dir| dir.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    };
    let (a_dirs, b_dirs) = (dirs(a), dirs(b));
    let shared = a_dirs.iter().zip(&b_dirs).take_while(|(x, y)| x == y).count();
    (a_dirs.len() - shared) + (b_dirs.len() - shared)
}

/// Context overrides for the base side of a delta: each changed file's base version, or `None`
/// for files that didn't exist (or were binary or oversized) at the base.
pub fn base_side_overrides(
    changed: &[ChangedFile],
    project_root: &Path,
    max_file_size_kb: u64,
) -> HashMap<String, Option<String>> {
    let root = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    changed
        .iter()
        .map(|file| {
            let base = file
                .base_content
                .as_deref()
                .filter(|bytes| is_content_eligible(bytes, max_file_size_kb))
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
            (repo_relative(&file.path, &root), base)
        })
        .collect()
}

/// `path` relative to `root` (already canonical), with `/` separators.
pub fn repo_relative(path: &Path, root: &Path) -> String {
    let absolute = path.canonicalize().unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    });
    to_slash(absolute.strip_prefix(root).unwrap_or(path))
}

fn to_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Lowercase alphanumerics only, so `UserService`, `user_service`, and `user-service` compare equal.
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Lowercase words of a file stem, split at separators and camelCase boundaries.
fn name_words(rel: &str) -> Vec<String> {
    let stem = Path::new(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for c in stem.chars() {
        if !c.is_alphanumeric() || (c.is_uppercase() && previous_lowercase) {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lowercase = false;
            if !c.is_alphanumeric() {
                continue;
            }
        }
        current.extend(c.to_lowercase());
        previous_lowercase = c.is_lowercase() || c.is_numeric();
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

pub fn is_test_file(rel: &str) -> bool {
    let in_test_dir = Path::new(rel).parent().is_some_and(|dir| {
        dir.components()
            .any(|c| TEST_WORDS.contains(&normalize(&c.as_os_str().to_string_lossy()).as_str()))
    });
    in_test_dir || name_words(rel).iter().any(|w| TEST_WORDS.contains(&w.as_str()))
}

/// The name other files use for this one: its stem without test words, normalized. `None` when
/// too short or generic to identify it in other files' text.
pub fn reference_name(rel: &str) -> Option<String> {
    let name: String = name_words(rel)
        .into_iter()
        .filter(|w| !TEST_WORDS.contains(&w.as_str()))
        .collect();
    (name.len() >= MIN_NAME_LEN && !GENERIC_NAMES.contains(&name.as_str())).then_some(name)
}

fn truncate_chars(content: &str, limit: usize) -> (String, bool) {
    if content.chars().count() <= limit {
        (content.to_string(), false)
    } else {
        let mut cut: String = content.chars().take(limit).collect();
        cut.push_str("\n[... truncated]");
        (cut, true)
    }
}
