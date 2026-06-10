//! End-to-end tests: the real `stp` binary driving the in-workspace `stp-mock-provider` over
//! stdio. Exercises the host pipeline and the exit-code taxonomy (SPEC §4).

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

/// Path to the mock provider binary (built on demand if missing).
fn mock() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    p.pop(); // debug/
    let name = if cfg!(windows) {
        "stp-mock-provider.exe"
    } else {
        "stp-mock-provider"
    };
    p.push(name);
    if !p.exists() {
        let status = std::process::Command::new(env!("CARGO"))
            .args(["build", "-p", "stp-mock-provider"])
            .status()
            .expect("build mock provider");
        assert!(status.success(), "failed to build mock provider");
    }
    p
}

/// A `stp` command wired to spawn the mock, with a hard timeout so a hung handshake fails the
/// test instead of hanging CI.
fn stp() -> Command {
    let mut c = Command::cargo_bin("stp").unwrap();
    c.arg("--exec").arg(mock().to_str().unwrap());
    c.timeout(Duration::from_secs(30));
    c
}

#[test]
fn version_needs_no_connection() {
    Command::cargo_bin("stp")
        .unwrap()
        .arg("version")
        .assert()
        .success()
        .stdout(predicate::str::contains("stp"));
}

#[test]
fn no_server_is_usage_error() {
    Command::cargo_bin("stp")
        .unwrap()
        .arg("tools")
        .env_remove("STP_URL")
        .env_remove("STP_EXEC")
        .env_remove("STP_SERVER")
        .assert()
        .code(2);
}

#[test]
fn tools_lists_fixtures() {
    stp()
        .arg("tools")
        .assert()
        .success()
        .stdout(predicate::str::contains("t2i"))
        .stdout(predicate::str::contains("Mock Text-to-Image"));
}

#[test]
fn tools_json_is_parseable() {
    let out = stp().args(["tools", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == "t2i"));
}

#[test]
fn show_tool_shows_params() {
    stp()
        .args(["show", "t2i"])
        .assert()
        .success()
        .stdout(predicate::str::contains("prompt"))
        .stdout(predicate::str::contains("sampler"));
}

#[test]
fn show_no_tool_shows_provider() {
    stp()
        .args(["show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Mock STP Provider"))
        .stdout(predicate::str::contains("stp-mock-provider/"))
        .stdout(predicate::str::contains("capabilities"))
        // The mock sends a compliant `Name/Version` `server`, so no product-identity warning.
        .stderr(predicate::str::contains("warning:").not());
}

#[test]
fn show_unknown_tool_is_usage_error() {
    stp().args(["show", "nope"]).assert().code(2);
}

#[test]
fn run_writes_output_and_prints_path() {
    let dir = tempfile::tempdir().unwrap();
    let assert = stp()
        .current_dir(dir.path())
        .args(["run", "t2i", "a cat", "--steps", "5"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let path = PathBuf::from(stdout.trim());
    assert!(path.exists(), "expected written asset at {path:?}");
}

#[test]
fn run_json_no_download_is_clean() {
    let out = stp()
        .args(["run", "t2i", "a cat", "--json", "--no-download"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["success"], serde_json::json!(true));
    assert_eq!(
        v["output"]["assets"][0]["role"],
        serde_json::json!("primary")
    );
    assert!(v["paths"].as_array().unwrap().is_empty());
}

#[test]
fn run_stdout_streams_bytes() {
    let out = stp()
        .args(["run", "t2i", "a cat", "--stdout"])
        .output()
        .unwrap();
    assert!(out.status.success());
    // 1x1 PNG starts with the PNG magic.
    assert_eq!(&out.stdout[..4], &[0x89, b'P', b'N', b'G']);
}

#[test]
fn bad_value_is_validation_error() {
    stp()
        .args(["run", "t2i", "x", "--steps", "999"])
        .assert()
        .code(6)
        .stderr(predicate::str::contains("maximum"));
}

#[test]
fn unknown_param_is_validation_error() {
    stp()
        .args(["run", "t2i", "x", "--stepps", "5"])
        .assert()
        .code(6)
        .stderr(predicate::str::contains("unknown parameter"));
}

#[test]
fn dry_run_previews_without_executing() {
    let out = stp()
        .args([
            "run",
            "t2i",
            "a cat",
            "--guidance",
            "1",
            "--dry-run",
            "--json",
            "--allow-unknown",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["parameters"]["prompt"], serde_json::json!("a cat"));
    assert!(v["upload_plan"].as_array().unwrap().is_empty());
}

#[test]
fn job_failure_maps_to_exit_4() {
    stp()
        .args(["run", "boom", "x"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("EXECUTION_FAILED"));
}

#[test]
fn timeout_maps_to_exit_5() {
    stp()
        .args(["run", "slow", "x", "--timeout", "1"])
        .assert()
        .code(5);
}

#[test]
fn multi_asset_role_selection() {
    // primary is unambiguous → --stdout works without --role.
    let out = stp()
        .args(["run", "multi", "x", "--stdout"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(&out.stdout[..4], &[0x89, b'P', b'N', b'G']);

    // explicit preview role also selects.
    let out = stp()
        .args(["run", "multi", "x", "--stdout", "--role", "preview"])
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn img2img_auto_uploads_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.png");
    std::fs::write(&input, b"ROUNDTRIP").unwrap();
    let outdir = dir.path().join("out");

    stp()
        .args(["run", "edit", "make it blue"])
        .arg("--input_images")
        .arg(&input)
        .arg("-o")
        .arg(format!("{}/", outdir.display()))
        .assert()
        .success()
        .stderr(predicate::str::contains("uploading"));

    // The mock round-trips the input bytes.
    let written = std::fs::read_dir(&outdir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(std::fs::read(written).unwrap(), b"ROUNDTRIP");
}

#[test]
fn grammar_named_file_still_uploads() {
    // Regression: our own output naming (`gen_<ts>.png`) matches the asset-id grammar. Feeding
    // such a file straight back in must upload it, not pass the name through as a bogus id.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("gen_20260604-213023.png"); // matches the asset-id grammar
    std::fs::write(&input, b"ROUNDTRIP").unwrap();
    let outdir = dir.path().join("out");

    stp()
        .args(["run", "edit", "make it blue"])
        .arg("--input_images")
        .arg(&input)
        .arg("-o")
        .arg(format!("{}/", outdir.display()))
        .assert()
        .success()
        .stderr(predicate::str::contains("uploading"));

    let written = std::fs::read_dir(&outdir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(std::fs::read(written).unwrap(), b"ROUNDTRIP");
}

#[test]
fn raw_reaches_methods_directly() {
    let out = stp().args(["raw", "tools.list"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["tools"].is_array());
}

#[test]
fn agent_guide_json_is_structured() {
    let out = Command::cargo_bin("stp")
        .unwrap()
        .args(["agent-guide", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["role"], serde_json::json!("host"));
    assert!(v["exit_codes"].is_object());
}

#[test]
fn config_add_list_remove_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    let with_cfg = || {
        let mut c = Command::cargo_bin("stp").unwrap();
        c.env("STP_CONFIG", &cfg);
        c
    };
    with_cfg()
        .args([
            "config",
            "add",
            "box",
            "--url",
            "wss://x/stp-v1",
            "--token-env",
            "T",
        ])
        .assert()
        .success();
    with_cfg()
        .args(["config", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("box"));
    with_cfg()
        .args(["config", "remove", "box"])
        .assert()
        .success();
    with_cfg()
        .args(["config", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("box").not());
}
