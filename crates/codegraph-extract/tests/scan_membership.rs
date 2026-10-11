//! `scan_membership` judges one path exactly as the scan walk does: for every
//! file on disk, it is `Admitted` iff `scan_project` yields that file.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use codegraph_extract::ExtractOptions;
use codegraph_extract::engine::{Membership, scan_membership, scan_project};

fn sandbox(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "cg_scan_membership_{tag}_{}_{nanos}_{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create sandbox");
    dir
}

fn touch(root: &Path, relative: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "export const value = 1;\n").unwrap();
}

/// Every regular file under `root`, root-relative.
fn files_on_disk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let relative = entry.path().strip_prefix(root).unwrap().to_path_buf();
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn membership_matches_the_walk_for_every_file_on_disk() {
    let root = sandbox("parity");
    fs::write(root.join(".gitignore"), "gen/\n!gen/keep.ts\nlogs/*.ts\n").unwrap();
    for file in [
        "src/app.ts",
        "src/util.ts",
        "gen/out.ts",
        "gen/keep.ts",
        "logs/run.ts",
        "node_modules/pkg/index.ts",
        "build/out.ts",
        "src/main/java/com/build/Builder.java",
        ".cache/cached.ts",
        ".codegraph/stray.ts",
        ".codegraph-sources/kept.ts",
        "src/.codegraph/nested.ts",
        ".git/hooks/hook.ts",
        "docs/notes.txt",
        "assets/blob.bin",
    ] {
        touch(&root, file);
    }

    for options in [
        ExtractOptions::default(),
        ExtractOptions {
            include: vec!["gen/keep.ts".to_string()],
            ..ExtractOptions::default()
        },
        ExtractOptions {
            exclude: vec!["src/util.ts".to_string(), "logs/".to_string()],
            ..ExtractOptions::default()
        },
    ] {
        let scanned = scan_project(&root, &options).expect("scan");
        for file in files_on_disk(&root) {
            let membership = scan_membership(&root, &options, &file, false);
            assert_ne!(membership, Membership::Unknown, "{file}: no symlinks here");
            assert_eq!(
                membership == Membership::Admitted,
                scanned.contains(&file),
                "{file}: membership {membership:?} vs scan {scanned:?} (include {:?}, exclude {:?})",
                options.include,
                options.exclude
            );
        }
    }
    fs::remove_dir_all(&root).ok();
}

#[test]
fn directories_missing_paths_and_symlinks() {
    let root = sandbox("dirs");
    fs::write(root.join(".gitignore"), "gen/\n").unwrap();
    touch(&root, "src/app.ts");
    touch(&root, "gen/keep.ts");
    touch(&root, "node_modules/pkg/index.ts");
    let default = ExtractOptions::default();
    assert_eq!(
        scan_membership(&root, &default, "src", true),
        Membership::Admitted
    );
    assert_eq!(
        scan_membership(&root, &default, "node_modules", true),
        Membership::Rejected
    );
    assert_eq!(
        scan_membership(&root, &default, "gen", true),
        Membership::Rejected
    );
    let include = ExtractOptions {
        include: vec!["gen/keep.ts".to_string()],
        ..ExtractOptions::default()
    };
    assert_eq!(
        scan_membership(&root, &include, "gen", true),
        Membership::Admitted,
        "an include below an ignored directory makes the walk descend"
    );
    assert_eq!(
        scan_membership(&root, &default, "src/missing.ts", false),
        Membership::Rejected
    );
    assert_eq!(
        scan_membership(&root, &default, "../escape.ts", false),
        Membership::Rejected
    );

    let outside = sandbox("dirs_outside");
    touch(&outside, "o.ts");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, root.join("linked"));
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&outside, root.join("linked"));
    match made {
        Ok(()) => {
            assert_eq!(
                scan_membership(&root, &default, "linked/o.ts", false),
                Membership::Unknown,
                "a path through a symlink is the full scan's to resolve"
            );
            assert_eq!(
                scan_membership(&root, &default, "linked", true),
                Membership::Unknown
            );
        }
        Err(error) if std::env::var_os("CI").is_some() => {
            panic!("CI must be able to create symlinks: {error}")
        }
        Err(_) => {}
    }
    fs::remove_dir_all(&root).ok();
    fs::remove_dir_all(&outside).ok();
}

/// The repository's own `.git/info/exclude` prunes the scan like a root
/// `.gitignore` (upstream #1728). The root `.gitignore` is read after it, so its
/// `!` lines re-include what the exclude file ignored, as in git.
#[test]
fn the_repositorys_info_exclude_prunes_the_scan() {
    let root = sandbox("info-exclude");
    fs::create_dir_all(root.join(".git/info")).unwrap();
    fs::write(
        root.join(".git/info/exclude"),
        "# local excludes\nscratch/\n*.local.ts\nkept/\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "!kept/\n").unwrap();
    for file in [
        "src/app.ts",
        "src/debug.local.ts",
        "scratch/try.ts",
        "kept/again.ts",
    ] {
        touch(&root, file);
    }

    let options = ExtractOptions::default();
    let scanned = scan_project(&root, &options).expect("scan");
    assert_eq!(scanned, ["kept/again.ts", "src/app.ts"]);
    for file in files_on_disk(&root) {
        let membership = scan_membership(&root, &options, &file, false);
        assert_eq!(
            membership == Membership::Admitted,
            scanned.contains(&file),
            "{file}: membership {membership:?} agrees with the scan"
        );
    }
    fs::remove_dir_all(&root).ok();
}

/// Only the project's own repository counts: a `.git` file (a linked worktree
/// or submodule) points outside the project, and is never followed.
#[test]
fn a_git_file_pointing_elsewhere_contributes_no_excludes() {
    let root = sandbox("info-exclude-gitfile");
    let elsewhere = sandbox("info-exclude-elsewhere");
    fs::create_dir_all(elsewhere.join("info")).unwrap();
    fs::write(elsewhere.join("info/exclude"), "src/\n").unwrap();
    fs::write(
        root.join(".git"),
        format!("gitdir: {}\n", elsewhere.display()),
    )
    .unwrap();
    touch(&root, "src/app.ts");

    let scanned = scan_project(&root, &ExtractOptions::default()).expect("scan");
    assert_eq!(scanned, ["src/app.ts"]);
    fs::remove_dir_all(&root).ok();
    fs::remove_dir_all(&elsewhere).ok();
}
