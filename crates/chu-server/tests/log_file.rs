//! `--log-file`: the binary appends its log there as plain text, across
//! restarts, while still logging to the console.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// Starts the server, waits for "listening on" on its console, then stops it
/// and returns the console log.
fn run_once(dir: &std::path::Path, log: &std::path::Path) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_chu-server"))
        .args(["--listen", "127.0.0.1:0", "--http", "127.0.0.1:0"])
        .arg("--db")
        .arg(format!("sqlite://{}", dir.join("chu.db").display()))
        .arg("--records")
        .arg(dir.join("records"))
        .arg("--leagues")
        .arg(dir.join("leagues.toml"))
        .arg("--log-file")
        .arg(log)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut console = String::new();
    for line in BufReader::new(child.stderr.take().unwrap()).lines() {
        let line = line.unwrap();
        console.push_str(&line);
        console.push('\n');
        if line.contains("listening on") {
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    console
}

#[test]
fn log_file_gets_the_log_and_is_appended_to() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("log-file");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("leagues.toml"),
        "[[league]]\nname = \"test\"\nkind = \"test\"\ntotal_s = 60\nbyoyomi_s = 0\nincrement_s = 1\nrules = \"lishogi\"\nrated = false\n",
    )
    .unwrap();
    let log = dir.join("server.log");

    let console = run_once(&dir, &log);
    assert!(console.contains("listening on"), "{console}");
    let first = std::fs::read_to_string(&log).unwrap();
    assert!(first.contains("logging to") && first.contains("league test") && first.contains("listening on"), "{first}");
    assert!(!first.contains('\x1b'), "colour codes in the file: {first:?}");

    run_once(&dir, &log);
    let second = std::fs::read_to_string(&log).unwrap();
    assert!(second.starts_with(&first), "the file was not appended to");
    assert_eq!(second.matches("listening on").count(), 2, "{second}");
}

#[test]
fn an_unwritable_log_file_is_an_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_chu-server"))
        .args(["--log-file", "/nonexistent-dir/server.log"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--log-file /nonexistent-dir/server.log"), "{err}");
}
