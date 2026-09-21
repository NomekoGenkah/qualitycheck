# qualitycheck

[![CI](https://github.com/NomekoGenkah/qualitycheck/actions/workflows/ci.yml/badge.svg)](https://github.com/NomekoGenkah/qualitycheck/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-2024%20%7C%202021-orange.svg)](https://www.rust-lang.org/)

A fast, deterministic Rust CLI that uses the **Jev** API (TypeSafe AI) to score code quality in a quantifiable, configurable way — think "eslint, but for qualitative criteria" (naming clarity, dead code, cyclomatic complexity, cohesion, security posture) that traditional linters cannot evaluate.

Designed to be run by humans directly, in CI pipelines, or invoked by AI coding agents (Claude Code, Codex, Cursor, Antigravity, OpenCode) as a fast, deterministic shell tool.

---

## Key Features

- **Deterministic Aggregation**: Jev returns raw typed metric values (`scale`, `enum`, `binary`) with calibrated confidence. All aggregation (weighted averages, threshold comparisons, and gating decisions) is calculated deterministically in Rust.
- **Content-Addressed Caching**: Powered by `blake3(file_content) + blake3(active_metric_definitions)`. Re-scans cost zero API calls and zero latency for unchanged files. Editing a metric automatically invalidates stale results.
- **Offline Token & Cost Estimation (`--preview`)**: Pre-calculate token usage and estimated cost before making any API calls. Fully offline, 0 network requests, and aware of cached files.
- **Token & Cost Tracking**: Live runs report exact input/output tokens and cost ($0.042 / 1M input tokens), reporting $0.00 for cache hits.
- **Git-Aware Scans (`patch`)**: Scores files modified versus the working tree or upstream base branch.
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

```bash
# Estimate token consumption and cost for a directory (0 network requests, works offline)
qualitycheck scan . --preview

# Estimate cost for git uncommitted changes
qualitycheck patch --preview

# Machine-readable preview for agents
qualitycheck scan src/ --preview --format json
```

### 2. Scanning Code

```bash
# Scan current directory with default "quality" profile
qualitycheck scan .

# Scan with multiple profiles combined
qualitycheck scan ./src --profile quality,security --format table

# Output structured JSON for automation or agents
qualitycheck scan ./src --profile quality,security --format json

# Strict gating: exit code 1 if any file falls below profile fail_below threshold
qualitycheck scan ./src --strict
```

### 3. Git Patch Scans

```bash
# Scan uncommitted changes in the working tree
qualitycheck patch

# Scan changes relative to base branch (e.g. main)
qualitycheck patch --base main --strict --format json
```

### 4. Triage & History

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

### 5. Profiles & Agent Introspection

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
| `0` | Success — informational scan, or `--strict` passed with all files meeting thresholds |
| `1` | Strict failure — `--strict` was passed and at least one score fell below `fail_below` |
| `2` | Error — usage error, configuration/API key missing, or malformed profile |

---

## Profiles Data Model

Profiles are stored in JSON (built into the binary and copied to `~/.config/qualitycheck/profiles/`):

```json
{
  "$schema": "https://qualitycheck.dev/schema/profile-v1.json",
  "name": "quality",
  "description": "Overall code readability, maintainability, and structure",
  "fail_below": 3.0,
  "metrics": [
    {
      "id": "naming_clarity",
      "type": "scale",
      "range": [1, 5],
      "question": "Do variable and function names communicate their purpose clearly?",
      "weight": 1.0
    },
    {
      "id": "has_dead_code",
      "type": "binary",
      "question": "Does the file contain dead, commented-out, or unreachable code?",
      "weight": 0.5
    },
    {
      "id": "complexity_level",
      "type": "enum",
      "options": ["low", "medium", "high", "critical"],
      "question": "What is the perceived cyclomatic complexity level of the file?",
      "weight": 1.5
    }
  ]
}
```

---

## Architecture

```
qualitycheck/
├── Cargo.toml
├── profiles/          # Default profiles (qa.json, security.json, quality.json)
├── skills/            # Open agent skill package
├── src/
│   ├── lib.rs         # Library root
│   ├── main.rs        # Entry point and command routing
│   ├── cli.rs         # Clap command structures
│   ├── config.rs      # ~/.config/qualitycheck/config.toml and API key resolution
│   ├── profile.rs     # Profile data structures, JSON schema validation, metric hashing
│   ├── walker.rs      # Directory walking respecting .gitignore, globs, and size limits
│   ├── git.rs         # Git changed files detection for patch command
│   ├── cache.rs       # Blake3 content-addressed cache (.qualitycheck/cache/)
│   ├── jev_client.rs  # Async batch client for TypeSafe AI Jev API
│   ├── pipeline.rs    # Concurrent scan orchestration & offline preview estimation
│   ├── scorer.rs      # Metric score normalization and weighted composite aggregation
│   ├── storage.rs     # Run persistence and run listing (.qualitycheck/runs/)
│   ├── output.rs      # Table/JSON display, report persistence, triage & diffing
│   └── error.rs       # Typed errors (thiserror) with exit codes
└── tests/
    ├── scorer_tests.rs
    ├── cache_tests.rs
    └── cli_integration_tests.rs
```

---

## License

This project is licensed under the [MIT License](LICENSE).