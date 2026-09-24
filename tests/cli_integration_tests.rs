use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn test_cli_describe() {
    let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
    let assert = cmd.arg("describe").assert();

    assert
        .success()
        .stdout(predicate::str::contains("\"commands\""))
        .stdout(predicate::str::contains("\"profiles\""))
        .stdout(predicate::str::contains("\"quality\""))
        .stdout(predicate::str::contains("\"security\""))
        .stdout(predicate::str::contains("\"qa\""));
}

#[test]
fn test_cli_profiles_list_and_show() {
    let mut cmd_list = Command::cargo_bin("qualitycheck").unwrap();
    cmd_list
        .arg("profiles")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("quality"))
        .stdout(predicate::str::contains("security"))
        .stdout(predicate::str::contains("qa"));

    let mut cmd_show = Command::cargo_bin("qualitycheck").unwrap();
    cmd_show
        .arg("profiles")
        .arg("show")
        .arg("quality")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"quality\""))
        .stdout(predicate::str::contains("naming_clarity"));
}

#[test]
fn test_cli_init_non_interactive_success() {
    let tmp_config = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
    cmd.env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .arg("init")
        .arg("--api-key")
        .arg("jev_secret_test_123")
        .arg("--non-interactive")
        .assert()
        .success()
        .stdout(predicate::str::contains("Config saved"))
        .stdout(predicate::str::contains("Default profiles installed"));

    let config_file = tmp_config.path().join("config.toml");
    assert!(config_file.exists());
    let content = fs::read_to_string(config_file).unwrap();
    assert!(content.contains("jev_secret_test_123"));

    let profiles_dir = tmp_config.path().join("profiles");
    assert!(profiles_dir.join("quality.json").exists());
    assert!(profiles_dir.join("security.json").exists());
    assert!(profiles_dir.join("qa.json").exists());
}

#[test]
fn test_cli_init_non_interactive_failure_without_key() {
    let tmp_config = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
    cmd.env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env_remove("JEV_API_KEY")
        .arg("init")
        .arg("--non-interactive")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("No Jev API key found"));
}

#[test]
fn test_cli_scan_missing_key_fails_fast() {
    let tmp_config = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
    cmd.env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env_remove("JEV_API_KEY")
        .arg("scan")
        .arg(".")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("No Jev API key found"));
}

#[test]
fn test_cli_scan_with_mock_jev_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mock_url = format!("http://127.0.0.1:{}/v1/systemone", port);

    // Spawn a mock Jev server thread
    let server_handle = thread::spawn(move || {
        for _ in 0..10 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);

                // Jev scores are 0-based positions on the criteria levels: 3.0 on a
                // 1..5 scale is level "4", 3.5 is 4.5.
                let mock_body = serde_json::json!({
                    "answers": {
                        "naming_clarity": { "score": 3.0, "confidence": 0.87 },
                        "has_dead_code": { "noul": 0.05 },
                        "complexity_level": { "choice": "low", "confidence": 0.95 },
                        "cohesion": { "score": 3.5, "confidence": 0.90 },
                        "naming_clarity:applies": { "noul": 0.97 },
                        "cohesion:applies": { "noul": 0.97 }
                    }
                })
                .to_string();

                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    mock_body.len(),
                    mock_body
                );
                let _ = stream.write_all(http_response.as_bytes());
            }
        }
    });

    let tmp_repo = tempdir().unwrap();
    let test_file = tmp_repo.path().join("auth.rs");
    fs::write(&test_file, "pub fn authenticate() -> bool { true }").unwrap();

    let tmp_config = tempdir().unwrap();

    // 1. Run scan with json format
    let mut scan_cmd = Command::cargo_bin("qualitycheck").unwrap();
    let assert = scan_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &mock_url)
        .arg("scan")
        .arg(".")
        .arg("--format")
        .arg("json")
        .assert();

    assert
        .success()
        .stdout(predicate::str::contains("\"naming_clarity\""))
        .stdout(predicate::str::contains("\"composite_score\": 4.6"))
        .stdout(predicate::str::contains("\"passed\": true"));

    // 2. Verify run saved to .qualitycheck/runs/
    let runs_dir = tmp_repo.path().join(".qualitycheck").join("runs");
    assert!(runs_dir.exists());
    assert!(runs_dir.join("latest.json").exists());

    // 3. Run gaps command (should report all passed)
    let mut gaps_cmd = Command::cargo_bin("qualitycheck").unwrap();
    gaps_cmd
        .current_dir(tmp_repo.path())
        .arg("gaps")
        .assert()
        .success()
        .stdout(predicate::str::contains("All metrics passed"));

    // 4. Run file command
    let mut file_cmd = Command::cargo_bin("qualitycheck").unwrap();
    file_cmd
        .current_dir(tmp_repo.path())
        .arg("file")
        .arg("auth.rs")
        .assert()
        .success()
        .stdout(predicate::str::contains("Composite score: 4.6/5 [PASS]"))
        // Score position 3.0 is level 4 of naming_clarity's rubric.
        .stdout(predicate::str::contains(
            "↳ Nearly every name describes its purpose clearly, with only a few vague or inconsistent spots.",
        ));

    // 5. Run table format scan to verify table output
    let mut table_cmd = Command::cargo_bin("qualitycheck").unwrap();
    table_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &mock_url)
        .arg("scan")
        .arg(".")
        .arg("--no-color")
        .assert()
        .success()
        .stdout(predicate::str::contains("naming_clarity: 4/5 (87% conf.)"))
        .stdout(predicate::str::contains("Composite score: quality 4.6/5"))
        // Rubric situations are shown only for metrics below the threshold.
        .stdout(predicate::str::contains("↳").not());

    // 6. Test repeated scan hits cache
    let mut cache_cmd = Command::cargo_bin("qualitycheck").unwrap();
    cache_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &mock_url)
        .arg("scan")
        .arg(".")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"cached_files\": 1"))
        .stdout(predicate::str::contains("\"served_from_cache\": true"));

    // 7. Test runs list command
    let mut runs_cmd = Command::cargo_bin("qualitycheck").unwrap();
    runs_cmd
        .current_dir(tmp_repo.path())
        .arg("runs")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("RUN ID"))
        .stdout(predicate::str::contains("PASS"));

    drop(server_handle);
}

#[test]
fn test_cli_scan_strict_mode_exit_code() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mock_url = format!("http://127.0.0.1:{}/v1/systemone", port);

    // Mock server returns low scores below fail_below (3.0)
    let server_handle = thread::spawn(move || {
        for _ in 0..5 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);

                let mock_body = serde_json::json!({
                    "answers": {
                        "naming_clarity": { "score": 0.0, "confidence": 0.95 },
                        "has_dead_code": { "noul": 0.95 }, // true -> dead code present -> score 1.0
                        "complexity_level": { "choice": "critical", "confidence": 0.95 }, // critical -> score 1.0
                        "cohesion": { "score": 0.0, "confidence": 0.95 },
                        "naming_clarity:applies": { "noul": 0.97 },
                        "cohesion:applies": { "noul": 0.97 }
                    }
                })
                .to_string();

                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    mock_body.len(),
                    mock_body
                );
                let _ = stream.write_all(http_response.as_bytes());
            }
        }
    });

    let tmp_repo = tempdir().unwrap();
    let test_file = tmp_repo.path().join("poor_code.rs");
    fs::write(&test_file, "bad code").unwrap();

    let tmp_config = tempdir().unwrap();

    // With --strict, it must exit code 1
    let mut strict_cmd = Command::cargo_bin("qualitycheck").unwrap();
    strict_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &mock_url)
        .arg("scan")
        .arg(".")
        .arg("--strict")
        .assert()
        .code(1);

    // Without --strict, it must exit code 0 (informational)
    let mut info_cmd = Command::cargo_bin("qualitycheck").unwrap();
    info_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &mock_url)
        .arg("scan")
        .arg(".")
        .assert()
        .success();

    // gaps says in words what each failing answer means
    let mut gaps_cmd = Command::cargo_bin("qualitycheck").unwrap();
    gaps_cmd
        .current_dir(tmp_repo.path())
        .arg("gaps")
        .assert()
        .success()
        .stdout(predicate::str::contains("complexity_level: critical"))
        .stdout(predicate::str::contains("↳ Control flow is tangled"))
        .stdout(predicate::str::contains("↳ The file contains commented-out code blocks"));

    drop(server_handle);
}

#[test]
fn test_cli_diff_runs() {
    let tmp_repo = tempdir().unwrap();
    let runs_dir = tmp_repo.path().join(".qualitycheck").join("runs");
    fs::create_dir_all(&runs_dir).unwrap();

    let run_a_json = serde_json::json!({
        "run_id": "run-1",
        "timestamp": "2026-09-21T10:00:00Z",
        "target_path": ".",
        "total_files": 1,
        "cached_files": 0,
        "profiles_used": ["quality"],
        "files": [{
            "path": "test.rs",
            "relative_path": "test.rs",
            "served_from_cache": false,
            "profiles": [{
                "profile_name": "quality",
                "composite_score": 3.5,
                "fail_below": 3.0,
                "passed": true,
                "metrics": [{
                    "metric_id": "naming_clarity",
                    "metric_type": "scale",
                    "question": "Q",
                    "weight": 1.0,
                    "raw_value": 3.0,
                    "confidence": 0.8,
                    "normalized_score": 3.0
                }]
            }]
        }],
        "passed": true,
        "exit_reason": null
    });

    let run_b_json = serde_json::json!({
        "run_id": "run-2",
        "timestamp": "2026-09-21T11:00:00Z",
        "target_path": ".",
        "total_files": 1,
        "cached_files": 0,
        "profiles_used": ["quality"],
        "files": [{
            "path": "test.rs",
            "relative_path": "test.rs",
            "served_from_cache": false,
            "profiles": [{
                "profile_name": "quality",
                "composite_score": 4.5,
                "fail_below": 3.0,
                "passed": true,
                "metrics": [{
                    "metric_id": "naming_clarity",
                    "metric_type": "scale",
                    "question": "Q",
                    "weight": 1.0,
                    "raw_value": 4.5,
                    "confidence": 0.9,
                    "normalized_score": 4.5
                }]
            }]
        }],
        "passed": true,
        "exit_reason": null
    });

    fs::write(runs_dir.join("run-1.json"), serde_json::to_string(&run_a_json).unwrap()).unwrap();
    fs::write(runs_dir.join("run-2.json"), serde_json::to_string(&run_b_json).unwrap()).unwrap();

    let mut diff_cmd = Command::cargo_bin("qualitycheck").unwrap();
    diff_cmd
        .current_dir(tmp_repo.path())
        .arg("diff")
        .arg("run-1")
        .arg("run-2")
        .arg("--no-color")
        .assert()
        .success()
        .stdout(predicate::str::contains("Comparing runs: run-1 -> run-2"))
        .stdout(predicate::str::contains("test.rs"))
        .stdout(predicate::str::contains("composite: 3.5 -> 4.5 (+1.0)"));
}

#[test]
fn test_cli_patch_command() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mock_url = format!("http://127.0.0.1:{}/v1/systemone", port);

    let server_handle = thread::spawn(move || {
        for _ in 0..5 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);

                let mock_body = serde_json::json!({
                    "answers": {
                        "naming_clarity": { "score": 3.5, "confidence": 0.9 },
                        "has_dead_code": { "noul": 0.01 },
                        "complexity_level": { "choice": "low", "confidence": 0.95 },
                        "cohesion": { "score": 3.5, "confidence": 0.9 },
                        "naming_clarity:applies": { "noul": 0.97 },
                        "cohesion:applies": { "noul": 0.97 }
                    }
                })
                .to_string();

                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    mock_body.len(),
                    mock_body
                );
                let _ = stream.write_all(http_response.as_bytes());
            }
        }
    });

    let tmp_repo = tempdir().unwrap();
    // Initialize git repository
    let repo = git2::Repository::init(tmp_repo.path()).unwrap();
    let test_file = tmp_repo.path().join("modified.rs");
    fs::write(&test_file, "pub fn new_code() {}").unwrap();

    let tmp_config = tempdir().unwrap();

    let mut patch_cmd = Command::cargo_bin("qualitycheck").unwrap();
    patch_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &mock_url)
        .arg("patch")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("modified.rs"))
        .stdout(predicate::str::contains("\"total_files\": 1"));

    // The run report and cache entry just written must be invisible to git, even though the
    // repo has no .gitignore of its own.
    let mut status_opts = git2::StatusOptions::new();
    status_opts.include_untracked(true).recurse_untracked_dirs(true);
    let leaked: Vec<String> = repo
        .statuses(Some(&mut status_opts))
        .unwrap()
        .iter()
        .filter_map(|s| s.path().ok().map(str::to_string))
        .filter(|p| p.starts_with(".qualitycheck"))
        .collect();
    assert!(leaked.is_empty(), "qualitycheck output visible to git: {leaked:?}");

    drop(repo);
    drop(server_handle);
}

#[test]
fn test_cli_scan_preview_offline_without_api_key() {
    let tmp_repo = tempdir().unwrap();
    let test_file = tmp_repo.path().join("main.rs");
    fs::write(&test_file, "fn main() { println!(\"hello\"); }").unwrap();

    let tmp_config = tempdir().unwrap();

    let mut preview_cmd = Command::cargo_bin("qualitycheck").unwrap();
    preview_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env_remove("JEV_API_KEY")
        .arg("scan")
        .arg(".")
        .arg("--preview")
        .arg("--no-color")
        .assert()
        .success()
        .stdout(predicate::str::contains("Scan Preview:"))
        .stdout(predicate::str::contains("main.rs"))
        .stdout(predicate::str::contains("[UNCACHED]"))
        .stdout(predicate::str::contains("Est. tokens:"))
        .stdout(predicate::str::contains("Preview mode: No API calls made"));
}

#[test]
fn test_cli_scan_preview_json_format() {
    let tmp_repo = tempdir().unwrap();
    let test_file = tmp_repo.path().join("lib.rs");
    fs::write(&test_file, "pub fn add(a: i32, b: i32) -> i32 { a + b }").unwrap();

    let tmp_config = tempdir().unwrap();

    let mut preview_cmd = Command::cargo_bin("qualitycheck").unwrap();
    preview_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env_remove("JEV_API_KEY")
        .arg("scan")
        .arg(".")
        .arg("--dry-run")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_estimated_tokens\""))
        .stdout(predicate::str::contains("\"uncached_files\": 1"))
        .stdout(predicate::str::contains("\"served_from_cache\": false"));
}

#[test]
fn test_cli_scan_preview_summary_vs_full() {
    let tmp_repo = tempdir().unwrap();
    // Create 7 files (> 5 threshold)
    for i in 1..=7 {
        fs::write(
            tmp_repo.path().join(format!("file_{}.rs", i)),
            format!("pub fn func_{}() {{}}", i),
        )
        .unwrap();
    }

    let tmp_config = tempdir().unwrap();

    // 1. Without --full: JSON output should omit "files" array and have "top_uncached"
    let mut summary_cmd = Command::cargo_bin("qualitycheck").unwrap();
    summary_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env_remove("JEV_API_KEY")
        .arg("scan")
        .arg(".")
        .arg("--preview")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_files\": 7"))
        .stdout(predicate::str::contains("\"is_full\": false"))
        .stdout(predicate::str::contains("\"top_uncached\""))
        .stdout(predicate::str::contains("\"files\"").not());

    // 2. With --full: JSON output should include "files" array with all 7 files
    let mut full_cmd = Command::cargo_bin("qualitycheck").unwrap();
    full_cmd
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env_remove("JEV_API_KEY")
        .arg("scan")
        .arg(".")
        .arg("--preview")
        .arg("--full")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_files\": 7"))
        .stdout(predicate::str::contains("\"is_full\": true"))
        .stdout(predicate::str::contains("\"files\""));
}

fn commit_all(repo: &git2::Repository, message: &str) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap();
}

#[test]
fn test_cli_patch_skips_own_output_binaries_and_excludes() {
    let tmp_repo = tempdir().unwrap();
    let root = tmp_repo.path();
    let repo = git2::Repository::init(root).unwrap();
    fs::write(root.join("base.rs"), "pub fn base() {}").unwrap();
    commit_all(&repo, "base");

    // The real change.
    fs::write(root.join("modified.rs"), "pub fn new_code() {}").unwrap();
    // Reports left by an older qualitycheck version, with no self-ignoring .gitignore and no
    // `.qualitycheck/` entry in the repo's .gitignore: git sees them as untracked.
    let runs_dir = root.join(".qualitycheck").join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    fs::write(runs_dir.join("old-run.json"), "{\"run_id\": \"old\"}").unwrap();
    // A binary asset and a file the user excludes explicitly.
    fs::write(root.join("logo.png"), [0x89u8, b'P', b'N', b'G', 0, 0, 1, 2]).unwrap();
    fs::create_dir_all(root.join("vendor")).unwrap();
    fs::write(root.join("vendor").join("gen.rs"), "pub fn generated() {}").unwrap();

    let tmp_config = tempdir().unwrap();

    Command::cargo_bin("qualitycheck")
        .unwrap()
        .current_dir(root)
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env_remove("JEV_API_KEY")
        .arg("patch")
        .arg("--base")
        .arg("HEAD")
        .arg("--exclude")
        .arg("vendor/**")
        .arg("--preview")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_files\": 1"))
        .stdout(predicate::str::contains("modified.rs"))
        .stdout(predicate::str::contains("old-run.json").not())
        .stdout(predicate::str::contains("logo.png").not())
        .stdout(predicate::str::contains("gen.rs").not());
}

#[test]
fn test_cli_scan_accepts_multiple_paths() {
    let tmp_repo = tempdir().unwrap();
    let root = tmp_repo.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("new.rs"), "pub fn new_impl() {}").unwrap();
    fs::write(root.join("src").join("old.rs"), "pub fn old_impl() {}").unwrap();
    fs::write(root.join("src").join("other.rs"), "pub fn other() {}").unwrap();
    let tmp_config = tempdir().unwrap();

    let preview = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
        cmd.current_dir(root)
            .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
            .env_remove("JEV_API_KEY")
            .args(args)
            .args(["--preview", "--format", "json"]);
        cmd.assert()
    };

    // Two specific files, via the subcommand and via the implicit default command.
    for args in [
        &["scan", "src/new.rs", "src/old.rs"][..],
        &["src/new.rs", "src/old.rs"][..],
    ] {
        preview(args)
            .success()
            .stdout(predicate::str::contains("\"total_files\": 2"))
            .stdout(predicate::str::contains("src/new.rs"))
            .stdout(predicate::str::contains("src/old.rs"))
            .stdout(predicate::str::contains("other.rs").not());
    }

    // Overlapping targets scan each file once.
    preview(&["scan", "src", "src/new.rs", "./src/new.rs"])
        .success()
        .stdout(predicate::str::contains("\"total_files\": 3"));

    // Any missing target is an error, not silently skipped.
    preview(&["scan", "src/new.rs", "src/missing.rs"])
        .failure()
        .stderr(predicate::str::contains("src/missing.rs"));
}

/// Serves `body` as the Jev response for up to `max_requests` requests; returns the endpoint URL.
fn spawn_mock_jev(body: serde_json::Value, max_requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/v1/systemone", listener.local_addr().unwrap().port());
    let body = body.to_string();
    thread::spawn(move || {
        for _ in 0..max_requests {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(http_response.as_bytes());
            }
        }
    });
    url
}

#[test]
fn test_cli_scan_excludes_low_confidence_metrics() {
    // cohesion at the lowest level but 5% confidence: counted, it would drag 4.0 down to 3.3.
    let mock_url = spawn_mock_jev(
        serde_json::json!({
            "answers": {
                "naming_clarity": { "score": 3.0, "confidence": 0.87 },
                "has_dead_code": { "noul": 0.05 },
                "complexity_level": { "choice": "medium", "confidence": 0.9 },
                "cohesion": { "score": 0.0, "confidence": 0.05 },
                "naming_clarity:applies": { "noul": 0.97 },
                "cohesion:applies": { "noul": 0.97 }
            }
        }),
        5,
    );

    let tmp_repo = tempdir().unwrap();
    fs::write(tmp_repo.path().join("controller.rs"), "pub fn handle() {}").unwrap();
    let tmp_config = tempdir().unwrap();

    let scan = |extra: &[&str]| {
        let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
        cmd.current_dir(tmp_repo.path())
            .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
            .env("JEV_API_KEY", "test_key")
            .env("JEV_API_URL", &mock_url)
            .args(["scan", ".", "--strict"])
            .args(extra);
        cmd.assert()
    };

    // (4*1 + 5*0.5 + 3.67*1.5) / 3 = 4.0 without cohesion.
    scan(&["--format", "json"])
        .success()
        .stdout(predicate::str::contains("\"composite_score\": 4.0"))
        .stdout(predicate::str::contains("\"excluded_low_confidence\": true"))
        .stdout(predicate::str::contains("\"inconclusive\": false"));

    scan(&["--no-color"])
        .success()
        .stdout(predicate::str::contains(
            "cohesion: 1/5 (5% conf.) [excluded: below 20% min conf.]",
        ))
        .stdout(predicate::str::contains("has_dead_code: false (90% conf.)"));
}

#[test]
fn test_cli_unedited_installed_profile_is_upgraded_but_edited_one_is_kept() {
    let tmp_config = tempdir().unwrap();
    let profiles_dir = tmp_config.path().join("profiles");
    fs::create_dir_all(&profiles_dir).unwrap();
    // Copies installed by v0.1.0's `init`: one untouched, one edited by the user.
    fs::write(
        profiles_dir.join("quality.json"),
        include_str!("fixtures/superseded_profiles/quality-0.1.0.json"),
    )
    .unwrap();
    let edited_security = include_str!("fixtures/superseded_profiles/security-0.1.0.json")
        .replace("\"fail_below\": 3.5", "\"fail_below\": 4.2");
    fs::write(profiles_dir.join("security.json"), &edited_security).unwrap();

    let run = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
        cmd.env("QUALITYCHECK_CONFIG_DIR", tmp_config.path()).args(args);
        cmd.assert()
    };

    // The stale copy no longer shadows the built-in, even before anything rewrites it.
    run(&["profiles", "show", "quality"])
        .success()
        .stdout(predicate::str::contains("\"rubric\""));
    run(&["profiles", "show", "security"])
        .success()
        .stdout(predicate::str::contains("4.2"))
        .stdout(predicate::str::contains("\"rubric\"").not());

    // Installing defaults refreshes the stale copy on disk and leaves the edited one alone.
    run(&["init", "--api-key", "k", "--non-interactive"]).success();
    let quality_on_disk = fs::read_to_string(profiles_dir.join("quality.json")).unwrap();
    assert!(quality_on_disk.contains("\"rubric\""));
    assert_eq!(
        fs::read_to_string(profiles_dir.join("security.json")).unwrap(),
        edited_security
    );

    // An installed copy identical to the built-in is not reported as a custom profile.
    let listing = run(&["profiles", "list"]).success();
    let listing = String::from_utf8_lossy(&listing.get_output().stdout).to_string();
    let source_of = |name: &str| {
        listing
            .lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(3))
            .map(str::to_string)
    };
    assert_eq!(source_of("quality").as_deref(), Some("builtin"));
    assert_eq!(source_of("security").as_deref(), Some("custom"));
}

#[test]
fn test_cli_scan_excludes_not_applicable_metrics() {
    // A module-declaration file: Jev judges naming and cohesion not applicable.
    let mock_url = spawn_mock_jev(
        serde_json::json!({
            "answers": {
                "naming_clarity": { "score": 0.0, "confidence": 0.9 },
                "naming_clarity:applies": { "noul": 0.04 },
                "has_dead_code": { "noul": 0.02 },
                "complexity_level": { "choice": "low", "confidence": 0.95 },
                "cohesion": { "score": 0.0, "confidence": 0.9 },
                "cohesion:applies": { "noul": 0.06 }
            }
        }),
        5,
    );

    let tmp_repo = tempdir().unwrap();
    fs::write(tmp_repo.path().join("lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    let tmp_config = tempdir().unwrap();

    Command::cargo_bin("qualitycheck")
        .unwrap()
        .current_dir(tmp_repo.path())
        .env("QUALITYCHECK_CONFIG_DIR", tmp_config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &mock_url)
        .args(["scan", ".", "--strict", "--no-color"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "naming_clarity: 1/5 (90% conf.) [excluded: applies 4%]",
        ))
        .stdout(predicate::str::contains("Composite score: quality 5.0/5"));
}

/// Serves Jev responses chosen from each request body.
fn spawn_content_aware_mock_jev(respond: fn(&serde_json::Value) -> serde_json::Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/v1/systemone", listener.local_addr().unwrap().port());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buffer = [0u8; 8192];
            let body_start = loop {
                let n = stream.read(&mut buffer).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                request.extend_from_slice(&buffer[..n]);
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(pos + 4);
                }
            };
            let Some(body_start) = body_start else { continue };
            let headers = String::from_utf8_lossy(&request[..body_start]).to_lowercase();
            let content_length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while request.len() < body_start + content_length {
                let n = stream.read(&mut buffer).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..n]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&request[body_start..]).unwrap_or_default();
            let response = respond(&body).to_string();
            let http_response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            let _ = stream.write_all(http_response.as_bytes());
        }
    });
    url
}

/// Files containing "HIGH_QUALITY" score 4.7/5 on the quality profile; anything else 1.0/5.
fn quality_by_marker(body: &serde_json::Value) -> serde_json::Value {
    quality_answers(body["state"].to_string().contains("HIGH_QUALITY"))
}

/// Quality-profile answers worth 4.7/5 when `high`, 1.0/5 otherwise.
fn quality_answers(high: bool) -> serde_json::Value {
    if high {
        serde_json::json!({ "answers": {
            "naming_clarity": { "score": 3.5, "confidence": 0.9 },
            "naming_clarity:applies": { "noul": 0.97 },
            "has_dead_code": { "noul": 0.02 },
            "complexity_level": { "choice": "low", "confidence": 0.95 },
            "cohesion": { "score": 3.5, "confidence": 0.9 },
            "cohesion:applies": { "noul": 0.97 }
        }})
    } else {
        serde_json::json!({ "answers": {
            "naming_clarity": { "score": 0.0, "confidence": 0.9 },
            "naming_clarity:applies": { "noul": 0.97 },
            "has_dead_code": { "noul": 0.97 },
            "complexity_level": { "choice": "critical", "confidence": 0.95 },
            "cohesion": { "score": 0.0, "confidence": 0.9 },
            "cohesion:applies": { "noul": 0.97 }
        }})
    }
}

/// A repo committed with a good `controller.rs` and an already-poor `legacy.rs`.
fn repo_with_quality_history() -> (tempfile::TempDir, git2::Repository) {
    let tmp = tempdir().unwrap();
    let repo = git2::Repository::init(tmp.path()).unwrap();
    fs::write(tmp.path().join("controller.rs"), "// HIGH_QUALITY v1\npub fn handle() {}\n").unwrap();
    fs::write(tmp.path().join("legacy.rs"), "// v1\npub fn x() {}\n").unwrap();
    commit_all(&repo, "base");
    (tmp, repo)
}

fn patch_cmd(root: &std::path::Path, config: &std::path::Path, url: &str, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
    cmd.current_dir(root)
        .env("QUALITYCHECK_CONFIG_DIR", config)
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", url)
        .arg("patch")
        .args(args);
    cmd.assert()
}

#[test]
fn test_cli_patch_fail_on_regression_ignores_preexisting_low_scores() {
    let url = spawn_content_aware_mock_jev(quality_by_marker);
    let (tmp, _repo) = repo_with_quality_history();
    let root = tmp.path();
    let config = tempdir().unwrap();

    // Controller stays good and legacy stays poor: the low score predates the change.
    fs::write(root.join("controller.rs"), "// HIGH_QUALITY v2\npub fn handle() {}\n").unwrap();
    fs::write(root.join("legacy.rs"), "// v2\npub fn x() {}\n").unwrap();

    patch_cmd(root, config.path(), &url, &["--base", "HEAD", "--fail-on-regression", "0.5", "--no-color"])
        .success()
        .stdout(predicate::str::contains("Score changes vs HEAD (2 changed files, 0 without a base version)"))
        .stdout(predicate::str::contains("[quality] composite: 1.0 -> 1.0 (0.0)"))
        .stdout(predicate::str::contains("No regressions (allowed drop: 0.5)."));

    // --strict alone would have failed on legacy.rs.
    patch_cmd(root, config.path(), &url, &["--base", "HEAD", "--strict"]).code(1);
}

#[test]
fn test_cli_patch_fail_on_regression_flags_drops_and_poor_new_files() {
    let url = spawn_content_aware_mock_jev(quality_by_marker);
    let (tmp, _repo) = repo_with_quality_history();
    let root = tmp.path();
    let config = tempdir().unwrap();

    fs::write(root.join("controller.rs"), "// v2, rewritten badly\npub fn handle() {}\n").unwrap();
    fs::write(root.join("legacy.rs"), "// v2\npub fn x() {}\n").unwrap();
    fs::write(root.join("service.rs"), "// new\npub fn serve() {}\n").unwrap();

    let assert = patch_cmd(
        root,
        config.path(),
        &url,
        &["--base", "HEAD", "--fail-on-regression", "0.5", "--format", "json"],
    )
    .code(1);
    let report: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(report["passed"], false);
    assert_eq!(report["base"], "HEAD");
    let regressions: Vec<(String, String)> = report["regressions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["relative_path"].as_str().unwrap().to_string(), r["kind"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(
        regressions,
        vec![
            ("controller.rs".to_string(), "dropped".to_string()),
            ("service.rs".to_string(), "below_threshold_without_base".to_string()),
        ]
    );

    let service = report["file_diffs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["relative_path"] == "service.rs")
        .unwrap();
    assert_eq!(service["is_new"], true);
    assert!(service["profile_diffs"][0]["old_composite"].is_null());

    // The working-tree run is still saved for `gaps` and `file`.
    assert!(root.join(".qualitycheck/runs/latest.json").exists());
}

/// Good answers on every quality metric, except cohesion drops two levels in files marked
/// "WORSE_COHESION": 5.0 -> 4.5 on the composite, 5 -> 3 on cohesion.
fn cohesion_by_marker(body: &serde_json::Value) -> serde_json::Value {
    let cohesion = if body["state"].to_string().contains("WORSE_COHESION") { 2.0 } else { 4.0 };
    serde_json::json!({ "answers": {
        "naming_clarity": { "score": 4.0, "confidence": 0.9 },
        "naming_clarity:applies": { "noul": 0.97 },
        "has_dead_code": { "noul": 0.02 },
        "complexity_level": { "choice": "low", "confidence": 0.95 },
        "cohesion": { "score": cohesion, "confidence": 0.9 },
        "cohesion:applies": { "noul": 0.97 }
    }})
}

#[test]
fn test_cli_patch_fail_on_metric_regression_catches_what_the_composite_averages_away() {
    let url = spawn_content_aware_mock_jev(cohesion_by_marker);
    let (tmp, _repo) = repo_with_quality_history();
    let root = tmp.path();
    let config = tempdir().unwrap();
    fs::write(root.join("controller.rs"), "// WORSE_COHESION\npub fn handle() {}\n").unwrap();

    patch_cmd(root, config.path(), &url, &["--base", "HEAD", "--fail-on-regression", "0.5", "--no-color"])
        .success()
        .stdout(predicate::str::contains("[quality] composite: 5.0 -> 4.5 (-0.5)"));

    let assert = patch_cmd(
        root,
        config.path(),
        &url,
        &["--base", "HEAD", "--fail-on-regression", "0.5", "--fail-on-metric-regression", "1.0", "--format", "json"],
    )
    .code(1);
    let report: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(report["regressions"], serde_json::json!([]));
    assert_eq!(report["max_metric_regression"], 1.0);
    let regressions = report["metric_regressions"].as_array().unwrap();
    assert_eq!(regressions.len(), 1);
    assert_eq!(regressions[0]["relative_path"], "controller.rs");
    assert_eq!(regressions[0]["metric_id"], "cohesion");
    assert_eq!(regressions[0]["weighted_drop"], 2.0);

    patch_cmd(root, config.path(), &url, &["--base", "HEAD", "--fail-on-metric-regression", "1.0", "--no-color"])
        .code(1)
        .stdout(predicate::str::contains("Metric regressions (allowed drop: 1.0):"))
        .stdout(predicate::str::contains("controller.rs [quality] cohesion: 5.0 -> 3.0 (weighted drop 2.0)"));
}

/// Scores cohesion at the lowest level and locates it in the second region of the file; any
/// other quality answer is good.
fn cohesion_located_in_second_region(body: &serde_json::Value) -> serde_json::Value {
    let questions = body["questions"].as_object().unwrap();
    if questions.contains_key("cohesion#0") {
        let answers: serde_json::Map<String, serde_json::Value> = questions
            .keys()
            .map(|id| (id.clone(), serde_json::json!({ "noul": if id == "cohesion#1" { 0.9 } else { 0.1 } })))
            .collect();
        return serde_json::json!({ "answers": answers, "usage": { "input_tokens": 700, "output_tokens": 30 } });
    }
    serde_json::json!({ "answers": {
        "naming_clarity": { "score": 4.0, "confidence": 0.9 },
        "naming_clarity:applies": { "noul": 0.97 },
        "has_dead_code": { "noul": 0.02 },
        "complexity_level": { "choice": "low", "confidence": 0.95 },
        "cohesion": { "score": 0.0, "confidence": 0.9 },
        "cohesion:applies": { "noul": 0.97 }
    }, "usage": { "input_tokens": 500, "output_tokens": 20 } })
}

/// Answers scoring requests like `cohesion_located_in_second_region`, and explain requests
/// with nothing usable.
fn explain_request_fails(body: &serde_json::Value) -> serde_json::Value {
    if body["questions"].as_object().unwrap().contains_key("cohesion#0") {
        return serde_json::json!({ "answers": {} });
    }
    cohesion_located_in_second_region(body)
}

/// 30 three-line functions separated by blank lines: long enough to split into regions.
fn many_functions() -> String {
    (0..30).map(|i| format!("fn f{i}() {{\n    step();\n}}\n")).collect::<Vec<_>>().join("\n")
}

#[test]
fn test_cli_scan_explain_locates_failing_metrics() {
    let url = spawn_content_aware_mock_jev(cohesion_located_in_second_region);
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("lib.rs"), many_functions()).unwrap();
    let config = tempdir().unwrap();
    let scan = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("qualitycheck").unwrap();
        cmd.current_dir(tmp.path())
            .env("QUALITYCHECK_CONFIG_DIR", config.path())
            .env("JEV_API_KEY", "test_key")
            .env("JEV_API_URL", &url)
            .args(["scan", ".", "--explain"])
            .args(args);
        cmd.assert()
    };
    let json = |assert: assert_cmd::assert::Assert| -> serde_json::Value {
        serde_json::from_slice(&assert.get_output().stdout).unwrap()
    };
    let metric = |run: &serde_json::Value, id: &str| {
        run["files"][0]["profiles"][0]["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["metric_id"] == id)
            .unwrap()
            .clone()
    };

    let run = json(scan(&["--format", "json"]).success());
    let cohesion = metric(&run, "cohesion");
    let hotspots = cohesion["evidence"]["hotspots"].as_array().unwrap();
    assert_eq!(hotspots.len(), 1);
    assert_eq!(hotspots[0]["probability"], 0.9);
    let regions = cohesion["evidence"]["regions"].as_array().unwrap();
    assert!(regions.len() >= 2);
    assert_eq!(hotspots[0]["start_line"], regions[1]["start_line"]);
    // Only metrics below the threshold are located.
    assert!(metric(&run, "naming_clarity").get("evidence").is_none());
    assert_eq!(run["files"][0]["explain_usage"]["input_tokens"], 700);
    assert_eq!(run["usage"]["input_tokens"], 1200);

    // Scores and evidence both come from the cache the second time.
    let cached = json(scan(&["--format", "json"]).success());
    assert_eq!(metric(&cached, "cohesion")["evidence"], cohesion["evidence"]);
    assert!(cached["files"][0].get("explain_usage").is_none());
    assert_eq!(cached["usage"]["input_tokens"], 0);

    let start = hotspots[0]["start_line"].as_u64().unwrap();
    let end = hotspots[0]["end_line"].as_u64().unwrap();
    scan(&["--no-color"])
        .success()
        .stdout(predicate::str::contains(format!("        at L{start}-{end} (90%)")));
}

#[test]
fn test_cli_scan_explain_failure_keeps_the_scores() {
    let url = spawn_content_aware_mock_jev(explain_request_fails);
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("lib.rs"), many_functions()).unwrap();
    let config = tempdir().unwrap();

    let assert = Command::cargo_bin("qualitycheck")
        .unwrap()
        .current_dir(tmp.path())
        .env("QUALITYCHECK_CONFIG_DIR", config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &url)
        .args(["scan", ".", "--explain", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Could not locate findings in lib.rs"));
    let run: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let metrics = run["files"][0]["profiles"][0]["metrics"].as_array().unwrap();
    assert!(metrics.iter().all(|m| m.get("evidence").is_none()));
    assert!(run["files"][0]["profiles"][0]["composite_score"].is_number());
}

#[test]
fn test_cli_scan_github_format_annotates_hotspots_and_writes_the_step_summary() {
    let url = spawn_content_aware_mock_jev(cohesion_located_in_second_region);
    let tmp = tempdir().unwrap();
    fs::create_dir(tmp.path().join("src")).unwrap();
    fs::write(tmp.path().join("src").join("lib.rs"), many_functions()).unwrap();
    let config = tempdir().unwrap();
    let summary = tmp.path().join("summary.md");
    fs::write(&summary, "earlier step\n").unwrap();

    let assert = Command::cargo_bin("qualitycheck")
        .unwrap()
        .current_dir(tmp.path())
        .env("QUALITYCHECK_CONFIG_DIR", config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &url)
        .env("GITHUB_WORKSPACE", tmp.path())
        .env("GITHUB_STEP_SUMMARY", &summary)
        .args(["scan", "src", "--explain", "--format", "github"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();

    // Paths are repository-relative although the scan target was `src`; the quality profile
    // passes (4.0), so its low metric is a warning, pinned to the region --explain located.
    let annotations: Vec<&str> = stdout.lines().collect();
    assert_eq!(annotations.len(), 1, "{stdout}");
    assert!(annotations[0].starts_with("::warning file=src/lib.rs,line="), "{stdout}");
    assert!(annotations[0].contains(",title=qualitycheck%3A cohesion 1.0/5 (quality)::The file mixes many"));

    let summary = fs::read_to_string(&summary).unwrap();
    assert!(summary.starts_with("earlier step\n## qualitycheck"), "{summary}");
    assert!(summary.contains("✅ Every file meets its profile thresholds · 1 file(s) · 1 finding(s)"));
    assert!(summary.contains("| `src/lib.rs` | 4.0 |"));
    assert!(summary.contains("| ⚠️ | `src/lib.rs` | cohesion 1.0/5 (quality) | L"));
}

#[test]
fn test_cli_patch_ci_formats_report_regressions_as_errors() {
    let url = spawn_content_aware_mock_jev(cohesion_by_marker);
    let (tmp, _repo) = repo_with_quality_history();
    let root = tmp.path();
    let config = tempdir().unwrap();
    fs::write(root.join("controller.rs"), "// WORSE_COHESION\npub fn handle() {}\n").unwrap();
    let gates = ["--base", "HEAD", "--fail-on-regression", "0.5", "--fail-on-metric-regression", "1.0"];

    let assert = Command::cargo_bin("qualitycheck")
        .unwrap()
        .current_dir(root)
        .env("QUALITYCHECK_CONFIG_DIR", config.path())
        .env("JEV_API_KEY", "test_key")
        .env("JEV_API_URL", &url)
        .env("GITHUB_WORKSPACE", root)
        .env_remove("GITHUB_STEP_SUMMARY")
        .arg("patch")
        .args(gates)
        .args(["--format", "github"])
        .assert()
        .code(1);
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    // cohesion 5 -> 3 is a metric regression; it isn't below fail_below, but it failed the gate.
    assert_eq!(
        stdout.trim(),
        "::error file=controller.rs,title=qualitycheck%3A cohesion 3.0/5 (quality)::The file mostly serves one purpose, with one or two parts that would fit better in another module."
    );

    patch_cmd(root, config.path(), &url, &[&gates[..], &["--format", "markdown"]].concat())
        .code(1)
        .stdout(predicate::str::contains("❌ 1 regression(s) vs `HEAD` · 1 file(s) · 1 finding(s)"))
        .stdout(predicate::str::contains("| `controller.rs` | 5.0 → 4.5 (-0.5) |"))
        .stdout(predicate::str::contains("| ❌ | `controller.rs` | cohesion 3.0/5 (quality) | – |"));
}

#[test]
fn test_cli_patch_delta_preview_counts_both_sides_offline() {
    let (tmp, _repo) = repo_with_quality_history();
    let root = tmp.path();
    fs::write(root.join("controller.rs"), "// v2\npub fn handle() {}\n").unwrap();
    fs::write(root.join("service.rs"), "// new\npub fn serve() {}\n").unwrap();
    let config = tempdir().unwrap();

    Command::cargo_bin("qualitycheck")
        .unwrap()
        .current_dir(root)
        .env("QUALITYCHECK_CONFIG_DIR", config.path())
        .env_remove("JEV_API_KEY")
        .args(["patch", "--base", "HEAD", "--delta", "--preview", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_files\": 3"))
        .stdout(predicate::str::contains("controller.rs (at HEAD)"))
        .stdout(predicate::str::contains("service.rs (at HEAD)").not());
}

const CONTROLLER_JAVA: &str =
    "class Controlador { Servicio servicio; void post(Req r) { servicio.crear(r); } }\n";

/// High quality only when the request carries related files as context, with every question
/// told to judge `file` alone; low otherwise.
fn quality_with_context(body: &serde_json::Value) -> serde_json::Value {
    let has_context = body["state"]["related_files"].as_array().is_some_and(|r| !r.is_empty());
    let questions_scoped = body["questions"]
        .as_object()
        .unwrap()
        .values()
        .all(|q| q["instructions"].as_str().unwrap().contains("Answer only about `file.content`"));
    quality_answers(has_context && questions_scoped)
}

fn file_entry<'a>(run: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    run["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["relative_path"] == path)
        .unwrap_or_else(|| panic!("{path} missing from run"))
}

#[test]
fn test_cli_patch_context_sends_the_delegated_service() {
    let url = spawn_content_aware_mock_jev(quality_with_context);
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let _repo = git2::Repository::init(root).unwrap();
    fs::create_dir_all(root.join("web")).unwrap();
    fs::create_dir_all(root.join("core")).unwrap();
    fs::write(root.join("web/Controlador.java"), CONTROLLER_JAVA).unwrap();
    fs::write(root.join("core/Servicio.java"), "class Servicio { void crear(Req r) {} }\n").unwrap();
    let config = tempdir().unwrap();

    let with_context = patch_cmd(root, config.path(), &url, &["--context", "--format", "json"]).success();
    let run: serde_json::Value = serde_json::from_slice(&with_context.get_output().stdout).unwrap();
    let controller = file_entry(&run, "web/Controlador.java");
    assert_eq!(controller["context_files"], serde_json::json!(["core/Servicio.java"]));
    assert_eq!(controller["profiles"][0]["composite_score"], 4.7);
    assert_eq!(
        file_entry(&run, "core/Servicio.java")["context_files"],
        serde_json::json!(["web/Controlador.java"])
    );

    // Answers given with context are cached separately from answers without it.
    let without = patch_cmd(root, config.path(), &url, &["--format", "json"]).success();
    let run: serde_json::Value = serde_json::from_slice(&without.get_output().stdout).unwrap();
    let controller = file_entry(&run, "web/Controlador.java");
    assert_eq!(controller["served_from_cache"], false);
    assert!(controller.get("context_files").is_none());
    assert_eq!(controller["profiles"][0]["composite_score"], 1.0);
}

/// High quality only when the related files shown are the base (v1) version of the service.
fn quality_if_service_v1_in_context(body: &serde_json::Value) -> serde_json::Value {
    quality_answers(body["state"]["related_files"].to_string().contains("SERVICE_V1"))
}

#[test]
fn test_cli_patch_delta_context_uses_base_versions_of_related_files() {
    let url = spawn_content_aware_mock_jev(quality_if_service_v1_in_context);
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let repo = git2::Repository::init(root).unwrap();
    fs::create_dir_all(root.join("web")).unwrap();
    fs::create_dir_all(root.join("core")).unwrap();
    fs::write(root.join("web/Controlador.java"), format!("{CONTROLLER_JAVA}// v1\n")).unwrap();
    fs::write(root.join("core/Servicio.java"), "class Servicio { /* SERVICE_V1 */ }\n").unwrap();
    commit_all(&repo, "base");

    fs::write(root.join("web/Controlador.java"), format!("{CONTROLLER_JAVA}// v2\n")).unwrap();
    fs::write(root.join("core/Servicio.java"), "class Servicio { /* SERVICE_V2 */ }\n").unwrap();
    let config = tempdir().unwrap();

    // Base side: the controller is shown the v1 service (high); working tree: v2 (low). Had the
    // base side been shown the working-tree service, both sides would score low and match.
    let assert = patch_cmd(
        root,
        config.path(),
        &url,
        &["--base", "HEAD", "--context", "--fail-on-regression", "0.5", "--format", "json"],
    )
    .code(1);
    let report: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let regressed: Vec<&str> = report["regressions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["relative_path"].as_str().unwrap())
        .collect();
    assert_eq!(regressed, vec!["web/Controlador.java"]);
}

/// A project mixing source code with docs, config, vendored, minified, and generated files.
fn mixed_project(root: &std::path::Path) {
    let files: &[(&str, &str)] = &[
        ("src/app.ts", "export function start() { return 1; }\n"),
        ("src/api.pb.go", "package api\n"),
        ("src/schema.ts", "// @generated by prisma\nexport type User = { id: string };\n"),
        ("static/app.min.js", "function a(){return 1}\n"),
        ("node_modules/lib/index.js", "module.exports = 1;\n"),
        ("README.md", "# Project\n"),
        ("package.json", "{ \"name\": \"p\" }\n"),
        (".github/workflows/ci.yml", "on: push\n"),
    ];
    for (rel, content) in files {
        fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
        fs::write(root.join(rel), content).unwrap();
    }
}

fn preview_json(root: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let config = tempdir().unwrap();
    let assert = Command::cargo_bin("qualitycheck")
        .unwrap()
        .current_dir(root)
        .env("QUALITYCHECK_CONFIG_DIR", config.path())
        .env_remove("JEV_API_KEY")
        .args(args)
        .args(["--preview", "--full", "--format", "json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout).unwrap()
}

fn previewed_paths(preview: &serde_json::Value) -> Vec<String> {
    preview["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["relative_path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn test_cli_scan_scores_only_source_code_by_default() {
    let tmp = tempdir().unwrap();
    mixed_project(tmp.path());

    let preview = preview_json(tmp.path(), &["scan", "."]);
    assert_eq!(previewed_paths(&preview), vec!["src/app.ts"]);
    assert_eq!(preview["skipped_non_source_files"], 7);

    // --all-files scores every text file.
    let all = preview_json(tmp.path(), &["scan", ".", "--all-files"]);
    assert_eq!(all["total_files"], 8);
    assert_eq!(all["skipped_non_source_files"], 0);

    // A file named explicitly is scored whatever its type.
    let explicit = preview_json(tmp.path(), &["scan", "README.md", "src/app.ts"]);
    assert_eq!(previewed_paths(&explicit), vec!["README.md", "src/app.ts"]);

    // The skip is reported on stderr, keeping JSON stdout clean.
    Command::cargo_bin("qualitycheck")
        .unwrap()
        .current_dir(tmp.path())
        .env_remove("JEV_API_KEY")
        .args(["scan", ".", "--preview", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped 7 file(s) that aren't source code"));
}

#[test]
fn test_cli_patch_scores_only_changed_source_code_by_default() {
    let tmp = tempdir().unwrap();
    let _repo = git2::Repository::init(tmp.path()).unwrap();
    mixed_project(tmp.path());

    let preview = preview_json(tmp.path(), &["patch"]);
    assert_eq!(previewed_paths(&preview), vec!["src/app.ts"]);
    assert_eq!(preview["skipped_non_source_files"], 7);

    let all = preview_json(tmp.path(), &["patch", "--all-files"]);
    assert_eq!(all["total_files"], 8);
}
