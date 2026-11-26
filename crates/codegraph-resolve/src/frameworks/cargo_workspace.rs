//! A Cargo workspace's crates by name (upstream
//! `resolution/frameworks/cargo-workspace.ts` at `e8ddc4e2`, #151): the
//! root `Cargo.toml`'s `[workspace] members`, glob members expanded through
//! the project's directory listing, each named by its own manifest's
//! `[package] name` and by that name with `-` read as `_`. The Rust
//! resolver's module rule ([`super::rust`]) reads it to link a crate's name
//! to its `src/lib.rs` or `src/main.rs`.
//!
//! The manifests are read as upstream reads them, by lines and quotes, not
//! as TOML: the `[workspace]` section starts at a line that is exactly its
//! header and ends at the next line that is a `[section]` header (an
//! `[[array]]` header ends none); `members` is the first array a
//! `members =` opens in it, `default-members =` included; every quoted
//! value in the array counts, one in a comment included, and `exclude` is
//! not read. A glob member walks the project's directories from its static
//! prefix down to six levels below it, skipping `target`, `node_modules`,
//! `.git`, `dist`, `build` and every directory whose name starts with `.`.
//!
//! Rust's own (KNOWN_DIFFS, KEEP-RUST):
//!
//! - a member outside the project root (an absolute path, one that leaves
//!   the root once normalized, a glob whose static prefix does) is neither
//!   read nor listed; upstream reads it, and finds no indexed file there;
//! - a directory's subdirectories come sorted by name, where upstream's come
//!   in the filesystem's order;
//! - a name two members give (both their names, or one's name and the
//!   other's alias) names neither, where upstream's later member takes it;
//!   Cargo itself refuses two packages of one name;
//! - a glob reads `*`, `?`, `[…]` (`[!…]` and `[^…]` negated), `{a,b}` and
//!   `**`, the part of picomatch a Cargo workspace writes, and any other
//!   form (a leading `!`, an extglob, a group, an escape, a POSIX class, a
//!   brace without a comma) expands to nothing.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use super::js_scan::{JS_SPACE, is_js_space, js_trim};
use crate::pathutil;
use crate::types::ResolutionContext;

/// The crates of a workspace: each name and alias, and its member's
/// directory (project-relative, as the root manifest writes it once
/// cleaned).
pub type CrateMap = BTreeMap<String, String>;

/// The root manifest.
const ROOT_MANIFEST: &str = "Cargo.toml";

/// The characters that make a member a glob (upstream `GLOB_CHARS`).
const GLOB_CHARS: [char; 7] = ['*', '?', '[', ']', '{', '}', '!'];

/// The directories a glob's walk never enters (upstream `SKIP_DIRS`), with
/// every one whose name starts with `.`.
const SKIP_DIRS: [&str; 5] = ["target", "node_modules", ".git", "dist", "build"];

/// The deepest listing a glob's walk makes below its static prefix
/// (upstream `MAX_GLOB_WALK_DEPTH`): the walk lists the prefix at depth 0
/// and stops past this depth, so a match lies at most six levels below it.
const MAX_GLOB_WALK_DEPTH: usize = 5;

/// The most alternatives a glob's braces expand to; one with more expands
/// to nothing.
const MAX_ALTERNATIVES: usize = 256;

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, and its `\b`
/// is written `(?-u:\b)`.
fn manifest_regex(pattern: &str) -> Regex {
    Regex::new(&pattern.replace("{S}", JS_SPACE)).expect("cargo workspace pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| manifest_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// A line that heads a section (upstream `/^\[[^\]]+\]$/`, on the
    /// trimmed line).
    section_header = r"^\[[^\]]+\]$";
    /// The `members` key (upstream `\bmembers\b\s*=`).
    members_key = r"(?-u:\b)members(?-u:\b){S}*=";
    /// A package's name (upstream `/name\s*=\s*["']([^"'\n]+)["']/`).
    package_name_value = r#"name{S}*={S}*["']([^"'\n]+)["']"#;
}

/// The workspace's crates (upstream `getCargoWorkspaceCrateMap`): every
/// member's `[package] name` and its `_` alias, each mapped to the member's
/// directory. A project without a root manifest, or one naming no members,
/// has none.
pub fn crate_map<C: ResolutionContext + ?Sized>(context: &C) -> CrateMap {
    let mut crates = CrateMap::new();
    let Some(root) = context.read_file_shared(ROOT_MANIFEST) else {
        return crates;
    };
    let mut conflicts = BTreeSet::new();
    for member in expand_members(&workspace_members(&root), context) {
        let Some(manifest) = context.read_file_shared(&format!("{member}/{ROOT_MANIFEST}")) else {
            continue;
        };
        let Some(name) = package_name(&manifest) else {
            continue;
        };
        let alias = name.replace('-', "_");
        for key in [name, alias] {
            match crates.get(&key) {
                Some(dir) if *dir != member => {
                    conflicts.insert(key);
                }
                _ => {
                    crates.insert(key, member.clone());
                }
            }
        }
    }
    for key in conflicts {
        crates.remove(&key);
    }
    crates
}

/// The direct subdirectories of the project-relative directory `dir` under
/// `project_root`, sorted by name (upstream #151's `listDirectories`, which
/// a glob member's walk reads; `.` or `""` is the root). A directory that
/// leaves the root lexically, one that cannot be read, and a name that is
/// no UTF-8 list none; a symbolic link is no directory, as upstream's
/// `Dirent.isDirectory()` reads it.
pub(crate) fn list_directories_within(project_root: &str, dir: &str) -> Vec<String> {
    let Some(path) = pathutil::lexical_path_within_root(project_root, dir) else {
        return Vec::new();
    };
    list_directories_at(&path)
}

fn list_directories_at(path: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// The `[workspace] members` of the root manifest (upstream
/// `parseWorkspaceMembers`).
fn workspace_members(manifest: &str) -> Vec<String> {
    section(manifest, "workspace")
        .and_then(|section| members_array(&section))
        .map(|values| quoted_values(&values))
        .unwrap_or_default()
}

/// The lines of the section `name` heads (upstream `getSection`): from the
/// line after one that is exactly `[name]` once trimmed to the next section
/// header, joined; `None` when no line heads it.
fn section(content: &str, name: &str) -> Option<String> {
    let header = format!("[{name}]");
    let mut lines = content.split('\n');
    lines.by_ref().find(|line| js_trim(line) == header)?;
    let body: Vec<&str> = lines
        .take_while(|line| !section_header().is_match(js_trim(line)))
        .collect();
    Some(body.join("\n"))
}

/// The text inside the first array `members =` opens in `section`
/// (upstream `getArrayValue`): its brackets counted outside quotes, a
/// quote's `\` escaping the byte after it. `None` when no `[` follows the
/// key, or the array never closes.
fn members_array(section: &str) -> Option<String> {
    let key = members_key().find(section)?;
    let rest = section[key.end()..].trim_start_matches(is_js_space);
    let rest = rest.strip_prefix('[')?;
    let bytes = rest.as_bytes();
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut depth = 1;
    for (at, &byte) in bytes.iter().enumerate() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == open {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(rest[..at].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Every quoted value of an array's text (upstream `extractQuotedValues`):
/// either quote, `\` escaping the character after it, each value trimmed,
/// the empty ones dropped.
fn quoted_values(list: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut current = String::new();
    for ch in list.chars() {
        let Some(open) = quote else {
            if ch == '"' || ch == '\'' {
                quote = Some(ch);
                current.clear();
            }
            continue;
        };
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == open {
            values.push(js_trim(&current).to_string());
            quote = None;
            current.clear();
        } else {
            current.push(ch);
        }
    }
    values.retain(|value| !value.is_empty());
    values
}

/// A member manifest's `[package] name`, trimmed (upstream
/// `parsePackageName`); `None` for no `[package]` section, or no name in it.
fn package_name(manifest: &str) -> Option<String> {
    let section = section(manifest, "package")?;
    let found = package_name_value().captures(&section)?;
    let name = js_trim(&found[1]);
    (!name.is_empty()).then(|| name.to_string())
}

/// The members, globs expanded, each cleaned, those outside the root
/// dropped, the rest once each in the root manifest's order (upstream
/// `expandMembers`).
fn expand_members<C: ResolutionContext + ?Sized>(members: &[String], context: &C) -> Vec<String> {
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();
    for member in members {
        let candidates = if member.contains(GLOB_CHARS) {
            expand_glob(member, context)
        } else {
            vec![member.clone()]
        };
        for candidate in candidates {
            let cleaned = clean_path(&candidate);
            // An empty path is `/` (or `\\`) with its slash dropped: the
            // filesystem root, whose `/Cargo.toml` is outside the project.
            if cleaned.is_empty() || pathutil::lexical_relative(&cleaned).is_none() {
                continue;
            }
            if seen.insert(cleaned.clone()) {
                expanded.push(cleaned);
            }
        }
    }
    expanded
}

/// `\` read as `/`, one trailing `/` dropped (upstream `cleanPath`).
fn clean_path(member: &str) -> String {
    let member = member.replace('\\', "/");
    member.strip_suffix('/').unwrap_or(&member).to_string()
}

/// The directories a glob member matches (upstream `expandGlobMember`):
/// the walk from its static prefix (the text before its first glob
/// character, cut back to its last `/`) lists each directory's children,
/// depth first in the listing's order.
fn expand_glob<C: ResolutionContext + ?Sized>(member: &str, context: &C) -> Vec<String> {
    let first = member.find(GLOB_CHARS).unwrap_or(member.len());
    let before = &member[..first];
    let prefix = &before[..before.rfind('/').map_or(0, |at| at + 1)];
    // Checked before its trailing `/` goes: `/*`'s prefix is the
    // filesystem root, not the project's.
    if pathutil::lexical_relative(prefix).is_none() {
        return Vec::new();
    }
    let prefix = prefix.strip_suffix('/').unwrap_or(prefix);
    let start = if prefix.is_empty() { "." } else { prefix };
    let Some(matcher) = glob_matcher(member) else {
        return Vec::new();
    };
    let mut walk = Walk {
        context,
        matcher: &matcher,
        matches: Vec::new(),
        seen: HashSet::new(),
    };
    walk.visit(start, 0);
    walk.matches
}

struct Walk<'w, C: ?Sized> {
    context: &'w C,
    matcher: &'w Regex,
    matches: Vec<String>,
    seen: HashSet<String>,
}

impl<C: ResolutionContext + ?Sized> Walk<'_, C> {
    fn visit(&mut self, dir: &str, depth: usize) {
        if depth > MAX_GLOB_WALK_DEPTH {
            return;
        }
        for child in self.context.list_directories(dir) {
            if SKIP_DIRS.contains(&child.as_str()) || child.starts_with('.') {
                continue;
            }
            let rel = if dir == "." {
                child
            } else {
                format!("{dir}/{child}")
            };
            if self.matcher.is_match(&rel) && self.seen.insert(rel.clone()) {
                self.matches.push(rel.clone());
            }
            self.visit(&rel, depth + 1);
        }
    }
}

/// A glob member's matcher over project-relative directory paths, the
/// whole path matched; `None` for a form outside the supported part of
/// picomatch (see the module's notes).
fn glob_matcher(pattern: &str) -> Option<Regex> {
    if pattern.starts_with('!') || pattern.contains('\\') {
        return None;
    }
    let alternatives = expand_braces(pattern)?;
    let mut bodies = Vec::with_capacity(alternatives.len());
    for alternative in &alternatives {
        bodies.push(path_regex(alternative)?);
    }
    Regex::new(&format!("^(?:{})$", bodies.join("|"))).ok()
}

/// `pattern`'s `{a,b}` alternatives expanded, nested ones included; `None`
/// for a brace never closed or without a comma at its own level, or more
/// than [`MAX_ALTERNATIVES`] alternatives.
fn expand_braces(pattern: &str) -> Option<Vec<String>> {
    let Some(open) = pattern.find('{') else {
        return Some(vec![pattern.to_string()]);
    };
    let mut depth = 0usize;
    let mut cuts = Vec::new();
    let mut close = None;
    for (at, byte) in pattern.bytes().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(at);
                    break;
                }
            }
            b',' if depth == 1 => cuts.push(at),
            _ => {}
        }
    }
    let close = close?;
    if cuts.is_empty() {
        return None;
    }
    cuts.push(close);
    let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
    let mut out = Vec::new();
    let mut from = open + 1;
    for cut in cuts {
        for expanded in expand_braces(&format!("{head}{}{tail}", &pattern[from..cut]))? {
            out.push(expanded);
            if out.len() > MAX_ALTERNATIVES {
                return None;
            }
        }
        from = cut + 1;
    }
    Some(out)
}

/// The regex body of a brace-free glob over `/`-separated segments: a `**`
/// segment is any number of whole segments, any other segment one.
fn path_regex(pattern: &str) -> Option<String> {
    let mut units: Vec<Option<String>> = Vec::new();
    for segment in pattern.split('/') {
        if segment == "**" {
            if units.last().is_some_and(Option::is_none) {
                continue;
            }
            units.push(None);
        } else {
            units.push(Some(segment_regex(segment)?));
        }
    }
    let last = units.len().saturating_sub(1);
    let mut out = String::new();
    for (at, unit) in units.iter().enumerate() {
        match unit {
            Some(segment) => {
                if at > 0 && units[at - 1].is_some() {
                    out.push('/');
                }
                out.push_str(segment);
            }
            None if units.len() == 1 => out.push_str("[^/]+(?:/[^/]+)*"),
            None if at == 0 => out.push_str("(?:[^/]+/)*"),
            None if at == last => out.push_str("(?:/[^/]+)*"),
            None => out.push_str("(?:/[^/]+)*/"),
        }
    }
    Some(out)
}

/// One segment's regex: `*` any characters, `?` one, `[…]` one of a class,
/// anything else itself; `None` for an extglob (`@(`, `!(`, `+(`, `*(`,
/// `?(`), a group or a class this reading does not take.
fn segment_regex(segment: &str) -> Option<String> {
    let chars: Vec<char> = segment.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        let ch = chars[at];
        if matches!(ch, '@' | '!' | '+' | '*' | '?') && chars.get(at + 1) == Some(&'(') {
            return None;
        }
        match ch {
            '*' => {
                while chars.get(at + 1) == Some(&'*') {
                    at += 1;
                }
                if chars.get(at + 1) == Some(&'(') {
                    return None;
                }
                out.push_str("[^/]*");
            }
            '?' => out.push_str("[^/]"),
            '[' => {
                let (class, next) = class_regex(&chars, at)?;
                out.push_str(&class);
                at = next;
                continue;
            }
            '(' | ')' | '|' => return None,
            _ => out.push_str(&regex::escape(&ch.to_string())),
        }
        at += 1;
    }
    Some(out)
}

/// The class opening at `chars[open]`, as a regex class, and the index
/// past its `]`: `!` or `^` first negates it, a `]` right after that is a
/// member, `a-z` is a range. `None` for one never closed, or holding a
/// POSIX class.
fn class_regex(chars: &[char], open: usize) -> Option<(String, usize)> {
    let mut at = open + 1;
    let negated = matches!(chars.get(at), Some('!' | '^'));
    if negated {
        at += 1;
    }
    let mut members: Vec<char> = Vec::new();
    loop {
        let ch = *chars.get(at)?;
        if ch == ']' && !members.is_empty() {
            break;
        }
        if ch == '[' && chars.get(at + 1) == Some(&':') {
            return None;
        }
        members.push(ch);
        at += 1;
    }
    let mut class = String::from(if negated { "[^/" } else { "[" });
    let mut index = 0;
    while index < members.len() {
        let low = members[index];
        if members.get(index + 1) == Some(&'-') && index + 2 < members.len() {
            let high = members[index + 2];
            if low > high {
                return None;
            }
            class.push_str(&format!("\\x{{{:X}}}-\\x{{{:X}}}", low as u32, high as u32));
            index += 3;
        } else {
            class.push_str(&format!("\\x{{{:X}}}", low as u32));
            index += 1;
        }
    }
    class.push(']');
    Some((class, at + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn package(name: &str) -> String {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n")
    }

    fn crates_of(ctx: &RouteCtx) -> Vec<(String, String)> {
        crate_map(ctx).into_iter().collect()
    }

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, dir)| (key.to_string(), dir.to_string()))
            .collect()
    }

    /// A workspace whose root manifest is `root` and whose members under
    /// `crates/` are `members`, each `(directory, package name)`.
    fn workspace(root: &str, members: &[(&str, &str)]) -> RouteCtx {
        members.iter().fold(
            RouteCtx::default().unlisted("Cargo.toml", root),
            |ctx, (dir, name)| ctx.unlisted(&format!("{dir}/Cargo.toml"), &package(name)),
        )
    }

    // -- Upstream frameworks.test.ts 1468-1772 ---------------------------

    #[test]
    fn a_member_names_its_crate_and_its_underscore_alias() {
        let ctx = workspace(
            "\n[workspace]\nmembers = [\"crates/mytool-core\", \"crates/mytool-fetcher\"]\n",
            &[("crates/mytool-core", "mytool-core")],
        );
        // The fetcher has no manifest: it names no crate.
        assert_eq!(
            crates_of(&ctx),
            entries(&[
                ("mytool-core", "crates/mytool-core"),
                ("mytool_core", "crates/mytool-core"),
            ])
        );
    }

    #[test]
    fn a_glob_member_expands_through_the_directory_listing() {
        let ctx = workspace(
            "\n[workspace]\nmembers = [\"crates/*\"]\n",
            &[
                ("crates/mytool-foo", "mytool-foo"),
                ("crates/mytool-bar", "mytool-bar"),
            ],
        )
        .dirs(".", &["crates"])
        .dirs("crates", &["mytool-foo", "mytool-bar"])
        .dirs("crates/mytool-foo", &["src"])
        .dirs("crates/mytool-bar", &["src"]);
        assert_eq!(
            crates_of(&ctx),
            entries(&[
                ("mytool-bar", "crates/mytool-bar"),
                ("mytool-foo", "crates/mytool-foo"),
                ("mytool_bar", "crates/mytool-bar"),
                ("mytool_foo", "crates/mytool-foo"),
            ])
        );
        // The walk starts at the glob's static prefix.
        assert_eq!(ctx.listed().first().map(String::as_str), Some("crates"));
    }

    #[test]
    fn a_name_glob_matches_at_the_root() {
        let ctx = workspace(
            "\n[workspace]\nmembers = [\"helix-*\"]\n",
            &[("helix-core", "helix-core")],
        )
        .dirs(".", &["helix-core", "docs", "target"])
        .dirs("helix-core", &["src"]);
        assert_eq!(
            crates_of(&ctx),
            entries(&[("helix-core", "helix-core"), ("helix_core", "helix-core")])
        );
    }

    // -- The manifest as upstream reads it --------------------------------

    #[test]
    fn the_first_members_key_is_read_even_when_it_is_default_members() {
        let ctx = workspace(
            "[workspace]\ndefault-members = [\"crates/a\"]\nmembers = [\"crates/a\", \"crates/b\"]\n",
            &[("crates/a", "alpha"), ("crates/b", "beta")],
        );
        assert_eq!(crates_of(&ctx), entries(&[("alpha", "crates/a")]));
    }

    #[test]
    fn the_workspace_section_opens_on_its_bare_header_and_ends_at_the_next() {
        let members = [("crates/a", "alpha")];
        // A header followed by a comment opens no section.
        let commented = workspace("[workspace] # c\nmembers = [\"crates/a\"]\n", &members);
        assert_eq!(crates_of(&commented), []);
        // Another table's header ends it.
        let ended = workspace(
            "[workspace]\n[workspace.package]\nmembers = [\"crates/a\"]\n",
            &members,
        );
        assert_eq!(crates_of(&ended), []);
        // An array-of-tables header does not.
        let array = workspace("[workspace]\n[[bin]]\nmembers = [\"crates/a\"]\n", &members);
        assert_eq!(crates_of(&array), entries(&[("alpha", "crates/a")]));
        // Padded headers and CRLF line ends count.
        let padded = workspace(
            "  [workspace]  \r\nmembers = [\r\n  \"crates/a\",\r\n]\r\n",
            &members,
        );
        assert_eq!(crates_of(&padded), entries(&[("alpha", "crates/a")]));
        // The root's own package names no crate.
        let package_only = RouteCtx::default().unlisted("Cargo.toml", &package("root"));
        assert_eq!(crates_of(&package_only), []);
    }

    #[test]
    fn exclude_is_not_read() {
        let ctx = workspace(
            "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/skip\"]\n",
            &[("crates/keep", "keep"), ("crates/skip", "skip")],
        )
        .dirs("crates", &["keep", "skip"]);
        assert_eq!(
            crates_of(&ctx),
            entries(&[("keep", "crates/keep"), ("skip", "crates/skip")])
        );
    }

    #[test]
    fn every_quoted_value_counts_and_an_empty_one_drops() {
        let ctx = workspace(
            "[workspace]\nmembers = [\n  \"crates/a\", # \"crates/b\"\n  \"\",\n  ' crates/c ',\n  \"crates/[d]\",\n]\n",
            &[
                ("crates/a", "alpha"),
                ("crates/b", "beta"),
                ("crates/c", "gamma"),
            ],
        );
        // `crates/[d]` is a glob whose listing holds nothing.
        assert_eq!(
            crates_of(&ctx),
            entries(&[
                ("alpha", "crates/a"),
                ("beta", "crates/b"),
                ("gamma", "crates/c")
            ])
        );
    }

    /// A bracket inside a quoted value closes no array (upstream
    /// `getArrayValue` counts brackets outside quotes only).
    #[test]
    fn a_bracket_inside_a_quoted_member_closes_no_array() {
        let ctx = workspace(
            "[workspace]\nmembers = [\"crates/a\", \"odd]name\", 'x]', \"crates/b\"]\n",
            &[("crates/a", "alpha"), ("crates/b", "beta")],
        );
        assert_eq!(
            crates_of(&ctx),
            entries(&[("alpha", "crates/a"), ("beta", "crates/b")])
        );
    }

    #[test]
    fn a_member_path_is_cleaned_and_deduplicated() {
        // `\\` escapes a backslash, which reads as `/`; one trailing `/` goes.
        let ctx = workspace(
            "[workspace]\nmembers = [\"crates\\\\a\", \"crates/a/\", \"crates/a\"]\n",
            &[("crates/a", "alpha")],
        );
        assert_eq!(crates_of(&ctx), entries(&[("alpha", "crates/a")]));
        assert_eq!(
            ctx.reads()
                .iter()
                .filter(|read| read.as_str() == "crates/a/Cargo.toml")
                .count(),
            1,
            "{:?}",
            ctx.reads()
        );
    }

    /// Each cleaning on its own, with no sibling member naming the crate:
    /// `\\` escapes the backslash, which reads as `/`, and one trailing `/`
    /// goes.
    #[test]
    fn each_member_form_is_cleaned_on_its_own() {
        for members in ["[\"crates\\\\a\"]", "[\"crates/a/\"]", "['crates\\\\a/']"] {
            let ctx = workspace(
                &format!("[workspace]\nmembers = {members}\n"),
                &[("crates/a", "alpha")],
            );
            assert_eq!(
                crates_of(&ctx),
                entries(&[("alpha", "crates/a")]),
                "{members}"
            );
        }
    }

    #[test]
    fn the_package_name_is_the_first_name_of_its_section() {
        let ctx = RouteCtx::default()
            .unlisted(
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/a\", \"crates/b\", \"crates/c\"]\n",
            )
            .unlisted(
                "crates/a/Cargo.toml",
                "[lib]\nname = \"lib_a\"\n\n[package]\nname = ' spaced '\n",
            )
            .unlisted("crates/b/Cargo.toml", "[lib]\nname = \"only_lib\"\n")
            .unlisted("crates/c/Cargo.toml", "[package]\nversion = \"1\"\n");
        // `[lib] name` names no crate; a member without a package name is
        // none.
        assert_eq!(crates_of(&ctx), entries(&[("spaced", "crates/a")]));
    }

    // -- Glob members -----------------------------------------------------

    #[test]
    fn a_glob_walks_six_levels_below_its_prefix() {
        let ctx = workspace(
            "[workspace]\nmembers = [\"crates/**\"]\n",
            &[
                ("crates/a/b/c/d/e/f", "six"),
                ("crates/a/b/c/d/e/f/g", "seven"),
            ],
        )
        .dirs("crates", &["a"])
        .dirs("crates/a", &["b"])
        .dirs("crates/a/b", &["c"])
        .dirs("crates/a/b/c", &["d"])
        .dirs("crates/a/b/c/d", &["e"])
        .dirs("crates/a/b/c/d/e", &["f"])
        .dirs("crates/a/b/c/d/e/f", &["g"]);
        assert_eq!(crates_of(&ctx), entries(&[("six", "crates/a/b/c/d/e/f")]));
        assert!(
            !ctx.listed().contains(&"crates/a/b/c/d/e/f".to_string()),
            "{:?}",
            ctx.listed()
        );
    }

    #[test]
    fn a_glob_walk_skips_build_and_hidden_directories() {
        let skipped = ["target", "node_modules", ".git", "dist", "build", ".cache"];
        let mut ctx = workspace(
            "[workspace]\nmembers = [\"*\", \"*/*\"]\n",
            &[
                ("a", "a"),
                ("target", "t"),
                ("node_modules", "n"),
                (".git", "g"),
                ("dist", "d"),
                ("build", "b"),
                (".cache", "c"),
                ("target/x", "tx"),
                ("a/x", "ax"),
            ],
        )
        .dirs(
            ".",
            &[
                "a",
                "target",
                "node_modules",
                ".git",
                "dist",
                "build",
                ".cache",
            ],
        )
        .dirs("a", &["x"]);
        for dir in skipped {
            ctx = ctx.dirs(dir, &["x"]);
        }
        assert_eq!(crates_of(&ctx), entries(&[("a", "a"), ("ax", "a/x")]));
        for dir in skipped {
            assert!(
                !ctx.listed().iter().any(|listed| listed == dir),
                "{dir} listed: {:?}",
                ctx.listed()
            );
        }
    }

    #[test]
    fn a_glob_reads_the_picomatch_forms_cargo_writes() {
        let members = [("crates/a", "pa"), ("crates/b", "pb"), ("crates/cc", "pc")];
        let crates = |pattern: &str| {
            let ctx = workspace(
                &format!("[workspace]\nmembers = [\"{pattern}\"]\n"),
                &members,
            )
            .dirs(".", &["crates"])
            .dirs("crates", &["a", "b", "cc"]);
            crate_map(&ctx).into_keys().collect::<Vec<_>>()
        };
        assert_eq!(crates("crates/*"), ["pa", "pb", "pc"]);
        assert_eq!(crates("crates/?"), ["pa", "pb"]);
        assert_eq!(crates("crates/{a,cc}"), ["pa", "pc"]);
        assert_eq!(crates("crates/[ab]"), ["pa", "pb"]);
        assert_eq!(crates("crates/[!a]"), ["pb"]);
        assert_eq!(crates("crates/[^a]*"), ["pb", "pc"]);
        assert_eq!(crates("crates/c*"), ["pc"]);
        assert_eq!(crates("**/a"), ["pa"]);
        assert_eq!(crates("crates/**"), ["pa", "pb", "pc"]);
        // Every other form expands to nothing: a leading `!`, an extglob,
        // a group, an escape, a POSIX class, a brace without a comma.
        for unsupported in [
            "!crates/a",
            "crates/*(a)",
            "crates/?(a|b)",
            "crates/(a|b)*",
            "crates/\\\\*",
            "crates/[[:alpha:]]",
            "crates/{a}",
            "crates/{a,b",
        ] {
            assert_eq!(crates(unsupported), Vec::<String>::new(), "{unsupported}");
        }
    }

    // -- Rust's own (KEEP-RUST) -------------------------------------------

    #[test]
    fn a_member_outside_the_root_is_neither_read_nor_listed() {
        let ctx = RouteCtx::default()
            .unlisted(
                "Cargo.toml",
                "[workspace]\nmembers = [\"../x\", \"/abs/y\", \"crates/../../z\", \"C:/w\", \"../g/*\", \"/abs/*\", \"/\", \"\\\\\", \"/*\"]\n",
            )
            .unlisted("../x/Cargo.toml", &package("x"))
            .unlisted("/abs/y/Cargo.toml", &package("y"))
            .unlisted("../z/Cargo.toml", &package("z"))
            .unlisted("crates/../../z/Cargo.toml", &package("z"))
            .unlisted("C:/w/Cargo.toml", &package("w"))
            // `/`, and `\\` read as `/`, are the filesystem root.
            .unlisted("/Cargo.toml", &package("root"))
            .dirs(".", &["inner"])
            .dirs("../g", &["inner"])
            .dirs("/abs", &["inner"]);
        assert_eq!(crates_of(&ctx), []);
        assert_eq!(ctx.reads(), ["Cargo.toml"]);
        assert_eq!(ctx.listed(), Vec::<String>::new());
    }

    #[test]
    fn a_name_two_members_share_names_neither() {
        let ctx = workspace(
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\", \"crates/c\", \"crates/d\"]\n",
            &[
                ("crates/a", "dup"),
                ("crates/b", "dup"),
                ("crates/c", "x-y"),
                ("crates/d", "x_y"),
            ],
        );
        // `x_y` is `crates/c`'s alias and `crates/d`'s name.
        assert_eq!(crates_of(&ctx), entries(&[("x-y", "crates/c")]));
    }

    #[test]
    fn a_missing_or_empty_root_manifest_names_no_crate() {
        assert_eq!(crates_of(&RouteCtx::default()), []);
        assert_eq!(
            crates_of(&RouteCtx::default().unlisted("Cargo.toml", "")),
            []
        );
        // A `members` key that is no array, or an array never closed.
        for root in [
            "[workspace]\nmembers = \"crates/a\"\n",
            "[workspace]\nmembers = [\"crates/a\"\n",
        ] {
            let ctx = workspace(root, &[("crates/a", "alpha")]);
            assert_eq!(crates_of(&ctx), [], "{root:?}");
        }
        // The array closed, the member names its crate.
        let ctx = workspace(
            "[workspace]\nmembers = [\"crates/a\"]\n",
            &[("crates/a", "alpha")],
        );
        assert_eq!(crates_of(&ctx), entries(&[("alpha", "crates/a")]));
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "cg-cargo-workspace-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create the temp dir");
        dir
    }

    /// A directory's subdirectories come sorted by name, symlinks and files
    /// left out; a directory outside the root, or one that cannot be read,
    /// lists none.
    #[test]
    fn a_directory_lists_its_subdirectories_sorted_within_the_root() {
        let root = temp_dir("list");
        let names: Vec<String> = (0..24).map(|at| format!("d{:02}", (at * 7) % 24)).collect();
        for name in &names {
            std::fs::create_dir_all(root.join("crates").join(name)).unwrap();
        }
        std::fs::write(root.join("crates/file.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("crates/d00"), root.join("crates/link")).unwrap();
        let root_str = root.to_str().unwrap();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(list_directories_within(root_str, "crates"), sorted);
        assert_eq!(list_directories_within(root_str, "."), ["crates"]);
        assert_eq!(list_directories_within(root_str, ""), ["crates"]);
        assert_eq!(
            list_directories_within(root_str, "missing"),
            Vec::<String>::new()
        );
        assert_eq!(
            list_directories_within(root_str, "crates/file.txt"),
            Vec::<String>::new()
        );
        let inner = root.join("crates/d01");
        let inner_str = inner.to_str().unwrap();
        assert_eq!(
            list_directories_within(inner_str, ".."),
            Vec::<String>::new()
        );
        assert_eq!(
            list_directories_within(inner_str, root_str),
            Vec::<String>::new()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
