//! The object-registry pass (upstream `objectRegistryEdges` at `e8ddc4e2`):
//! a registry maps keys to handlers in an object literal and dispatches on a
//! runtime key static parsing cannot follow, `this.commands = { add:
//! AddCommand }` then `new this.commands[command]().execute()`, so the
//! dispatching function links to each registered handler's callable entry
//! (`object-registry`). The registry and the dispatch are read from one
//! file's text.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use regex::Regex;
use serde_json::json;

use super::dispatch::synthesized;
use super::stage::{LineAt, View, enclosing_fn};
use crate::frameworks::js_scan::JS_SPACE;
use crate::strip_comments::strip_js_comments_keeping_offsets;

/// The fewest handler names an object literal registers (upstream
/// `REGISTRY_MIN_ENTRIES`).
const MIN_ENTRIES: usize = 2;
/// The handlers one dispatch site links at most (`REGISTRY_FANOUT_CAP`).
const FANOUT_CAP: usize = 40;
/// A class handler's entry methods (`REGISTRY_CLASS_ENTRY`).
const CLASS_ENTRY: [&str; 8] = [
    "execute", "run", "handle", "perform", "process", "call", "apply", "dispatch",
];
/// The average line length past which a file is a minified bundle.
const MINIFIED_LINE: f64 = 200.0;
/// The UTF-16 units after a dispatch site its chained method is read from.
const CHAIN_WINDOW: usize = 160;

/// An upstream pattern with JavaScript's `\s` ([`JS_SPACE`]) for the
/// `regex` crate's Unicode one.
fn js_regex(pattern: &str, what: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", JS_SPACE)).unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// The files the pass reads (`REGISTRY_JS_EXT`).
static JS_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(?:ts|tsx|js|jsx|mjs|cjs)$").expect("registry file"));
/// A computed member access by name, the dispatch shape: the cheap check a
/// file passes before it is scanned.
static COMPUTED_ACCESS: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"[A-Za-z0-9_$]\s*\[\s*[A-Za-z_$]", "registry prefilter"));
/// A dispatch site, `(new )?<ref>[<key>]` followed by a call or a chained
/// member (`REGISTRY_DISPATCH_SRC`); a quoted key is a static access.
static DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?:(?-u:\b)new\s+)?((?:this\.)?[A-Za-z_$][A-Za-z0-9_$]*)\s*\[\s*([A-Za-z_$][A-Za-z0-9_$.]*)\s*\]\s*(?:\(|\.[A-Za-z_$])",
        "registry dispatch",
    )
});
/// The method chained after a dispatch's call: `reg[k](…).run`.
static CHAINED_AFTER_CALL: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"\]\s*\([^)]*\)\s*\.\s*([A-Za-z_$][A-Za-z0-9_$]*)",
        "registry chain",
    )
});
/// The method chained right after the key: `reg[k].run`.
static CHAINED: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"\]\s*\.\s*([A-Za-z_$][A-Za-z0-9_$]*)", "registry member"));
/// An object literal assigned to a name (`REGISTRY_ASSIGN_RE`), the second
/// form held to [`name_starts_here`].
static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?:(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)|((?:this\.)?[A-Za-z_$][A-Za-z0-9_$]*))\s*=\s*\{",
        "registry assignment",
    )
});
/// One top-level `key: Identifier` entry of an object literal.
static ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r#"^\s*(?:\[[^\]]+\]|['"]?[A-Za-z0-9_$]+['"]?)\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*$"#,
        "registry entry",
    )
});

/// Object-literal registries (`objectRegistryEdges`): each function holding
/// `<ref>[<key>](…)` reaches the callable entry of every handler the same
/// file's object literal assigned to `<ref>` registers, at most
/// [`FANOUT_CAP`] a dispatch site.
pub(super) fn object_registry_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for file in view.files()? {
        if !JS_FILE.is_match(&file) {
            continue;
        }
        let Some(content) = view.text(&file) else {
            continue;
        };
        if content.is_empty() || !COMPUTED_ACCESS.is_match(&content) || minified(&content) {
            continue;
        }
        let safe = strip_js_comments_keeping_offsets(&content);
        let lines = LineAt::new(&safe, 1);

        let mut dispatches: Vec<(String, i64, Option<String>)> = Vec::new();
        for captures in DISPATCH.captures_iter(&safe) {
            let whole = captures.get(0).expect("match");
            let end = utf16_window_end(&content, whole.start(), CHAIN_WINDOW);
            let window = &safe[whole.start()..end];
            let chained = CHAINED_AFTER_CALL
                .captures(window)
                .or_else(|| CHAINED.captures(window))
                .map(|chain| chain[1].to_string());
            dispatches.push((captures[1].to_string(), lines.line(whole.start()), chained));
        }
        if dispatches.is_empty() {
            continue;
        }
        let refs: HashSet<&str> = dispatches
            .iter()
            .map(|(reference, _, _)| without_this(reference))
            .collect();

        // Each dispatched name's first object literal of at least two
        // handler names.
        let mut registries: HashMap<String, (Vec<String>, i64)> = HashMap::new();
        let mut at = 0;
        while let Some(captures) = ASSIGN.captures_at(&safe, at) {
            let whole = captures.get(0).expect("match");
            let name = match captures.get(1) {
                Some(declared) => declared,
                None => {
                    let assigned = captures.get(2).expect("assigned name");
                    if !name_starts_here(&safe, assigned.start()) {
                        at = next_char(&safe, whole.start());
                        continue;
                    }
                    assigned
                }
            };
            at = whole.end();
            let target = without_this(name.as_str());
            if !refs.contains(target) || registries.contains_key(target) {
                continue;
            }
            let Some(body) = brace_body(&safe, whole.end() - 1) else {
                continue;
            };
            let names = entry_names(body);
            if names.len() >= MIN_ENTRIES {
                registries.insert(target.to_string(), (names, lines.line(whole.start())));
            }
        }
        if registries.is_empty() {
            continue;
        }

        let nodes = view.nodes_in_file(&file)?;
        for (reference, line, chained) in &dispatches {
            let Some((names, registered)) = registries.get(without_this(reference)) else {
                continue;
            };
            let Some(dispatcher) = enclosing_fn(&nodes, *line) else {
                continue;
            };
            let mut added = 0;
            for name in names {
                if added >= FANOUT_CAP {
                    break;
                }
                let Some(target) = handler(view, name, chained.as_deref(), &file)? else {
                    continue;
                };
                if target.id == dispatcher.id
                    || !seen.insert((dispatcher.id.clone(), target.id.clone()))
                {
                    continue;
                }
                edges.push(synthesized(
                    dispatcher,
                    &target,
                    EdgeKind::Calls,
                    *line,
                    json!({
                        "synthesizedBy": "object-registry",
                        "via": name,
                        "registeredAt": format!("{file}:{registered}"),
                    }),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// Whether `content` reads as a minified bundle: more than [`MINIFIED_LINE`]
/// UTF-16 units a line on average, as upstream measures a string's length.
fn minified(content: &str) -> bool {
    let units: usize = content.chars().map(char::len_utf16).sum();
    let lines = content.bytes().filter(|&byte| byte == b'\n').count() + 1;
    units as f64 / lines as f64 > MINIFIED_LINE
}

/// The byte offset `units` UTF-16 units past `start` in `content`, or its
/// end (upstream slices the window in UTF-16 units; a character that would
/// cross the limit is left out, as a lone half of it matches nothing).
fn utf16_window_end(content: &str, start: usize, units: usize) -> usize {
    let mut taken = 0;
    for (offset, character) in content[start..].char_indices() {
        taken += character.len_utf16();
        if taken > units {
            return start + offset;
        }
    }
    content.len()
}

/// `name` without a leading `this.`, so a class field's registry
/// (`commands = {…}`) answers a `this.commands[k]` dispatch.
fn without_this(name: &str) -> &str {
    name.strip_prefix("this.").unwrap_or(name)
}

/// Whether an assigned name may start at `start` (upstream
/// `REGISTRY_NAME_START`): at a `this.`, or where the identifier characters
/// just before it are digits only, so never inside a name.
fn name_starts_here(text: &str, start: usize) -> bool {
    text[start..].starts_with("this.")
        || text[..start]
            .bytes()
            .rev()
            .take_while(|&byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$')
            .all(|byte| byte.is_ascii_digit())
}

/// The offset of the character after the one at `at`.
fn next_char(text: &str, at: usize) -> usize {
    at + text[at..].chars().next().map_or(1, char::len_utf8)
}

/// The text between the `{` at `open` and its matching `}` (`braceBody`):
/// braces counted only, nothing when they never balance.
fn brace_body(text: &str, open: usize) -> Option<&str> {
    let mut depth = 0usize;
    for (offset, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + 1..offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The handler names of an object literal's top-level `key: Identifier`
/// entries (`registryEntryNames`): segments split at depth-0 commas, a
/// value of at least three characters, each name once in source order.
fn entry_names(body: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut depth = 0i64;
    let mut start = 0;
    for (offset, byte) in body.bytes().enumerate() {
        match byte {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                segments.push(&body[start..offset]);
                start = offset + 1;
            }
            _ => {}
        }
    }
    segments.push(&body[start..]);
    let mut names: Vec<String> = Vec::new();
    for segment in segments {
        if let Some(captures) = ENTRY.captures(segment) {
            let name = &captures[1];
            if name.len() >= 3 && !names.iter().any(|known| known == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// The callable entry a registered `name` means (`resolveRegistryHandler`):
/// a function of that name; else a class or struct, entered at the method
/// the dispatch chains when it is an entry name, else its first entry
/// method, else its constructor, else the class; else a method of that
/// name. The first of those kinds that has a node of the name decides, and
/// within it the one of the registry's file, else the one of the project
/// (KEEP-RUST: an ambiguous name links nothing, where upstream takes the
/// first of `getNodesByName`).
fn handler(view: &View<'_>, name: &str, chained: Option<&str>, file: &str) -> Result<Option<Node>> {
    let candidates = view.nodes_named(name)?;
    let of_kinds = |kinds: &[NodeKind]| -> Vec<&Node> {
        candidates
            .iter()
            .filter(|node| kinds.contains(&node.kind))
            .collect()
    };
    let functions = of_kinds(&[NodeKind::Function]);
    if !functions.is_empty() {
        return Ok(unique(&functions, file).cloned());
    }
    let classes = of_kinds(&[NodeKind::Class, NodeKind::Struct]);
    if !classes.is_empty() {
        let Some(class) = unique(&classes, file) else {
            return Ok(None);
        };
        let methods: Vec<Node> = view
            .nodes_in_file(&class.file_path)?
            .into_iter()
            .filter(|node| {
                node.kind == NodeKind::Method
                    && node.start_line >= class.start_line
                    && node.start_line <= class.end_line
            })
            .collect();
        let wanted = chained.filter(|chained| CLASS_ENTRY.contains(chained));
        let entry = wanted
            .and_then(|wanted| methods.iter().find(|method| method.name == wanted))
            .or_else(|| {
                methods
                    .iter()
                    .find(|method| CLASS_ENTRY.contains(&method.name.as_str()))
            })
            .or_else(|| methods.iter().find(|method| method.name == "constructor"));
        return Ok(Some(entry.cloned().unwrap_or_else(|| class.clone())));
    }
    Ok(unique(&of_kinds(&[NodeKind::Method]), file).cloned())
}

/// The candidate of `file`, else the project's only one (W7-14a's emitter
/// rule).
fn unique<'n>(candidates: &[&'n Node], file: &str) -> Option<&'n Node> {
    let in_file: Vec<&Node> = candidates
        .iter()
        .copied()
        .filter(|node| node.file_path == file)
        .collect();
    match (in_file.len(), candidates.len()) {
        (1, _) => Some(in_file[0]),
        (0, 1) => Some(candidates[0]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_assigned_name_starts_only_at_an_identifiers_start() {
        assert!(name_starts_here("x = {", 0));
        assert!(name_starts_here("  reg = {", 2));
        assert!(name_starts_here("ab.this.reg = {", 3));
        assert!(!name_starts_here("abc = {", 1));
        assert!(!name_starts_here("a1c = {", 2));
        assert!(name_starts_here("12c = {", 2));
        assert!(name_starts_here(".c = {", 1));
    }

    #[test]
    fn entries_are_top_level_identifier_values() {
        assert_eq!(
            entry_names(
                " add: AddCommand, [Cmd.RM]: RemoveCommand, 'mv': MoveCommand, n: 5, f: () => g(a, b), o: { k: Inner }, ab: Ab, add2: AddCommand "
            ),
            ["AddCommand", "RemoveCommand", "MoveCommand"]
        );
        // A comma inside a computed key does not split the entry.
        assert_eq!(
            entry_names(" [a, b]: Handler, c: Other "),
            ["Handler", "Other"]
        );
        assert_eq!(entry_names(""), Vec::<String>::new());
    }

    #[test]
    fn a_brace_body_balances_braces_only() {
        assert_eq!(brace_body("x = { a: { b } }; y", 4), Some(" a: { b } "));
        assert_eq!(brace_body("x = { a", 4), None);
    }

    #[test]
    fn the_window_counts_utf16_units() {
        assert_eq!(utf16_window_end("abcdef", 1, 3), 4);
        assert_eq!(utf16_window_end("ab", 0, 5), 2);
        // `é` is one unit and two bytes; `😀` two units and four bytes.
        assert_eq!(utf16_window_end("aéb", 0, 2), 3);
        assert_eq!(utf16_window_end("a😀b", 0, 2), 1);
        assert_eq!(utf16_window_end("a😀b", 0, 3), 5);
    }

    #[test]
    fn a_minified_file_averages_over_200_units_a_line() {
        assert!(!minified(&"x".repeat(200)));
        assert!(minified(&"x".repeat(201)));
        assert!(!minified(&format!(
            "{}\n{}",
            "x".repeat(200),
            "x".repeat(199)
        )));
        // `😀` is two units: 100 of them on a line average 200.
        assert!(!minified(&"😀".repeat(100)));
        assert!(minified(&format!("{}x", "😀".repeat(100))));
    }

    #[test]
    fn whitespace_is_javascripts() {
        // U+FEFF is JavaScript whitespace; U+0085 is not, though Unicode's
        // White_Space holds it.
        assert!(ASSIGN.is_match("const\u{FEFF}reg = {"));
        assert!(!ASSIGN.is_match("const\u{85}reg\u{85}=\u{85}{"));
        assert!(DISPATCH.is_match("reg[\u{FEFF}k\u{FEFF}](x)"));
        assert!(!DISPATCH.is_match("reg[\u{85}k](x)"));
        assert_eq!(
            entry_names("a:\u{FEFF}Alpha, b\u{FEFF}: Beta"),
            ["Alpha", "Beta"]
        );
        assert_eq!(entry_names("a:\u{85}Alpha, b: Beta"), ["Beta"]);
    }
}
