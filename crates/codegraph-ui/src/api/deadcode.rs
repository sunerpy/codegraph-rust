//! `GET /api/deadcode?limit=&kinds=&exported=&tests=&generated=` — symbols
//! nothing in this repository reaches, grouped by the file they live in.
//! Upstream `src/ui-server/api/deadcode.ts`.
//!
//! The derivation is `codegraph_graph::dead_code`, shared so a second surface
//! asking the same question cannot get a different answer. This module hands it
//! a source reader that goes through the viewer's read chokepoint, and carries
//! the exclusion counts onto the wire so the screen can say what the list could
//! not see. Rows come back ranked (largest first) and are grouped by file for
//! display; the group order follows the best row in it.

use std::collections::HashMap;
use std::time::Instant;

use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::NodeKind;
use codegraph_graph::dead_code::{
    DeadCodeQuery, MAX_CORROBORATION_BYTES, build_dead_code_report, is_allowed_dead_code_kind,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::source::read_indexed_file_text;
use super::wire::{WireList, WireNodeRef, locale_compare, to_node_ref, wire_list};
use crate::respond::{ApiResult, Query};

/// Rows carried on the payload. The screen shows every one it is given.
pub const MAX_DEAD_CODE_ROWS: i64 = 300;
/// Members folded under one row before the row just counts them.
pub const MAX_DEAD_CODE_MEMBERS: usize = 12;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireDeadCodeRow {
    #[serde(flatten)]
    node: WireNodeRef,
    /// Source lines it spans — the rank, and what deleting it would remove.
    lines: i64,
    members: WireList<WireNodeRef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireDeadCodeGroup {
    file: String,
    generated: bool,
    test: bool,
    lines: i64,
    rows: Vec<WireDeadCodeRow>,
}

/// The sentence each exclusion prints under the list, as "N <label>".
fn exclusion_label(reason: &str) -> &'static str {
    match reason {
        "tests" => "in test files",
        "generated" => "in generated files",
        "exported" => "exported, or declared in a header",
        "exportsUnknown" => "in languages this index records no exports for",
        "declarations" => "abstract, or declared on an interface",
        "decorated" => "carrying a decorator, so a framework registers them",
        "overriding" => "overriding a member declared further up",
        "implicit" => "named something the language calls by itself",
        "vendored" => "in vendored directories",
        "testScope" => "inside a test module",
        "markup" => "in component files, where markup can reference them invisibly",
        "unreachableFile" => "in files nothing reaches — islands, drawn on the map",
        "unresolvedName" => "sharing a name the index failed to resolve somewhere",
        "ambiguousName" => "sharing a name with a symbol that IS referenced",
        "mentioned" => "written more than once in a file that can reach them",
        "unreadable" => "in files that could not be read",
        "nested" => "folded into a container on this list",
        _ => "",
    }
}

/// The parsed query: `kinds` keeps only the kinds a request may ask for.
pub struct DeadCodeOptions {
    pub limit: i64,
    pub include_exported: bool,
    pub include_tests: bool,
    pub include_generated: bool,
    pub kinds: Option<Vec<NodeKind>>,
}

pub fn parse_dead_code_query(query: &Query) -> ApiResult<DeadCodeOptions> {
    let kinds = query.get("kinds").filter(|raw| !raw.is_empty()).map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter_map(|name| NodeKind::ALL.into_iter().find(|k| k.as_str() == name))
            .filter(|kind| is_allowed_dead_code_kind(*kind))
            .collect::<Vec<_>>()
    });
    Ok(DeadCodeOptions {
        limit: query.int("limit", 1, MAX_DEAD_CODE_ROWS, Some(MAX_DEAD_CODE_ROWS))?,
        include_exported: query.get("exported") == Some("1"),
        include_tests: query.get("tests") == Some("1"),
        include_generated: query.get("generated") == Some("1"),
        kinds: kinds.filter(|k| !k.is_empty()),
    })
}

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let started = Instant::now();
    let options = parse_dead_code_query(query)?;
    // The chokepoint, not `fs`: the viewer never opens a path the index does
    // not name and the containment check has not cleared.
    let reader = |file_path: &str| read_indexed_file_text(ctx, file_path, MAX_CORROBORATION_BYTES);
    let report = build_dead_code_report(
        ctx.store,
        &DeadCodeQuery {
            kinds: options.kinds.clone(),
            include_exported: options.include_exported,
            include_tests: options.include_tests,
            include_generated: options.include_generated,
            limit: options.limit as usize,
            read_source: Some(&reader),
        },
    )?;

    // The path convention plus the indexed banner verdict, both, so a generated
    // file dims here for the same reason it dims on the map.
    let paths: Vec<String> = report
        .entries
        .iter()
        .map(|e| e.node.file_path.clone())
        .collect();
    let flagged = ctx.store.generated_paths_among(&paths)?;
    let generated = |path: &str| flagged.contains(path) || is_generated_file(path);

    let mut rows: Vec<WireDeadCodeRow> = Vec::new();
    let mut groups: Vec<WireDeadCodeGroup> = Vec::new();
    let mut group_of: HashMap<String, usize> = HashMap::new();
    for entry in &report.entries {
        let row = WireDeadCodeRow {
            node: to_node_ref(&entry.node),
            lines: entry.lines,
            members: wire_list(
                entry
                    .members
                    .iter()
                    .take(MAX_DEAD_CODE_MEMBERS)
                    .map(to_node_ref)
                    .collect(),
                entry.members.len(),
            ),
        };
        rows.push(row.clone());
        let at = match group_of.get(&row.node.file) {
            Some(&at) => at,
            None => {
                group_of.insert(row.node.file.clone(), groups.len());
                groups.push(WireDeadCodeGroup {
                    file: row.node.file.clone(),
                    generated: generated(&entry.node.file_path),
                    test: row.node.test,
                    lines: 0,
                    rows: Vec::new(),
                });
                groups.len() - 1
            }
        };
        groups[at].lines += row.lines;
        groups[at].rows.push(row);
    }
    for group in &mut groups {
        group.rows.sort_by_key(|row| row.node.line);
    }

    let mut excluded: Vec<(&'static str, usize)> = report
        .excluded
        .entries()
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect();
    excluded.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| locale_compare(a.0, b.0)));
    let excluded_total: usize = excluded.iter().map(|(_, count)| count).sum();

    Ok(json!({
        "rows": wire_list(rows, report.total),
        "groups": groups,
        "candidates": report.candidates,
        "excluded": excluded
            .into_iter()
            .map(|(reason, count)| json!({ "reason": reason, "count": count, "label": exclusion_label(reason) }))
            .collect::<Vec<_>>(),
        "excludedTotal": excluded_total,
        "kinds": report.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        "includeExported": report.include_exported,
        "includeTests": options.include_tests,
        "includeGenerated": options.include_generated,
        "bounded": report.bounded,
        "corroborated": report.corroborated,
        "timing": { "elapsedMs": started.elapsed().as_millis() as u64 },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_graph::dead_code::DeadCodeExclusions;

    #[test]
    fn every_exclusion_has_a_label() {
        for (reason, _) in DeadCodeExclusions::default().entries() {
            assert!(!exclusion_label(reason).is_empty(), "{reason}");
        }
    }

    #[test]
    fn kinds_keep_only_what_a_request_may_ask_for() {
        let query = Query::parse(Some(
            "kinds=function, variable,import,bogus&exported=1&limit=5",
        ));
        let options = parse_dead_code_query(&query).unwrap();
        assert_eq!(
            options.kinds,
            Some(vec![NodeKind::Function, NodeKind::Variable])
        );
        assert!(options.include_exported);
        assert!(!options.include_tests);
        assert_eq!(options.limit, 5);
        let none = parse_dead_code_query(&Query::parse(Some("kinds=import"))).unwrap();
        assert_eq!(none.kinds, None);
    }
}
