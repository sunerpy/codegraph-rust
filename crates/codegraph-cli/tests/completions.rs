use assert_cmd::Command;

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
