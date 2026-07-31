//! #1063 scan⇔watch parity: the `include`/`exclude` PATH-MATCH decision must be
//! byte-identical between the engine scan (`codegraph index`) and the live
//! watcher (`WatchPolicy`), or `sync`/watch would drop a file `index` kept —
//! violating the AGENTS.md "sync == index --force" invariant. This guards the
//! `gen*` whole-path-vs-basename divergence specifically.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use codegraph_extract::{ExtractOptions, engine::scan_project};
use codegraph_watch::WatchPolicy;

fn unique_project(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "cg_parity_{tag}_{}_{nanos}_{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp project");
    dir
}

fn touch(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).expect("create parent dirs");
    fs::write(&path, contents).expect("write file");
}

/// For each include pattern form, a file's membership in the engine scan output
/// must equal `WatchPolicy::should_handle_file`, proving the two matchers agree.
#[test]
fn scan_and_watch_agree_on_include_file_verdicts() {
    let project = unique_project("include");
    touch(&project, ".gitignore", "gen/\nTools/\nLocal/\n");
    touch(&project, "src/app.ts", "export const a = 1;");
    touch(&project, "gen/helper.ts", "export const g = 1;");
    touch(&project, "Tools/helper.ts", "export const t = 1;");
    touch(&project, "Local/ts/wanted.ts", "export const w = 1;");
    touch(&project, "Local/ts/other.ts", "export const o = 1;");
    touch(&project, "Local/skip.ts", "export const s = 1;");

    let candidate_files = [
        "src/app.ts",
        "gen/helper.ts",
        "Tools/helper.ts",
        "Local/ts/wanted.ts",
        "Local/ts/other.ts",
        "Local/skip.ts",
    ];

    for include in [
        vec!["gen*".to_string()],
        vec!["Tools/".to_string()],
        vec!["Local/ts/**".to_string()],
        vec!["Local/ts/".to_string()],
    ] {
        let options = ExtractOptions {
            include: include.clone(),
            ..ExtractOptions::default()
        };
        let scanned = scan_project(&project, &options).expect("scan");
        let policy = WatchPolicy::with_config(&project, &options.ignore_dirs, &include, &[]);

        for file in candidate_files {
            let in_scan = scanned.iter().any(|f| f == file);
            let watched = policy.should_handle_file(file);
            assert_eq!(
                in_scan, watched,
                "scan⇔watch parity broken for include={include:?} file={file}: \
                 scan={in_scan} watch={watched}"
            );
        }
    }
}

/// The specific #1063 blocker: `gen*` (a documented supported form) must both
/// INDEX and WATCH a gitignored `gen/helper.ts` consistently.
#[test]
fn gen_glob_indexes_and_watches_gitignored_file() {
    let project = unique_project("gen_glob");
    touch(&project, ".gitignore", "gen/\n");
    touch(&project, "gen/helper.ts", "export const g = 1;");

    let include = vec!["gen*".to_string()];
    let options = ExtractOptions {
        include: include.clone(),
        ..ExtractOptions::default()
    };
    let scanned = scan_project(&project, &options).expect("scan");
    assert!(
        scanned.iter().any(|f| f == "gen/helper.ts"),
        "gen* must index gen/helper.ts: {scanned:?}"
    );

    let policy = WatchPolicy::with_config(&project, &options.ignore_dirs, &include, &[]);
    assert!(
        policy.should_handle_file("gen/helper.ts"),
        "gen* must watch gen/helper.ts (parity with scan)"
    );
    assert!(
        policy.should_watch_dir("gen"),
        "gen* must keep the gen/ dir watchable"
    );
}

#[test]
fn configured_exclude_applies_with_empty_include() {
    let project = unique_project("exclude_empty_include");
    touch(
        &project,
        "src/generated.ts",
        "export const generated = true;",
    );

    let exclude = vec!["src/generated.ts".to_string()];
    let options = ExtractOptions {
        exclude: exclude.clone(),
        ..ExtractOptions::default()
    };
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(&project, &options.ignore_dirs, &[], &exclude);

    assert!(!scanned.iter().any(|file| file == "src/generated.ts"));
    assert!(
        !policy.should_handle_file("src/generated.ts"),
        "configured exclude must apply even when include is empty"
    );
}

/// A configured `ignore_dirs` entry is STRUCTURAL: `scan_dir` prunes it with a
/// bare `continue` before any pattern set is evaluated, so a `.gitignore`
/// negation can never resurface it. The watcher must agree.
#[test]
fn gitignore_negation_cannot_reinclude_a_configured_ignore_dir() {
    let project = unique_project("negate_ignore_dir");
    touch(&project, ".gitignore", "!addons/\n");
    touch(&project, "src/app.ts", "export const a = 1;");
    touch(
        &project,
        "addons/vendor_plugin/plugin.ts",
        "export const p = 1;",
    );

    let options = ExtractOptions::default();
    assert!(
        options.ignore_dirs.iter().any(|dir| dir == "addons"),
        "this test needs `addons` to be a configured ignore dir"
    );
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(&project, &options.ignore_dirs, &[], &[]);

    assert!(scanned.iter().any(|file| file == "src/app.ts"));
    assert!(policy.should_handle_file("src/app.ts"));
    assert!(
        !scanned
            .iter()
            .any(|file| file == "addons/vendor_plugin/plugin.ts"),
        "scan must keep pruning a configured ignore dir despite `!addons/`: {scanned:?}"
    );
    assert!(
        !policy.should_watch_dir("addons"),
        "a .gitignore negation must NOT re-include a configured ignore dir (parity with scan)"
    );
    assert!(
        !policy.should_handle_file("addons/vendor_plugin/plugin.ts"),
        "a .gitignore negation must NOT re-include files under a configured ignore dir"
    );
}

/// The precision boundary of the rule above: `exclude` and `.gitignore` DO share
/// one last-match-wins stream on both sides, so a later `!pattern` still
/// overrides an earlier `exclude` match. Guards against over-correcting the
/// structural rule into "nothing can ever be negated".
#[test]
fn gitignore_negation_still_overrides_a_configured_exclude() {
    let project = unique_project("negate_exclude");
    touch(&project, ".gitignore", "!Tools/\n");
    touch(&project, "Tools/helper.ts", "export const t = 1;");

    let exclude = vec!["Tools/".to_string()];
    let options = ExtractOptions {
        exclude: exclude.clone(),
        ..ExtractOptions::default()
    };
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(&project, &options.ignore_dirs, &[], &exclude);

    assert!(
        scanned.iter().any(|file| file == "Tools/helper.ts"),
        "a .gitignore negation must override an earlier exclude in the scan: {scanned:?}"
    );
    assert!(
        policy.should_watch_dir("Tools"),
        "a .gitignore negation must override an earlier exclude for the watcher too"
    );
    assert!(
        policy.should_handle_file("Tools/helper.ts"),
        "a .gitignore negation must override an earlier exclude for files as well"
    );
}
