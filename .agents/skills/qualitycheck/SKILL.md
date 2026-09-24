---
name: qualitycheck
description: >-
  Evaluate and audit qualitative code quality (naming clarity, cyclomatic complexity, dead code, cohesion,
  security posture) using the qualitycheck CLI. Use when the user asks to review, audit, inspect
  code quality, evaluate git diffs/PRs for regressions, or triage quality gaps.
---

# qualitycheck Skill

Use the `qualitycheck` CLI tool to evaluate qualitative code metrics deterministically. It combines structured LLM evaluations with local deterministic scoring, content-addressed caching, git-aware patch scanning, and regression diffing.

## When to Use

Activate this skill when:
- The user requests a code review, quality audit, or sanity check on files or directories.
- Evaluating changes before a commit or PR (`qualitycheck patch`).
- Verifying whether a change or refactoring improved or degraded code quality (`qualitycheck patch --delta`, or `qualitycheck diff` between saved runs).
- Triaging quality regressions or identifying failing metrics (`qualitycheck gaps`).

---

## Agent Workflow & Best Practices

As an agent, follow these efficiency rules to optimize token usage and context window consumption:

### 1. Pre-Scan Cost Estimation (`--preview` / `--dry-run`)
Always run `--preview` before executing large scans to check which files are cached and estimate token consumption:
```bash
qualitycheck scan <path> --preview
```
- **Context-safe by default**: Outputs a compact summary and top token-consuming files to avoid polluting the LLM context window.
- If you explicitly need the full file-by-file list, add `--full` (or `--verbose`).
- Cache hits consume **0 tokens** ($0.00).
- Uncached files display an estimated token count and projected USD cost.
- `--preview` is completely offline and does not require an API key or make network requests.
- Directory scans and `patch` score only source code: docs, config, lockfiles, vendored, minified,
  and generated files are skipped (the count is reported as `skipped_non_source_files` and on
  stderr). Name a file explicitly, or pass `--all-files`, to score other files.

### 2. Prefer Git Patch Scans for Incremental Changes
When working in a Git repository with uncommitted or branch changes, **do not scan the entire codebase**. Use `patch`:
```bash
# Scan uncommitted working tree changes
qualitycheck patch --format json

# Scan changes relative to a base branch (e.g., main)
qualitycheck patch --base main --format json
```

To tell whether a low score was introduced by the change or predates it, don't scan the base
revision by hand: `--delta` scores each changed file at the base as well and reports score changes,
and `--fail-on-regression <POINTS>` exits 1 only for drops larger than POINTS or poor new files;
`--fail-on-metric-regression <POINTS>` also fails on a single metric dropping by more than POINTS
(reported under `metric_regressions`), which a composite can average away:
```bash
qualitycheck patch --base main --fail-on-regression 0.5 --fail-on-metric-regression 1.0 --format json
```
Scores vary by up to ~0.2 (composite) and ~0.4 (single metric) between runs on unchanged code, so
don't treat smaller moves as signal.
If a file is marked down for work it delegates (validation done by a service it calls, logic
tested in another file), re-run with `--context`: related files are sent alongside each file and
listed under `context_files` in the output.

In the JSON report, `regressions` lists what the change made worse (`kind`: `dropped` or
`below_threshold_without_base`); `file_diffs` has composite and metric deltas per changed file
(`old_composite` is `null` for files without a base version).

Metric scores are probability-weighted, so an uncertain answer that flips its top option between
near-identical versions moves the score only slightly. Composite changes of 0.1–0.2 on files the
change barely touched are still normal model variation: use an allowance of about `0.5`, and look at
the metric deltas in `file_diffs` before treating a small drop as real.

`--context` costs roughly 2–3× the input tokens for files that get related files; check what would
be attached with `--context --preview --full` first.

### 3. Always Use Structured JSON for Parsing
Always pass `--format json` so the output can be parsed directly without ANSI escape sequences:
```bash
qualitycheck scan <path> --format json
```
All diagnostic logs and progress bars are emitted to `stderr`, leaving `stdout` clean for JSON parsing.

### 4. Compact Triage with `gaps`
Avoid reading huge multi-file scan reports into context. Use `qualitycheck gaps` to only retrieve files and metrics that failed the threshold:
```bash
# Triage failing metrics from the latest run
qualitycheck gaps --format json

# Triage failing metrics from a specific historical run
qualitycheck gaps --run <run-id> --format json
```

### 5. Verify Refactoring Improvements
For uncommitted or branch changes, `qualitycheck patch --delta --format json` compares every
changed file against its base version in one run. To compare arbitrary points in time instead:
1. Run a scan before making edits and record the run ID:
   ```bash
   qualitycheck scan <file-path> --format json
   ```
2. Perform the refactor.
3. Run a scan after edits:
   ```bash
   qualitycheck scan <file-path> --format json
   ```
4. Compare both runs to verify score improvements:
   ```bash
   qualitycheck diff <old-run-id> <new-run-id>
   ```
   Positive deltas (`+0.5`) confirm quality improvements; negative deltas highlight regressions.

---

## Command Reference

| Command | Purpose | Key Flags |
|---|---|---|
| `qualitycheck scan [PATH]...` | Scan one or more files or directories | `--context`, `--all-files`, `--profile <name>`, `--format <table\|json>`, `--preview`, `--strict`, `--no-ignore` |
| `qualitycheck patch` | Scan git-changed files | `--base <branch>`, `--delta`, `--fail-on-regression <POINTS>`, `--context`, `--all-files`, `--preview`, `--strict`, `--format <table\|json>` |
| `qualitycheck gaps` | Filter latest or specified run for failing metrics | `--run <run-id>`, `--format <table\|json>` |
| `qualitycheck file <PATH>` | Detailed metric breakdown for a single file | `--run <run-id>`, `--format <table\|json>` |
| `qualitycheck diff <RUN-A> <RUN-B>` | Compare two historical runs for score deltas | `--format <table\|json>` |
| `qualitycheck runs list` | List all historical runs stored in `.qualitycheck/runs/` | |
| `qualitycheck profiles list` | List available built-in and custom profiles | |
| `qualitycheck profiles show <NAME>` | View full JSON schema and metrics of a profile | |
| `qualitycheck describe` | Machine-readable manifest of all commands and profiles | |

---

## Profiles

Built-in profiles:
- **`quality`** (default): Evaluates `naming_clarity`, `has_dead_code`, `complexity_level`, and `cohesion`. Default pass threshold: **3.0 / 5.0**.
- **`security`**: Evaluates `hardcoded_secrets`, `input_validation`, `injection_risk`, and `safe_error_handling`. Default pass threshold: **3.5 / 5.0**.
- **`qa`**: Evaluates `testability`, `edge_case_handling`, `has_unhandled_errors`, and `defensive_coding`. Default pass threshold: **3.0 / 5.0**.

Combine profiles with commas:
```bash
qualitycheck scan src/ --profile quality,security --format json
```

---

## Exit Codes

- `0`: Scan succeeded and all files met thresholds (or running in informational mode without `--strict`).
- `1`: Gate failure — with `--strict`, a file scored below its profile's `fail_below`; with `--fail-on-regression`, the change introduced a regression.
- `2`: Configuration or usage error (e.g. missing API key, invalid profile).

`normalized_score` is the probability-weighted score; `raw_value` is only Jev's top pick, and for
enum metrics `probabilities` shows how close the alternatives were. Judge by `normalized_score`.
`matched_rubric` is the profile's description of the situation Jev's answer corresponds to (the
nearest level for scale metrics): it names the kind of problem, not its location, so read the file
to find where it occurs.

Metrics flagged `"not_applicable": true` (the metric's `applies_when` condition likely doesn't hold
for the file, e.g. input validation in a file that receives no external input) or
`"excluded_low_confidence": true` (answered below the profile's `min_confidence`) count little or
nothing toward `composite_score`; don't treat them as findings. `inclusion` (0–1) is the share of its
weight each metric carried. A profile with `"inconclusive": true` had every metric flagged and was
not judged.
