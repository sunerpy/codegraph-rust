use assert_cmd::Command;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn assert_completions(shell: &str) {
    let assert = Command::cargo_bin("codegraph")
        .expect("locate codegraph binary")
        .args(["completions", shell])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("completion script is valid utf-8");
    assert!(
        stdout.len() > 100,
        "{shell} completion script is too short ({} bytes)",
        stdout.len()
    );
    assert!(
        stdout.contains("codegraph"),
        "{shell} completion script does not mention codegraph"
    );
}

#[test]
fn completions_bash() {
    assert_completions("bash");
}

#[test]
fn completions_zsh() {
    assert_completions("zsh");
}

#[test]
fn completions_powershell() {
    assert_completions("powershell");
}

#[test]
fn completions_fish() {
    assert_completions("fish");
}

#[test]
fn completions_elvish() {
    assert_completions("elvish");
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("codegraph-completions-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn completions_bash_install_writes_file() {
    let home = unique_temp_dir("bash-home");
    let data = unique_temp_dir("bash-data");
    Command::cargo_bin("codegraph")
        .expect("locate codegraph binary")
        .args(["completions", "bash", "--install"])
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &data)
        .assert()
        .success();
    let target = data.join("bash-completion/completions/codegraph");
    let script = std::fs::read_to_string(&target).expect("bash completion file written");
    assert!(
        script.contains("codegraph"),
        "bash completion file does not mention codegraph"
    );
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&data).ok();
}

#[test]
fn completions_powershell_install_is_idempotent() {
    let local = unique_temp_dir("ps-local");
    let profile_dir = unique_temp_dir("ps-profile");
    let profile = profile_dir.join("Microsoft.PowerShell_profile.ps1");

    let run = || {
        Command::cargo_bin("codegraph")
            .expect("locate codegraph binary")
            .args(["completions", "powershell", "--install"])
            .env("LOCALAPPDATA", &local)
            .env("CODEGRAPH_PS_PROFILE", &profile)
            .assert()
            .success();
    };

    run();
    let script = local.join("codegraph/completion.ps1");
    let script_body = std::fs::read_to_string(&script).expect("powershell script written");
    assert!(
        script_body.contains("using namespace System.Management.Automation"),
        "powershell script missing using-namespace header"
    );

    let dot_source = format!(". \"{}\"", script.display());
    let count_lines = || {
        std::fs::read_to_string(&profile)
            .expect("profile written")
            .lines()
            .filter(|l| l.trim() == dot_source)
            .count()
    };
    assert_eq!(
        count_lines(),
        1,
        "first install must add exactly one dot-source line"
    );

    run();
    assert_eq!(
        count_lines(),
        1,
        "re-install must not duplicate the dot-source line"
    );

    std::fs::remove_dir_all(&local).ok();
    std::fs::remove_dir_all(&profile_dir).ok();
}
