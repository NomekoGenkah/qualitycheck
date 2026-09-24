# AGENTS.md — Agent Guidelines for qualitycheck

Welcome! This document provides AI coding agents (Claude Code, Cursor, OpenCode, Antigravity, Aider) with conventions, build procedures, and quality gating instructions for this repository.

---

## 1. Project Overview

`qualitycheck` is a fast, deterministic Rust CLI that uses the TypeSafe AI **Jev** API to evaluate qualitative code metrics (naming clarity, dead code, cyclomatic complexity, cohesion, security posture).

- **Source Code**: Rust (edition 2024 / 2021)
- **Primary Modules**:
  - `src/cli.rs`: Clap command definitions and flags (`scan`, `patch`, `gaps`, `file`, `diff`, `runs`, `profiles`, `init`, `describe`).
  - `src/jev_client.rs`: Async client for Jev API (`evaluate_file`).
  - `src/profile.rs`: Profile model, schema validation, metrics hash, built-in profile upgrades.
  - `src/cache.rs`: Blake3 content-addressed cache in `.qualitycheck/cache/`.
  - `src/context.rs`: Deterministic selection of related files sent as context (`--context`).
  - `src/explain.rs`: Region splitting and hotspot selection for locating failing metrics (`--explain`).
  - `src/git.rs`: Changed files and their base-revision content for `patch`.
  - `src/walker.rs`: File discovery, including the default source-code filter (`--all-files` disables it).
  - `src/pipeline.rs`: Concurrent scanning pipeline & offline token pre-estimation (`--preview`).
  - `src/scorer.rs`: Metric normalization and weighted composite aggregation.
  - `src/storage.rs`: Historical run persistence under `.qualitycheck/runs/`.
  - `src/output.rs`: Terminal table rendering & machine-readable JSON formatting.

---

## 2. Common Agent Commands

### Build & Verification
```bash
# Check code without full compilation
cargo check

# Run the test suite (all tests must pass)
cargo test

# Linter checks (zero warnings required; CI denies warnings)
cargo clippy --all-targets -- -D warnings

# Compile optimized release binary
cargo build --release
```

### Running Self-Evaluation
```bash
# Offline estimation of token cost for current working tree
qualitycheck patch --preview

# Evaluate current changes before committing: fail only on regressions this change introduces
qualitycheck patch --fail-on-regression 0.5 --fail-on-metric-regression 1.0 --format json

# Filter failing metrics only
qualitycheck gaps --format json
```

---

## 3. Conventions & Rules
- **Pure stdout for JSON**: Machine-readable output (`--format json`) must print clean JSON to stdout. Diagnostic messages and progress go to stderr.
- **Deterministic Aggregation**: Never allow external LLM responses to determine composite pass/fail decisions. All rubric conversions and composite weighted averages must occur in Rust deterministically.
- **Offline Safety**: `--preview` must remain fully offline (no API key required, 0 network requests).
- **Built-in Profile Changes**: `init` copies `profiles/*.json` into the user's config dir, where copies take precedence. When changing a built-in, add the old version's hash to `SUPERSEDED_BUILTIN_HASHES` (`src/profile.rs`) so unedited copies keep upgrading; `tests/profile_tests.rs` pins the current hashes and fails until you do.
