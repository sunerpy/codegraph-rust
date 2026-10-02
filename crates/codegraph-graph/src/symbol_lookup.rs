//! How a typed symbol name resolves to nodes — a port of upstream
//! `src/graph/symbol-lookup.ts` (`matchesSymbol`, `lastQualifierPart`) and the
//! node half of `findAllSymbols` (`src/graph/named-symbol-flow.ts`), `v1.6.1`.
//!
//! Bare names are exact-name lookups (#1473: a mistyped name must not silently
//! resolve to the top fuzzy hit). Qualified names go through full-text search
//! and are kept only when [`matches_symbol`] accepts them: against the
//! qualified name under both separator conventions, then — for languages whose
//! hierarchy lives in the path (Rust modules, Python packages) — against the
//! file path.
//!
//! The MCP engine still carries its own, older copy of this matcher; the
//! viewer's flow search uses this one, and explore's Flow section (family F10)
//! is where the two meet.

use std::collections::HashSet;

use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_store::Store;

use crate::query::{SearchOptions, search_nodes};

/// Rust path prefixes that name no directory (`crate::x`, `super::y`).
pub const RUST_PATH_PREFIXES: &[&str] = &["crate", "super", "self"];

/// Does this query carry any scope qualifier at all?
pub fn is_qualified_symbol(symbol: &str) -> bool {
    symbol.contains(['.', '/']) || symbol.contains("::")
}

/// `fn/3` → `("fn", "3")`: Erlang's written arity, one to three digits.
fn split_arity(symbol: &str) -> Option<(&str, &str)> {
    let (base, arity) = symbol.rsplit_once('/')?;
    if base.is_empty()
        || !(1..=3).contains(&arity.len())
        || !arity.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((base, arity))
}

fn scope_parts(symbol: &str) -> Vec<&str> {
    symbol
        .split("::")
        .flat_map(|part| part.split(['.', '/']))
        .filter(|p| !p.is_empty())
        .collect()
}

/// The bare identifier at the end of a qualified query (arity spelling stripped).
pub fn last_qualifier_part(symbol: &str) -> &str {
    let base = split_arity(symbol).map(|(base, _)| base).unwrap_or(symbol);
    scope_parts(base).last().copied().unwrap_or(symbol)
}

/// Every scope separator rewritten to `.`, so a query and a stored qualified
/// name written in different conventions compare directly.
fn canonical_scope(text: &str) -> String {
    text.replace("::", ".").replace('/', ".")
}

/// `name.replace(/\.[^.]+$/, '')`: the last extension, when one follows the dot.
fn strip_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot + 1 < name.len() => &name[..dot],
        _ => name,
    }
}

/// Does `node` satisfy the user's symbol query?
pub fn matches_symbol(node: &Node, symbol: &str) -> bool {
    let mut symbol = symbol;
    // Erlang arity spelling: when the node's qualified name carries an arity
    // (#1610) the written arity must match, and the rest runs arity-less.
    if let Some((base, arity)) = split_arity(symbol)
        && let Some((_, node_arity)) = split_arity(&node.qualified_name)
    {
        if node_arity != arity {
            return false;
        }
        symbol = base;
    }

    if node.name == symbol {
        return true;
    }
    // File basename match ("product-card" matches "product-card.liquid").
    if node.kind == NodeKind::File && strip_extension(&node.name) == symbol {
        return true;
    }
    if !is_qualified_symbol(symbol) {
        return false;
    }
    let parts = scope_parts(symbol);
    if parts.len() < 2 {
        return false;
    }
    let last = parts[parts.len() - 1];
    if node.name != last {
        return false;
    }
    // Stage 1: qualified-name containment under the extractor's `::` convention.
    if node.qualified_name.contains(&parts.join("::")) {
        return true;
    }
    // Stage 1b: boundary-aligned suffix under a canonical separator, for
    // languages whose module names are themselves dotted.
    let canonical_query = canonical_scope(symbol);
    let canonical_node = canonical_scope(&node.qualified_name);
    if canonical_node == canonical_query || canonical_node.ends_with(&format!(".{canonical_query}"))
    {
        return true;
    }
    // Stage 2: file-path containment (Rust modules, Python packages).
    let hints: Vec<&str> = parts[..parts.len() - 1]
        .iter()
        .copied()
        .filter(|p| !RUST_PATH_PREFIXES.contains(p))
        .collect();
    if hints.is_empty() {
        return false;
    }
    let segments: Vec<&str> = node
        .file_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    hints.iter().all(|hint| {
        segments
            .iter()
            .any(|seg| seg == hint || strip_extension(seg) == *hint)
    })
}

/// A Nix option path: `xdg.configFile`, `launchd.user.agents`.
fn is_nix_option_path(symbol: &str) -> bool {
    // JavaScript's `\w` without the `u` flag: ASCII word characters only.
    let word_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '\'' || c == '-';
    let word = |s: &str| !s.is_empty() && s.chars().all(word_char);
    let mut parts = symbol.split('.');
    let Some(head) = parts.next() else {
        return false;
    };
    let mut head_chars = head.chars();
    let head_ok =
        head_chars.next().is_some_and(|c| c.is_ascii_lowercase()) && head_chars.all(word_char);
    let rest: Vec<&str> = parts.collect();
    head_ok && !rest.is_empty() && rest.iter().all(|p| word(p))
}

fn by_position(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Every node a symbol name names (`findAllSymbols`' node list): exact names
/// for a bare name, matcher-filtered search hits for a qualified one, and with
/// several matches the hand-written ones before generated scaffolds.
pub fn find_all_symbol_nodes(store: &Store, symbol: &str) -> rusqlite::Result<Vec<Node>> {
    // Nix option paths: the declaration is `options.<path>` and config writes
    // carry longer tails, so resolve the convention directly.
    if is_nix_option_path(symbol) {
        let mut hits = store.nodes_by_name(&format!("options.{symbol}"))?;
        by_position(&mut hits);
        let mut exact = store.nodes_by_name(symbol)?;
        by_position(&mut exact);
        hits.extend(exact);
        hits.extend(store.nodes_by_name_prefix(&format!("{symbol}."), 12)?);
        let mut seen = HashSet::new();
        let nix: Vec<Node> = hits
            .into_iter()
            .filter(|n| n.language == Language::Nix && seen.insert(n.id.clone()))
            .take(10)
            .collect();
        if !nix.is_empty() {
            return Ok(nix);
        }
    }

    let no_tokens = HashSet::new();
    let exact: Vec<Node> = if !is_qualified_symbol(symbol) {
        let mut nodes = store.nodes_by_name(symbol)?;
        by_position(&mut nodes);
        nodes
    } else {
        let options = SearchOptions {
            limit: Some(50),
            ..SearchOptions::default()
        };
        let mut results = search_nodes(store, symbol, &options, &no_tokens)?;
        if results.is_empty() {
            let tail = last_qualifier_part(symbol);
            if !tail.is_empty() && tail != symbol {
                results = search_nodes(store, tail, &options, &no_tokens)?;
            }
        }
        results
            .into_iter()
            .map(|r| r.node)
            .filter(|n| matches_symbol(n, symbol))
            .collect()
    };
    if exact.len() <= 1 {
        return Ok(exact);
    }
    // Same generated-file down-rank as the single-symbol lookup: the
    // hand-written implementations before the protobuf scaffold.
    let paths: Vec<String> = exact.iter().map(|n| n.file_path.clone()).collect();
    let flagged = store.generated_paths_among(&paths)?;
    let mut ranked = exact;
    ranked.sort_by_key(|n| flagged.contains(&n.file_path) || is_generated_file(&n.file_path));
    Ok(ranked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, qualified: &str, file: &str, kind: NodeKind) -> Node {
        Node {
            id: format!("{}:{name}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: qualified.to_string(),
            file_path: file.to_string(),
            language: Language::Rust,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    #[test]
    fn qualified_names_match_through_name_scope_or_path() {
        let run = node(
            "run",
            "stage_apply::run",
            "crates/x/src/stage_apply.rs",
            NodeKind::Function,
        );
        assert!(matches_symbol(&run, "run"));
        assert!(matches_symbol(&run, "stage_apply::run"));
        assert!(matches_symbol(&run, "crate::stage_apply::run"));
        assert!(!matches_symbol(&run, "other::run"));
        let elixir = node(
            "group",
            "AppWeb.Format::group",
            "lib/format.ex",
            NodeKind::Function,
        );
        assert!(matches_symbol(&elixir, "AppWeb.Format.group"));
        assert!(matches_symbol(&elixir, "Format.group"));
        assert!(!matches_symbol(&elixir, "Web.Format.group"));
        let file = node(
            "product-card.liquid",
            "",
            "snippets/product-card.liquid",
            NodeKind::File,
        );
        assert!(matches_symbol(&file, "product-card"));
    }

    #[test]
    fn erlang_arity_must_match_when_the_node_has_one() {
        let f = node("f", "mod::f/2", "src/mod.erl", NodeKind::Function);
        assert!(matches_symbol(&f, "f/2"));
        assert!(!matches_symbol(&f, "f/1"));
        assert_eq!(last_qualifier_part("mod::fn/2"), "fn");
        assert_eq!(last_qualifier_part("a.b"), "b");
        assert_eq!(last_qualifier_part("plain"), "plain");
    }

    #[test]
    fn nix_option_paths_are_recognised() {
        assert!(is_nix_option_path("xdg.configFile"));
        assert!(is_nix_option_path("launchd.user.agents"));
        assert!(!is_nix_option_path("Foo.bar"));
        assert!(!is_nix_option_path("plain"));
        assert!(!is_nix_option_path("a..b"));
    }
}
