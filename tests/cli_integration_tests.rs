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
        .stdout(predicate::str::contains("Composite score: 4.6/5 [PASS]"));

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
        .stdout(predicate::str::contains("Composite score: quality 4.6/5"));

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
            "naming_clarity: 1/5 (90% conf.) [excluded: not applicable to this file]",
        ))
        .stdout(predicate::str::contains("Composite score: quality 5.0/5"));
}
