# qualitycheck

[![CI](https://github.com/NomekoGenkah/qualitycheck/actions/workflows/ci.yml/badge.svg)](https://github.com/NomekoGenkah/qualitycheck/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-2024%20%7C%202021-orange.svg)](https://www.rust-lang.org/)

A fast, deterministic Rust CLI that uses the **Jev** API (TypeSafe AI) to score code quality in a quantifiable, configurable way — think "eslint, but for qualitative criteria" (naming clarity, dead code, cyclomatic complexity, cohesion, security posture) that traditional linters cannot evaluate.

Designed to be run by humans directly, in CI pipelines, or invoked by AI coding agents (Claude Code, Codex, Cursor, Antigravity, OpenCode) as a fast, deterministic shell tool.

---

## Key Features

- **Deterministic Aggregation**: Jev returns typed answers (`scale`, `enum`, `binary`) with calibrated probabilities. All aggregation (probability-weighted metric scores, weighted composites, threshold comparisons, confidence and applicability weighting, regression gating) is calculated deterministically in Rust.
- **Rubric-Described Metrics**: Every built-in metric describes each possible answer as a concrete situation, and can declare when it applies (`applies_when`), so a file isn't scored on concerns it doesn't own. Uncertain or likely-inapplicable answers count only partly, phasing in gradually so a borderline answer can't swing a composite. Low scores are shown with the rubric description of the answer.
- **Content-Addressed Caching**: Keyed by `blake3(file_content)` plus a hash of what is asked (questions, rubrics, conditions, and related files with `--context`). Re-scans cost zero API calls for unchanged files; changing a question re-queries, while changing weights or thresholds re-scores cached answers for free. `.qualitycheck/` ignores itself in git.
- **Offline Token & Cost Estimation (`--preview`)**: Pre-calculate token usage and estimated cost before making any API calls. Fully offline, 0 network requests, and aware of cached files.
- **Token & Cost Tracking**: Live runs report exact input/output tokens and cost ($0.042 / 1M input tokens), reporting $0.00 for cache hits.
- **Git-Aware Scans (`patch`)**: Scores files modified versus the working tree or a base branch, and with `--delta` / `--fail-on-regression` / `--fail-on-metric-regression` scores the base version too, so only regressions introduced by the change fail the gate.
- **Finding Locations (`--explain`)**: Asks Jev which regions of a file are responsible for each failing metric and reports them as line ranges.
- **Cross-File Context (`--context`)**: Sends related files (tests, referenced and referencing files) alongside each file, so delegated work isn't scored as missing.
- **CI-Ready**: `--format github` annotates findings on the changed files, at the lines `--explain` located, and writes a step summary; `--format markdown` renders the summary for pull request comments. See [`examples/github-workflow.yml`](examples/github-workflow.yml).
- **Agent-Ready**: Self-describing (`qualitycheck describe`), TTY-aware auto-disabling colors, clean JSON stdout (with logs/progress directed to stderr), and standard exit codes.
- **History & Triage**: Automatic persistence to `.qualitycheck/runs/` enables offline inspection with `gaps`, `file`, and `diff`.

---

## Installation & Setup

### From Source
```bash
cargo build --release
cp target/release/qualitycheck /usr/local/bin/ # or place in your PATH
```

Or install directly to your cargo bin:
```bash
cargo install --path .
```

### Agent Skill Installation
If you use AI coding agents (Claude Code, OpenCode, Cursor, Antigravity), install the global skill via `npx skills`:
```bash
npx skills add NomekoGenkah/qualitycheck -g -y
```

### First-Time Initialization

```bash
# Interactive setup: prompts for Jev API key, installs profiles, adds .qualitycheck/ to .gitignore
qualitycheck init

# Non-interactive / CI / Agent setup:
qualitycheck init --api-key <YOUR_JEV_API_KEY> --non-interactive
# Or export JEV_API_KEY:
export JEV_API_KEY="your-api-key"
```

---

## CLI Usage

### 1. Offline Token & Cost Preview (`--preview` / `--dry-run`)

By default, `--preview` outputs a **compact summary** (including top token-consuming files) to preserve LLM context windows and prevent output spam. Use `--full` (or `--verbose`) for a complete file-by-file breakdown:

```bash
# Compact preview for a directory (0 network requests, works offline)
qualitycheck scan . --preview

# Estimate cost for git uncommitted changes
qualitycheck patch --preview

# Full file-by-file breakdown (opt-in)
qualitycheck scan . --preview --full

# Machine-readable preview for agents (compact JSON by default)
qualitycheck scan src/ --preview --format json
```

### 2. Scanning Code

```bash
# Scan current directory with default "quality" profile
qualitycheck scan .

# Scan several specific files or directories in one run (e.g. new code vs. its precedent)
qualitycheck scan src/new_handler.rs src/old_handler.rs

# Scan with multiple profiles combined
qualitycheck scan ./src --profile quality,security --format table

# Output structured JSON for automation or agents
qualitycheck scan ./src --profile quality,security --format json

# Strict gating: exit code 1 if any file falls below profile fail_below threshold
qualitycheck scan ./src --strict

# Score every text file, not only source code
qualitycheck scan . --all-files
```

Directory scans and `patch` score only source code by default. They skip files without a
source-code extension (docs, config, data, lockfiles), vendored directories (`vendor/`,
`node_modules/`, `third_party/`, `dist/`, …), minified bundles, and generated code, detected by
name (`*.pb.go`, `*_pb2.py`, `*.generated.ts`, …) or by a marker comment near the top of the file
(`@generated`, `Code generated … DO NOT EDIT.`, `<auto-generated>`). A notice on stderr and the
preview report how many files were skipped. A file you name explicitly (`qualitycheck scan
infra/main.tf`) is always scored, and `--all-files` turns the filter off.

### 3. Git Patch Scans

```bash
# Scan uncommitted changes in the working tree
qualitycheck patch

# Scan changes relative to base branch (e.g. main)
qualitycheck patch --base main --strict --format json

# Score each changed file at the base too, and report per-file and per-metric score changes
qualitycheck patch --base main --delta

# Gate on regressions only: exit 1 if a changed file's composite drops by more than 0.5, or a
# new file scores below its profile's fail_below. Files that were already low at the base pass.
qualitycheck patch --base main --fail-on-regression 0.5

# Also fail when a single metric drops by more than 1.0, even if the composite averages it away
qualitycheck patch --base main --fail-on-regression 0.5 --fail-on-metric-regression 1.0
```

A metric's drop is scaled by how much it counts in both versions (its `inclusion`, below), so a
metric that barely applies can't fail the gate on its own. Jev's answers vary slightly between
runs, even on identical content (the cache is what makes re-runs identical): on trivial edits,
composites moved by up to 0.2 and single metrics by up to 0.4, so keep allowances above those —
0.5 for composites and 1.0 for metrics — and keep `.qualitycheck/cache` between CI runs.

With `--delta`, base versions go through the same content-addressed cache, so re-running after
further edits only re-scores what changed. `--preview` includes the base versions in its estimate.

### 4. Cross-File Context

Each file is scored on its own by default, so a thin controller that hands validation to a service
can be marked down for "poor input validation" that lives one file over. `--context` (on `scan` and
`patch`) sends Jev related files alongside each file, and every question is told to judge only the
file itself:

```bash
qualitycheck patch --base main --context
qualitycheck scan src/web/Controller.java --context --preview --full   # see what would be attached
```

Related files are picked by name matching, without model calls: test files named after the file
(`ServiceTest.java`, `service.spec.ts`, `tests/service_tests.rs`), files anywhere in the repository
whose name it mentions, and files in the change set or its directory that mention it. They must be
in the same language, are ranked by how often they're mentioned, and are capped at about 24k
characters per file. Expect roughly 2–3× the input tokens for files that get context. With `--delta`,
the base side is shown the base versions of related files. Answers with and without context are
cached separately.

### 5. Locating Findings (`--explain`)

A low score says what kind of problem was found (see `matched_rubric` below), not where. With
`--explain` (on `scan` and `patch`), each file with a metric that counts and scores below its
profile's `fail_below` gets one more request: the file is split into regions of about 40 lines,
cutting between top-level items or members where it can, and Jev is asked of each region whether it
is one of the places responsible for the finding. Regions that stand out are reported:

```
  [security] injection_risk: medium (18% conf.) [counts 64%: applies 65% · below 20% min conf.]
      ↳ Some interpreted strings are assembled by concatenating or formatting external data with partial or inconsistent escaping.
        at L125-161 (91%)
```

A region stands out when Jev judges it responsible with probability 0.5 or more and at least 0.25
above the file's median region (up to three are listed). A problem judged moderately everywhere,
like testability of a file that talks to a database throughout, is reported as `no single region
stands out`, which means it concerns the file as a whole. Both rules were calibrated on a handful
of files. The JSON carries every region's probability under `evidence.regions` and the standouts
under `evidence.hotspots`.

Region splitting is done in Rust, so Jev only ever judges regions and never makes up line numbers.
A request costs about as many input tokens as scoring the file, and answers are cached per file
content and finding, so re-runs are free until the file or its finding changes. `--preview` doesn't
include it: which metrics fail is only known after scoring. If an explain request fails, the scores
are still reported, with a notice on stderr. With `patch --delta`, only the working-tree side is
explained.

### 6. CI Integration

`--format github` (on `scan`, `patch`, and `gaps`) prints GitHub Actions annotations: one per metric
that scores below its threshold, placed on the region `--explain` located, or on the file. Findings
that fail the gate are errors (with `--fail-on-regression` / `--fail-on-metric-regression`, only
regressions; otherwise metrics of profiles below `fail_below`), and the rest are warnings, largest
shortfall first, up to the 10 per level GitHub shows per step. When `$GITHUB_STEP_SUMMARY` is set, a
Markdown summary is also appended to it: composites per file and profile (with score changes under
`patch --delta`) and every finding. `--format markdown` prints that summary alone, for a pull request
comment or another CI system. Paths are made relative to `$GITHUB_WORKSPACE`, or the current
directory.

[`examples/github-workflow.yml`](examples/github-workflow.yml) runs it on pull requests:

```yaml
- name: Score the change
  env:
    JEV_API_KEY: ${{ secrets.JEV_API_KEY }}
    BASE_REF: ${{ github.base_ref }}
  run: |
    qualitycheck patch --base "origin/$BASE_REF" --profile quality,security,qa \
      --fail-on-regression 0.5 --fail-on-metric-regression 1.0 --explain --format github
```

It checks out full history (`patch --base` needs the base branch), caches the built binary, and
keeps `.qualitycheck/cache` between runs, so unchanged content, such as the base side of each changed
file, reuses its answers instead of drawing new, slightly different ones. It skips with a notice
when the secret isn't available, as for pull requests from forks.

### 7. Triage & History

```bash
# Gaps triage: view files/metrics that failed their threshold in the latest run
qualitycheck gaps

# Detailed metric breakdown for a specific file
qualitycheck file src/auth.rs

# Compare two saved runs to audit improvements and regressions
qualitycheck diff <run-id-1> <run-id-2>

# List all saved runs
qualitycheck runs list
```

### 8. Profiles & Agent Introspection

```bash
# List available profiles (built-in and ~/.config/qualitycheck/profiles/)
qualitycheck profiles list

# Show raw JSON definition of a profile
qualitycheck profiles show security

# Machine-readable JSON manifest of commands and profiles
qualitycheck describe
```

---

## Exit Codes

| Code | Meaning |
|---|---|
| `0` | Success — informational scan, or all requested gates passed |
| `1` | Gate failure — `--strict` and a score below `fail_below`, or `--fail-on-regression` / `--fail-on-metric-regression` and a regression |
| `2` | Error — usage error, configuration/API key missing, or malformed profile |

---

## Profiles Data Model

Profiles are stored in JSON (built into the binary and copied to `~/.config/qualitycheck/profiles/`,
where you can edit them; unedited copies are refreshed when a new release improves the built-ins):

```json
{
  "$schema": "https://qualitycheck.dev/schema/profile-v1.json",
  "name": "quality",
  "description": "Overall code readability, maintainability, and structure",
  "fail_below": 3.0,
  "min_confidence": 0.2,
  "metrics": [
    {
      "id": "cohesion",
      "type": "scale",
      "range": [1, 5],
      "question": "How focused is this file on a single responsibility? ...",
      "rubric": [
        "The file mixes many unrelated responsibilities ...",
        "The file has a main purpose but also carries several unrelated concerns ...",
        "The file mostly serves one purpose, with one or two parts that would fit better elsewhere.",
        "Everything in the file serves one clear purpose, apart from at most a small helper.",
        "The file has one sharply defined responsibility and every element directly supports it."
      ],
      "applies_when": "The file contains its own logic or definitions, rather than only module declarations ...",
      "weight": 1.0
    },
    {
      "id": "complexity_level",
      "type": "enum",
      "options": ["low", "medium", "high", "critical"],
      "question": "How complex is the control flow of the code in this file?",
      "rubric": { "low": "Functions are short and straightforward ...", "medium": "...", "high": "...", "critical": "..." },
      "weight": 1.5
    }
  ]
}
```

- `rubric` (optional) describes each possible answer as a concrete situation: one entry per level
  for `scale` (lowest first), one per option for `enum`, and `"true"`/`"false"` for `binary`. Jev
  answers far more decisively against described situations than against bare labels.
- `applies_when` (optional) is asked as a separate yes/no question over the same file, so a
  controller that delegates validation isn't scored on validation it doesn't own. The metric counts
  in the composite in proportion to how likely it applies: not at all below a probability of 0.25,
  fully above 0.75, and linearly more in between. Below 0.5 it is also flagged `not_applicable`.
- `min_confidence` (optional, default `0.2`) keeps answers Jev gave with near-flat probability out
  of the composite: they count not at all below half of it, fully from it up, and linearly more in
  between, and are flagged `excluded_low_confidence` below it. If every metric of a profile is
  flagged, the profile is reported `inconclusive` and does not fail `--strict`.

  Each metric reports its P(applies) as `applicability` and the share of its weight it carried as
  `inclusion`, and output notes the reason when it counts less than 90%, e.g.
  `input_validation: fair (60% conf.) [counts 38%: applies 44%]`. The gradual ramps keep an answer
  that hovers around a threshold from jumping in and out of the composite between runs.
- `scale` ranges may span 2 to 10 levels (Jev's limit).

Each answer is scored on 1–5 as the probability-weighted mean over every possible answer: Jev's
`scale` score already is, `enum` answers average their options' scores by Jev's probability for each,
and `binary` answers score `1 + 4 × P(good_value)`. A near-tie between "good" and "fair" therefore
lands between them instead of jumping a whole option with whichever wins narrowly, which keeps
`--delta` comparisons stable. The chosen option is still reported as `raw_value`, with the
distribution under `probabilities`; `qualitycheck file` shows the likelier options, e.g.
`input_validation: good (29% conf.) -> normalized: 3.1/5 [good 46% · fair 43%]`.

Each answer also records `matched_rubric`: the rubric description of the chosen option, or of the
nearest level for `scale` metrics. `scan`, `patch`, and `gaps` print it under each metric that
counts toward the composite and scores below `fail_below`, and `file` under every metric that
counts, so a low score says what kind of problem was found:

```
  [security] injection_risk: medium (41% conf.)
      ↳ Some interpreted strings are assembled by concatenating or formatting external data with partial or inconsistent escaping.
```

Scoring policy (`weight`, `fail_below`, `min_confidence`, `good_value`, `score_map`) is not part of the
cache key: changing it re-scores cached answers without new API calls. Changing a `question`,
`rubric`, or `applies_when` re-queries.

---

## Architecture

```
qualitycheck/
├── Cargo.toml
├── profiles/          # Default profiles (qa.json, security.json, quality.json)
├── examples/          # GitHub Actions workflow
├── skills/            # Open agent skill package
├── src/
│   ├── lib.rs         # Library root
│   ├── main.rs        # Entry point and command routing
│   ├── cli.rs         # Clap command structures
│   ├── config.rs      # ~/.config/qualitycheck/config.toml and API key resolution
│   ├── profile.rs     # Profile data structures, JSON schema validation, metric hashing
│   ├── walker.rs      # Directory walking: .gitignore, source-code filter, globs, size limits
│   ├── git.rs         # Changed files and their base-revision content for patch
│   ├── context.rs     # Deterministic related-file selection for --context
│   ├── explain.rs     # Region splitting and hotspot selection for --explain
│   ├── cache.rs       # Blake3 content-addressed cache (.qualitycheck/cache/)
│   ├── jev_client.rs  # Async batch client for TypeSafe AI Jev API
│   ├── pipeline.rs    # Concurrent scan orchestration & offline preview estimation
│   ├── scorer.rs      # Normalization, inclusion-weighted composites, regression detection
│   ├── storage.rs     # Run persistence and run listing (.qualitycheck/runs/)
│   ├── output.rs      # Table/JSON display, report persistence, triage & diffing
│   ├── ci_report.rs   # GitHub Actions annotations and Markdown summary
│   └── error.rs       # Typed errors (thiserror) with exit codes
└── tests/
    ├── cache_tests.rs
    ├── cli_integration_tests.rs
    ├── context_tests.rs
    ├── git_tests.rs
    ├── profile_tests.rs
    ├── scorer_tests.rs
    ├── walker_tests.rs
    └── fixtures/      # Superseded built-in profiles, for upgrade tests
```

---

## License

This project is licensed under the [MIT License](LICENSE).