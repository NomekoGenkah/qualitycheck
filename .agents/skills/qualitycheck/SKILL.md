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
- Verifying whether a refactoring improved or degraded code quality (`qualitycheck diff`).
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

### 2. Prefer Git Patch Scans for Incremental Changes
When working in a Git repository with uncommitted or branch changes, **do not scan the entire codebase**. Use `patch`:
```bash
# Scan uncommitted working tree changes
qualitycheck patch --format json

# Scan changes relative to a base branch (e.g., main)
qualitycheck patch --base main --format json
```

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

### 5. Verify Refactoring Improvements with `diff`
When refactoring code to improve quality:
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
| `qualitycheck scan [PATH]...` | Scan one or more files or directories | `--profile <name>`, `--format <table\|json>`, `--preview`, `--strict`, `--no-ignore` |
| `qualitycheck patch` | Scan git-changed files | `--base <branch>`, `--preview`, `--strict`, `--format <table\|json>` |
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
- `1`: Strict mode failure (`--strict`) — one or more files scored below the profile's `fail_below` threshold.
- `2`: Configuration or usage error (e.g. missing API key, invalid profile).

Metrics flagged `"not_applicable": true` (the metric's `applies_when` condition doesn't hold for the
file, e.g. input validation in a file that receives no external input) or
`"excluded_low_confidence": true` (answered below the profile's `min_confidence`) do not count toward
`composite_score`; don't treat them as findings. A profile with `"inconclusive": true` had every
metric excluded and was not judged.
