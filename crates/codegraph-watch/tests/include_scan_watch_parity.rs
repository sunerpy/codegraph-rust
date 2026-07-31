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
        let policy = WatchPolicy::with_config(
            &project,
            &options.ignore_dirs,
            &options.ignore_paths,
            &include,
            &[],
        );

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

    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &include,
        &[],
    );
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
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &[],
        &exclude,
    );

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
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &[],
        &[],
    );

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

/// The `ignore_paths` tier — the FIRST set in the scan's ordered `pattern_sets`
/// (`ignore_paths` → `exclude` → `.gitignore`). XML is an extractable language
/// here (Tier-2 embedded / MyBatis mapper), so without this tier the watcher
/// would keep syncing an Android `res/values/strings.xml` that `index --force`
/// never indexed — breaking "sync == index --force".
#[test]
fn default_ignore_paths_exclude_android_res_from_scan_and_watch() {
    let project = unique_project("ignore_paths_res");
    touch(&project, "src/main/java/App.java", "class App {}\n");
    touch(
        &project,
        "res/values/strings.xml",
        "<resources></resources>\n",
    );

    let options = ExtractOptions::default();
    assert!(
        options.ignore_paths.iter().any(|p| p == "res/values*"),
        "this test needs the default `res/values*` ignore path: {:?}",
        options.ignore_paths
    );
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &[],
        &[],
    );

    assert!(
        scanned.iter().any(|file| file == "src/main/java/App.java"),
        "a normal source file must still be indexed: {scanned:?}"
    );
    assert!(
        policy.should_handle_file("src/main/java/App.java"),
        "a normal source file must still be watched"
    );
    assert!(
        !scanned.iter().any(|file| file == "res/values/strings.xml"),
        "the default `res/values*` ignore path must exclude it from the scan: {scanned:?}"
    );
    assert!(
        !policy.should_handle_file("res/values/strings.xml"),
        "the default `res/values*` ignore path must also stop the watcher (parity with scan)"
    );
}

/// The precision boundary of the tier above: `default_ignore_paths` deliberately
/// preserves `res/raw/` (real assets) and MyBatis mapper XML under
/// `src/main/resources/`. Guards against over-excluding with a broader rule.
#[test]
fn default_ignore_paths_preserve_res_raw_and_resources_in_scan_and_watch() {
    let project = unique_project("ignore_paths_preserve");
    touch(&project, "res/raw/config.xml", "<config></config>\n");
    touch(
        &project,
        "src/main/resources/mapper/UserMapper.xml",
        "<mapper></mapper>\n",
    );

    let options = ExtractOptions::default();
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &[],
        &[],
    );

    for file in [
        "res/raw/config.xml",
        "src/main/resources/mapper/UserMapper.xml",
    ] {
        assert!(
            scanned.iter().any(|scanned_file| scanned_file == file),
            "{file} must stay indexed: {scanned:?}"
        );
        assert!(
            policy.should_handle_file(file),
            "{file} must stay watched (parity with scan)"
        );
    }
}

/// `ignore_paths` is a PATTERN SET, not a structural prune: it shares the
/// negotiable last-match-wins stream with `.gitignore`, so a later `!res/values/`
/// re-includes what the default `res/values*` dropped — on both sides.
#[test]
fn gitignore_negation_still_overrides_a_default_ignore_path() {
    let project = unique_project("negate_ignore_path");
    touch(&project, ".gitignore", "!res/values/\n");
    touch(
        &project,
        "res/values/strings.xml",
        "<resources></resources>\n",
    );

    let options = ExtractOptions::default();
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &[],
        &[],
    );

    assert!(
        scanned.iter().any(|file| file == "res/values/strings.xml"),
        "`!res/values/` must override the default ignore path in the scan: {scanned:?}"
    );
    assert!(
        policy.should_watch_dir("res/values"),
        "`!res/values/` must keep the dir watchable for the watcher too"
    );
    assert!(
        policy.should_handle_file("res/values/strings.xml"),
        "`!res/values/` must override the default ignore path for the watcher too"
    );
}

/// The other half of "pattern set, not structural prune": `include` force-inclusion
/// still wins over an `ignore_paths` match, because `IncludeSet::forces` re-checks
/// only `exclude`. A configured `ignore_dirs` entry stays non-re-includable.
#[test]
fn include_force_includes_a_default_ignore_path_match() {
    let project = unique_project("include_ignore_path");
    touch(
        &project,
        "res/values/strings.xml",
        "<resources></resources>\n",
    );

    let include = vec!["res/values/**".to_string()];
    let options = ExtractOptions {
        include: include.clone(),
        ..ExtractOptions::default()
    };
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &include,
        &[],
    );

    assert!(
        scanned.iter().any(|file| file == "res/values/strings.xml"),
        "`include` must force-include an ignore_paths match in the scan: {scanned:?}"
    );
    assert!(
        policy.should_handle_file("res/values/strings.xml"),
        "`include` must force-include an ignore_paths match for the watcher too"
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
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &[],
        &exclude,
    );

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

/// `ignore_paths` is a `.gitignore`-STYLE ordered set, not a flat any-match:
/// `is_path_ignored` strips a leading `!` in EVERY pattern set it folds, so a
/// later `!gen/` re-includes what an earlier `gen/` in the SAME set dropped.
#[test]
fn negated_ignore_path_reincludes_within_the_same_set() {
    let project = unique_project("negate_within_ignore_paths");
    touch(&project, "gen/helper.ts", "export const g = 1;");

    let ignore_paths = vec!["gen/".to_string(), "!gen/".to_string()];
    let options = ExtractOptions {
        ignore_paths,
        ..ExtractOptions::default()
    };
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &[],
        &[],
    );

    assert!(
        scanned.iter().any(|file| file == "gen/helper.ts"),
        "a later `!gen/` in ignore_paths must re-include it in the scan: {scanned:?}"
    );
    assert!(
        policy.should_watch_dir("gen"),
        "a later `!gen/` in ignore_paths must keep the gen/ dir watchable (parity with scan)"
    );
    assert!(
        policy.should_handle_file("gen/helper.ts"),
        "a later `!gen/` in ignore_paths must re-include the file for the watcher too"
    );
}

/// Same ordered-set contract for the SECOND pattern set: a `!` is honored inside
/// `exclude` too, so `exclude = ["gen/", "!gen/"]` leaves `gen/helper.ts` in.
#[test]
fn negated_exclude_reincludes_within_the_same_set() {
    let project = unique_project("negate_within_exclude");
    touch(&project, "gen/helper.ts", "export const g = 1;");

    let exclude = vec!["gen/".to_string(), "!gen/".to_string()];
    let options = ExtractOptions {
        exclude: exclude.clone(),
        ..ExtractOptions::default()
    };
    let scanned = scan_project(&project, &options).expect("scan");
    let policy = WatchPolicy::with_config(
        &project,
        &options.ignore_dirs,
        &options.ignore_paths,
        &[],
        &exclude,
    );

    assert!(
        scanned.iter().any(|file| file == "gen/helper.ts"),
        "a later `!gen/` in exclude must re-include it in the scan: {scanned:?}"
    );
    assert!(
        policy.should_watch_dir("gen"),
        "a later `!gen/` in exclude must keep the gen/ dir watchable (parity with scan)"
    );
    assert!(
        policy.should_handle_file("gen/helper.ts"),
        "a later `!gen/` in exclude must re-include the file for the watcher too"
    );
}

/// The set ORDER is observable, which a `||` of the two config sets cannot
/// express: `ignore_paths` folds before `exclude`, so a `!` in `exclude`
/// re-includes an `ignore_paths` match, while the mirrored config (`!` in
/// `ignore_paths`, plain pattern in `exclude`) stays excluded.
#[test]
fn config_set_order_decides_which_negation_wins() {
    let project = unique_project("negate_across_sets");
    touch(&project, "gen/helper.ts", "export const g = 1;");
    touch(&project, "out/helper.ts", "export const o = 1;");

    let later_negation = ExtractOptions {
        ignore_paths: vec!["gen/".to_string(), "out/".to_string()],
        exclude: vec!["!gen/".to_string()],
        ..ExtractOptions::default()
    };
    let scanned = scan_project(&project, &later_negation).expect("scan");
    let policy = WatchPolicy::with_config(
        &project,
        &later_negation.ignore_dirs,
        &later_negation.ignore_paths,
        &[],
        &later_negation.exclude,
    );

    assert!(
        scanned.iter().any(|file| file == "gen/helper.ts"),
        "a `!gen/` in the later exclude set must re-include the ignore_paths match: {scanned:?}"
    );
    assert!(
        policy.should_handle_file("gen/helper.ts"),
        "a `!gen/` in the later exclude set must re-include it for the watcher too"
    );
    assert!(
        !scanned.iter().any(|file| file == "out/helper.ts"),
        "the un-negated `out/` ignore path must stay excluded from the scan: {scanned:?}"
    );
    assert!(
        !policy.should_handle_file("out/helper.ts"),
        "the un-negated `out/` ignore path must stay excluded for the watcher too"
    );

    let earlier_negation = ExtractOptions {
        ignore_paths: vec!["!gen/".to_string()],
        exclude: vec!["gen/".to_string()],
        ..ExtractOptions::default()
    };
    let scanned = scan_project(&project, &earlier_negation).expect("scan");
    let policy = WatchPolicy::with_config(
        &project,
        &earlier_negation.ignore_dirs,
        &earlier_negation.ignore_paths,
        &[],
        &earlier_negation.exclude,
    );

    assert!(
        !scanned.iter().any(|file| file == "gen/helper.ts"),
        "a `!gen/` in the EARLIER ignore_paths set must not survive the later exclude: {scanned:?}"
    );
    assert!(
        !policy.should_handle_file("gen/helper.ts"),
        "a `!gen/` in the EARLIER ignore_paths set must not survive the later exclude for the \
         watcher either"
    );
}
