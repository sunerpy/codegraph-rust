//! The project scan follows symlinked files and directories (upstream #935).
//!
//! Precedence is deterministic: a canonical directory is scanned once, under
//! the logical path that reaches it through the fewest symlinks, ties going to
//! the lexicographically smallest path. Links to the project root or one of its
//! ancestors, into `.git` or a reserved index root, broken links, and
//! unreadable targets are not followed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use codegraph_extract::engine::{LinkKind, ScanProjectResult, scan_project_with_stats};
use codegraph_extract::ExtractOptions;

fn sandbox(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "cg_scan_links_{tag}_{}_{nanos}_{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create sandbox");
    dir
}

fn touch(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).expect("create parent dirs");
    fs::write(&path, contents).expect("write file");
}

/// Create a symlink. Without the privilege (Windows) a local run skips the
/// test, but CI must exercise it, so there a refusal fails loudly.
fn link(target: &Path, link: &Path, dir: bool) -> bool {
    fs::create_dir_all(link.parent().unwrap()).expect("create link parent");
    #[cfg(unix)]
    let made = {
        let _ = dir;
        std::os::unix::fs::symlink(target, link)
    };
    #[cfg(windows)]
    let made = if dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match made {
        Ok(()) => true,
        Err(error) if std::env::var_os("CI").is_some() => {
            panic!("CI must be able to create symlinks: {error}")
        }
        Err(_) => false,
    }
}

fn link_dir(target: &Path, at: &Path) -> bool {
    link(target, at, true)
}

fn link_file(target: &Path, at: &Path) -> bool {
    link(target, at, false)
}

fn scan(project: &Path) -> ScanProjectResult {
    scan_project_with_stats(project, &ExtractOptions::default()).expect("scan")
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn link_summary(result: &ScanProjectResult) -> Vec<(String, LinkKind)> {
    result
        .links
        .iter()
        .map(|link| (link.relative.clone(), link.kind))
        .collect()
}

#[test]
fn file_links_and_out_of_root_directory_links_index_at_their_logical_paths() {
    let root = sandbox("basic");
    let project = root.join("proj");
    let outside = root.join("outside");
    touch(&project, "src/real/a.ts", "export const a = 1;");
    touch(&outside, "lib/out.ts", "export const out = 2;");
    touch(&outside, "lib/deep/inner.ts", "export const inner = 3;");
    if !link_file(&project.join("src/real/a.ts"), &project.join("src/afile.ts"))
        || !link_dir(&outside.join("lib"), &project.join("src/extlink"))
    {
        return;
    }

    let result = scan(&project);
    assert_eq!(
        result.files,
        strings(&[
            "src/afile.ts",
            "src/extlink/deep/inner.ts",
            "src/extlink/out.ts",
            "src/real/a.ts",
        ])
    );
    assert_eq!(
        link_summary(&result),
        vec![
            ("src/afile.ts".to_string(), LinkKind::File),
            ("src/extlink".to_string(), LinkKind::Dir),
        ]
    );
    let extlink = result
        .links
        .iter()
        .find(|link| link.relative == "src/extlink")
        .unwrap();
    assert_eq!(extlink.canonical, outside.join("lib").canonicalize().unwrap());
    assert_eq!(
        result.linked_dirs,
        strings(&["src/extlink", "src/extlink/deep"])
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn the_real_tree_wins_and_the_smallest_logical_path_breaks_ties() {
    let root = sandbox("precedence");
    let project = root.join("proj");
    let outside = root.join("outside");
    touch(&project, "src/real/a.ts", "export const a = 1;");
    touch(&outside, "x.ts", "export const x = 1;");
    // `src/alias` sorts before `src/real` but cannot displace the real tree;
    // `a/ext` and `b/ext` reach one target and `a/ext` sorts first.
    if !link_dir(&project.join("src/real"), &project.join("src/alias"))
        || !link_dir(&outside, &project.join("b/ext"))
        || !link_dir(&outside, &project.join("a/ext"))
    {
        return;
    }

    let result = scan(&project);
    assert_eq!(result.files, strings(&["a/ext/x.ts", "src/real/a.ts"]));
    assert_eq!(
        link_summary(&result),
        vec![("a/ext".to_string(), LinkKind::Dir)]
    );
    fs::remove_dir_all(&root).ok();
}

/// The round-1 review counterexample: `a -> A`, `z -> B`, `A/x -> B/sub`, and
/// a real `B/sub`. `B/sub` is one hop away through `z` but two through
/// `a/x`, so it is scanned as `z/sub`, and only once.
#[test]
fn fewer_symlink_hops_win_over_an_earlier_logical_path() {
    let root = sandbox("hops");
    let project = root.join("proj");
    let a = root.join("A");
    let b = root.join("B");
    fs::create_dir_all(&project).unwrap();
    touch(&a, "a.ts", "export const a = 1;");
    touch(&b, "sub/s.ts", "export const s = 1;");
    if !link_dir(&a, &project.join("a"))
        || !link_dir(&b, &project.join("z"))
        || !link_dir(&b.join("sub"), &a.join("x"))
    {
        return;
    }

    let result = scan(&project);
    assert_eq!(result.files, strings(&["a/a.ts", "z/sub/s.ts"]));
    assert_eq!(
        link_summary(&result),
        vec![
            ("a".to_string(), LinkKind::Dir),
            ("z".to_string(), LinkKind::Dir),
        ]
    );
    assert_eq!(result.linked_dirs, strings(&["a", "z", "z/sub"]));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn roots_ancestors_git_index_roots_and_broken_links_are_not_followed() {
    let root = sandbox("skipped");
    let project = root.join("proj");
    touch(&project, "src/app.ts", "export const app = 1;");
    touch(&project, ".codegraph/stray.ts", "export const stray = 1;");
    touch(&project, ".git/hooks/hook.ts", "export const hook = 1;");
    touch(&root, "sibling.ts", "export const sibling = 1;");
    if !link_dir(&project, &project.join("src/loop"))
        || !link_dir(&root, &project.join("up"))
        || !link_dir(&project.join(".codegraph"), &project.join("cg"))
        || !link_dir(&project.join(".git"), &project.join("gitdir"))
        || !link_dir(&project.join(".git/hooks"), &project.join("hooks"))
        || !link_file(&project.join(".git/hooks/hook.ts"), &project.join("hook.ts"))
        || !link_dir(&root.join("missing"), &project.join("broken"))
    {
        return;
    }

    let result = scan(&project);
    assert_eq!(result.files, strings(&["src/app.ts"]));
    assert!(result.links.is_empty(), "{:?}", result.links);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn ignore_rules_judge_a_link_at_its_logical_path_and_nested_links_follow() {
    let root = sandbox("rules");
    let project = root.join("proj");
    let outside = root.join("outside");
    let inner = root.join("inner");
    touch(&project, ".gitignore", "hidden/\n");
    touch(&outside, "o.ts", "export const o = 1;");
    touch(&inner, "i.ts", "export const i = 1;");
    if !link_dir(&outside, &project.join("node_modules"))
        || !link_dir(&outside, &project.join("hidden"))
        || !link_dir(&outside, &project.join("linked"))
        || !link_dir(&inner, &outside.join("deeper"))
    {
        return;
    }

    let result = scan(&project);
    assert_eq!(
        result.files,
        strings(&["linked/deeper/i.ts", "linked/o.ts"]),
        "an ignored link name or path is pruned; a link inside a followed target is followed"
    );
    assert_eq!(
        link_summary(&result),
        vec![
            ("linked".to_string(), LinkKind::Dir),
            ("linked/deeper".to_string(), LinkKind::Dir),
        ]
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn the_outcome_does_not_depend_on_link_creation_order() {
    let mut outcomes = Vec::new();
    for order in [[0, 1, 2], [2, 1, 0]] {
        let root = sandbox("order");
        let project = root.join("proj");
        let shared = root.join("shared");
        touch(&shared, "s.ts", "export const s = 1;");
        touch(&project, "m.ts", "export const m = 1;");
        let names = ["c/l", "a/l", "b/l"];
        for index in order {
            if !link_dir(&shared, &project.join(names[index])) {
                return;
            }
        }
        let result = scan(&project);
        let summary = link_summary(&result);
        outcomes.push((result.files, summary));
        fs::remove_dir_all(&root).ok();
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0].0, strings(&["a/l/s.ts", "m.ts"]));
}

#[test]
fn file_aliases_map_an_indexed_target_to_its_link_paths() {
    let root = sandbox("aliases");
    let project = root.join("proj");
    let outside = root.join("outside");
    touch(&project, "src/real/a.ts", "export const a = 1;");
    touch(&outside, "lib/b.ts", "export const b = 1;");
    touch(&root, "lonely/c.ts", "export const c = 1;");
    if !link_file(&project.join("src/real/a.ts"), &project.join("src/afile.ts"))
        || !link_file(&project.join("src/real/a.ts"), &project.join("other/a2.ts"))
        || !link_dir(&outside.join("lib"), &project.join("ext"))
        || !link_file(&outside.join("lib/b.ts"), &project.join("bfile.ts"))
        || !link_file(&root.join("lonely/c.ts"), &project.join("cfile.ts"))
    {
        return;
    }

    let result = scan(&project);
    let aliases: Vec<(String, Vec<String>)> = result
        .file_aliases
        .iter()
        .map(|(target, links)| (target.clone(), links.clone()))
        .collect();
    assert_eq!(
        aliases,
        vec![
            ("ext/b.ts".to_string(), strings(&["bfile.ts"])),
            (
                "src/real/a.ts".to_string(),
                strings(&["other/a2.ts", "src/afile.ts"])
            ),
        ],
        "a target outside every scanned directory has no alias entry"
    );
    assert!(result.files.contains(&"cfile.ts".to_string()));
    fs::remove_dir_all(&root).ok();
}

#[cfg(unix)]
#[test]
fn an_unreadable_link_target_is_skipped_without_failing_the_scan() {
    use std::os::unix::fs::PermissionsExt;

    let root = sandbox("unreadable");
    let project = root.join("proj");
    let locked = root.join("locked");
    touch(&project, "src/app.ts", "export const app = 1;");
    touch(&locked, "secret.ts", "export const secret = 1;");
    assert!(link_dir(&locked, &project.join("locked")));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        // Running as root: permissions do not apply, nothing to prove.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(&root).ok();
        return;
    }

    let result = scan(&project);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(result.files, strings(&["src/app.ts"]));
    assert!(result.links.is_empty());
    fs::remove_dir_all(&root).ok();
}
