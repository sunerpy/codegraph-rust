//! `GET /api/entrypoints?limit=&routes=` — where to start reading a project:
//! its routes, the files that run something at their top level, the test
//! suites that reach furthest, and the symbols most of the code depends on.
//! Upstream `src/ui-server/api/entrypoints.ts`.
//!
//! Every list is ranked by the index alone and carries a total that is a floor:
//! the server counts what its scan saw, never more. Memoised on the index
//! revision, since the four scans are the most expensive thing Start asks for.

use std::collections::HashMap;
use std::time::Instant;

use codegraph_core::file_class::is_generated_file;
use codegraph_core::generated_header::is_js_whitespace;
use codegraph_core::types::{EdgeKind, Node, NodeKind};
use codegraph_store::TopCallingFile;
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::routes::{WireRoute, build_routes};
use super::source::read_indexed_file_text;
use super::stats::frameworks;
use super::wire::{
    WireList, WireNodeRef, is_test_file, is_test_path, locale_compare, to_node_ref, to_posix_path,
    wire_list,
};
use crate::respond::{ApiResult, Query};

const DEFAULT_LIMIT: i64 = 12;
const DEFAULT_ROUTE_LIMIT: i64 = 60;
const MAX_ROUTE_LIMIT: i64 = 300;
/// Ranked rows each scan reads before filtering.
const SCAN_ROWS: i64 = 400;
/// At most this many executable files from one directory, so one busy folder
/// cannot fill the list.
const MAX_FILES_PER_DIR: usize = 2;
/// Test paths per reach query.
const TEST_CHUNK: usize = 500;
/// A Swift file read for its `#Preview` blocks, at most.
const MAX_PREVIEW_SCAN: u64 = 1024 * 1024;

/// Kinds that are bookkeeping rather than something to read first.
fn is_non_hub_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File | NodeKind::Import | NodeKind::Export | NodeKind::Parameter
    )
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireEntryFile {
    #[serde(flatten)]
    node: WireNodeRef,
    /// Calls and instantiations made at the top level of the file.
    calls: i64,
    /// Distinct other files this one's symbols reach.
    reaches: i64,
    /// Other files reaching into this one. Zero means nothing imports it.
    dependents: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireEntryTest {
    #[serde(flatten)]
    node: WireNodeRef,
    /// Distinct other files this test reaches — what it exercises.
    reaches: i64,
    /// References behind that reach.
    refs: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireEntryHub {
    #[serde(flatten)]
    node: WireNodeRef,
    /// Distinct symbols that depend on this one.
    dependents: i64,
}

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let started = Instant::now();
    let limit = query.int("limit", 1, 50, Some(DEFAULT_LIMIT))?;
    let route_limit = query.int("routes", 3, MAX_ROUTE_LIMIT, Some(DEFAULT_ROUTE_LIMIT))?;

    let counts = ctx.store.counts()?;
    let last_indexed_at = ctx.store.last_indexed_at()?;
    // JSON rather than a joined string: a project root can contain any
    // character a separator might have picked, and this key is compared for
    // equality only.
    let key = json!([
        ctx.project_root().display().to_string(),
        last_indexed_at.unwrap_or(0),
        counts.edge_count,
        counts.file_count,
        limit,
        route_limit,
    ])
    .to_string();
    if let Some(mut hit) = ctx.state.caches.entrypoints.get(&key) {
        // Re-stamp rather than mutate the shared body.
        hit["timing"] =
            json!({ "elapsedMs": started.elapsed().as_millis() as u64, "cached": true });
        return Ok(hit);
    }

    let mut payload = json!({
        "frameworks": frameworks(ctx)?,
        "routes": route_entries(ctx, route_limit)?,
        "files": executable_files(ctx, limit as usize)?,
        "tests": test_files(ctx, limit as usize)?,
        "hubs": hubs(ctx, limit as usize)?,
        "index": { "lastIndexedAt": last_indexed_at, "files": counts.file_count },
    });
    payload["timing"] =
        json!({ "elapsedMs": started.elapsed().as_millis() as u64, "cached": false });
    ctx.state.caches.entrypoints.put(key, payload.clone());
    Ok(payload)
}

fn route_entries(ctx: &Ctx<'_>, limit: i64) -> ApiResult<Value> {
    let manifest = build_routes(ctx, limit)?;
    // `shown` counts the rows; `truncated` is the manifest's own verdict on
    // whether the window cut anything, more trustworthy than comparing against
    // `routeCount` (which also counts URLs whose handler never resolved).
    let total = if manifest.truncated {
        (manifest.shown as i64 + 1).max(manifest.route_count) as usize
    } else {
        manifest.shown
    };
    let items: WireList<WireRoute> = WireList {
        total,
        shown: manifest.shown,
        truncated: manifest.truncated,
        items: manifest.entries,
    };
    Ok(json!({
        "routed": manifest.routed,
        "routeCount": manifest.route_count,
        "items": items,
    }))
}

fn executable_files(ctx: &Ctx<'_>, limit: usize) -> ApiResult<WireList<WireEntryFile>> {
    let ranked = without_previews(ctx, ctx.store.top_calling_files(SCAN_ROWS)?)?;
    let ids: Vec<String> = ranked.iter().map(|r| r.node_id.clone()).collect();
    let nodes = ctx.store.nodes_by_ids(&ids)?;

    let mut kept: Vec<(Node, i64, i64)> = Vec::new();
    let mut per_dir: HashMap<String, usize> = HashMap::new();
    let mut eligible = 0;
    for row in &ranked {
        if is_test_file(&row.file_path) {
            continue;
        }
        eligible += 1;
        if kept.len() >= limit {
            continue;
        }
        let dir = directory_of(&row.file_path);
        let taken = per_dir.get(&dir).copied().unwrap_or(0);
        if taken >= MAX_FILES_PER_DIR {
            continue;
        }
        let Some(node) = nodes.get(&row.node_id) else {
            continue;
        };
        per_dir.insert(dir, taken + 1);
        kept.push((node.clone(), row.calls, row.reaches));
    }

    let paths: Vec<String> = kept.iter().map(|(n, _, _)| n.file_path.clone()).collect();
    let dependents: HashMap<String, i64> = ctx
        .store
        .file_dependent_counts(&paths)?
        .into_iter()
        .collect();
    let items: Vec<WireEntryFile> = kept
        .into_iter()
        .map(|(node, calls, reaches)| WireEntryFile {
            dependents: dependents.get(&node.file_path).copied().unwrap_or(0),
            node: to_node_ref(&node),
            calls,
            reaches,
        })
        .collect();
    // `eligible` counts every non-test file the scan saw: a floor, never an
    // overstatement.
    let total = eligible.max(items.len());
    Ok(wire_list(items, total))
}

/// The ranking with a Swift file's `#Preview { … }` calls taken out (upstream
/// `withoutPreviews`, #2109).
///
/// A preview sits at the top level of a SwiftUI view's file, so its body's
/// constructions are module-level edges out of the file — and every view with
/// a preview ranked as "a file that runs something". A preview is Xcode's
/// canvas, never code the app runs. A file whose only module-level calls are
/// previews leaves the list; the rest are re-ranked on what is left.
fn without_previews(ctx: &Ctx<'_>, rows: Vec<TopCallingFile>) -> ApiResult<Vec<TopCallingFile>> {
    let mut adjusted = false;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if !row.file_path.ends_with(".swift") {
            out.push(row);
            continue;
        }
        let spans = read_indexed_file_text(ctx, &row.file_path, MAX_PREVIEW_SCAN)
            .filter(|text| text.contains("#Preview"))
            .map(|text| swift_preview_spans(&text))
            .unwrap_or_default();
        if spans.is_empty() {
            out.push(row);
            continue;
        }
        let in_preview = ctx
            .store
            .outgoing_edges_from(
                std::slice::from_ref(&row.node_id),
                &[EdgeKind::Calls, EdgeKind::Instantiates],
            )?
            .iter()
            .filter(|edge| {
                edge.line
                    .is_some_and(|line| spans.iter().any(|&(from, to)| line >= from && line <= to))
            })
            .count() as i64;
        if in_preview == 0 {
            out.push(row);
            continue;
        }
        adjusted = true;
        let calls = row.calls - in_preview;
        if calls > 0 {
            out.push(TopCallingFile {
                calls,
                score: calls * (1 + row.reaches),
                ..row
            });
        }
    }
    if adjusted {
        // The scan's own order: score, then calls, then path.
        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(b.calls.cmp(&a.calls))
                .then_with(|| a.file_path.cmp(&b.file_path))
        });
    }
    Ok(out)
}

/// The line spans (1-based, inclusive) of a Swift file's top-level
/// `#Preview { … }` macros, trailing closures included — WidgetKit's
/// `#Preview(as: .systemSmall) { … } timeline: { … }` is one preview.
fn swift_preview_spans(text: &str) -> Vec<(i64, i64)> {
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !opens_preview(lines[i]) {
            i += 1;
            continue;
        }
        let mut depth = 0i64;
        let mut opened = false;
        let mut end = None;
        'scan: for (j, line) in lines.iter().enumerate().skip(i) {
            for (c, ch) in line.char_indices() {
                if ch == '{' {
                    depth += 1;
                    opened = true;
                } else if ch == '}' && opened {
                    depth -= 1;
                    if depth == 0 {
                        // Another trailing closure (`} timeline: {`) continues the macro.
                        let next = lines.get(j + 1).copied().unwrap_or("");
                        if !opens_trailing_closure(&format!("{} {next}", &line[c + 1..])) {
                            end = Some(j);
                            break 'scan;
                        }
                    }
                }
            }
        }
        let Some(end) = end else {
            break;
        };
        spans.push((i as i64 + 1, end as i64 + 1));
        i = end + 1;
    }
    spans
}

/// `^#Preview\b`: the line opens a preview macro, not `#PreviewProvider`.
fn opens_preview(line: &str) -> bool {
    line.strip_prefix("#Preview")
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
}

/// `^\s*\w+\s*:\s*\{`: what follows a closing brace opens another labelled
/// trailing closure.
fn opens_trailing_closure(rest: &str) -> bool {
    let rest = rest.trim_start_matches(is_js_whitespace);
    let after_label = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
    if after_label.len() == rest.len() {
        return false;
    }
    after_label
        .trim_start_matches(is_js_whitespace)
        .strip_prefix(':')
        .is_some_and(|rest| rest.trim_start_matches(is_js_whitespace).starts_with('{'))
}

fn test_files(ctx: &Ctx<'_>, limit: usize) -> ApiResult<WireList<WireEntryTest>> {
    let candidates: Vec<String> = ctx
        .store
        .all_files()?
        .iter()
        .map(|file| to_posix_path(&file.path))
        .filter(|path| is_test_path(path))
        .collect();
    if candidates.is_empty() {
        return Ok(wire_list(Vec::new(), 0));
    }

    let mut reach: HashMap<String, (i64, i64)> = HashMap::new();
    for chunk in candidates.chunks(TEST_CHUNK) {
        for (path, reaches, refs) in ctx.store.file_reach_counts(chunk)? {
            reach.insert(to_posix_path(&path), (reaches, refs));
        }
    }
    let mut ranked: Vec<(String, i64, i64)> =
        reach.into_iter().map(|(p, (a, b))| (p, a, b)).collect();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| locale_compare(&a.0, &b.0))
    });

    let top: Vec<&(String, i64, i64)> = ranked.iter().take(limit).collect();
    let top_paths: Vec<String> = top.iter().map(|(p, _, _)| p.clone()).collect();
    let nodes: HashMap<String, Node> = ctx
        .store
        .file_nodes(&top_paths)?
        .into_iter()
        .map(|n| (to_posix_path(&n.file_path), n))
        .collect();
    let mut items: Vec<WireEntryTest> = Vec::new();
    for (path, reaches, refs) in top {
        let Some(node) = nodes.get(path) else {
            continue;
        };
        items.push(WireEntryTest {
            node: to_node_ref(node),
            reaches: *reaches,
            refs: *refs,
        });
    }
    let total = ranked.len().max(items.len());
    Ok(wire_list(items, total))
}

fn hubs(ctx: &Ctx<'_>, limit: usize) -> ApiResult<WireList<WireEntryHub>> {
    let ranked = ctx.store.top_depended_on(SCAN_ROWS)?;
    let ids: Vec<String> = ranked.iter().map(|(id, _)| id.clone()).collect();
    let nodes = ctx.store.nodes_by_ids(&ids)?;
    // A vendored bundle's one-letter functions are depended on by the bundle
    // alone (upstream #2122): generated files are skipped like tests, by the
    // indexed flag or the path convention.
    let mut paths: Vec<String> = nodes.values().map(|node| node.file_path.clone()).collect();
    paths.sort_unstable();
    paths.dedup();
    let flagged = ctx.store.generated_paths_among(&paths)?;
    let generated = |path: &str| flagged.contains(path) || is_generated_file(path);
    let mut items: Vec<WireEntryHub> = Vec::new();
    let mut eligible = 0;
    for (node_id, dependents) in &ranked {
        let Some(node) = nodes.get(node_id) else {
            continue;
        };
        if is_non_hub_kind(node.kind) || is_test_file(&node.file_path) || generated(&node.file_path)
        {
            continue;
        }
        eligible += 1;
        if items.len() >= limit {
            continue;
        }
        items.push(WireEntryHub {
            node: to_node_ref(node),
            dependents: *dependents,
        });
    }
    let total = eligible.max(items.len());
    Ok(wire_list(items, total))
}

fn directory_of(file_path: &str) -> String {
    let normalized = file_path.replace('\\', "/");
    match normalized.rfind('/') {
        Some(cut) => normalized[..cut].to_string(),
        None => ".".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_at_the_root_is_in_dot() {
        assert_eq!(directory_of("main.rs"), ".");
        assert_eq!(directory_of("src/bin/main.rs"), "src/bin");
        assert_eq!(directory_of("src\\win\\main.rs"), "src/win");
    }

    /// Upstream `swiftPreviewSpans`' fixture (#2109).
    #[test]
    fn finds_a_files_top_level_previews_a_widgets_timeline_closure_included() {
        let text = [
            "struct Row: View {",
            "  var body: some View { Text(\"x\") }",
            "}",
            "",
            "#Preview {",
            "  Row()",
            "}",
            "",
            "#Preview(as: .systemSmall) {",
            "  AccountWidget()",
            "} timeline: {",
            "  Entry(date: Date())",
            "}",
            "let later = Row()",
        ]
        .join("\n");
        assert_eq!(swift_preview_spans(&text), vec![(5, 7), (9, 13)]);
    }

    #[test]
    fn a_preview_provider_or_an_unclosed_preview_is_no_span() {
        assert!(swift_preview_spans("#PreviewProvider {\n}\n").is_empty());
        assert!(swift_preview_spans("  #Preview {\n}\n").is_empty());
        assert_eq!(
            swift_preview_spans("#Preview {\r\n  Row()\r\n}\r\n#Preview {\n  Row(\n"),
            vec![(1, 3)]
        );
        assert!(opens_trailing_closure(" timeline : {"));
        assert!(!opens_trailing_closure(" .padding()"));
        assert!(!opens_trailing_closure(" : {"));
    }

    #[test]
    fn bookkeeping_kinds_are_never_hubs() {
        for kind in [
            NodeKind::File,
            NodeKind::Import,
            NodeKind::Export,
            NodeKind::Parameter,
        ] {
            assert!(is_non_hub_kind(kind));
        }
        assert!(!is_non_hub_kind(NodeKind::Function));
        assert!(!is_non_hub_kind(NodeKind::Struct));
    }
}
