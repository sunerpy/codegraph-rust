//! `GET /api/map?root=&depth=` — the repository at module granularity.
//! Upstream `src/ui-server/api/map.ts` (+ `aggregateModuleGraph`).
//!
//! A module is a directory, not a guess: each indexed file maps to the first
//! `depth` path segments under the chosen root, a file loose in the root joins
//! one `(root files)` box unless it is a façade (`index.ts`, `lib.rs`,
//! `__init__.py`). A link's `count` is every confident cross-module edge behind
//! it; its `declared` count — what the viewer layers on — is the subset resolved
//! through an import, a qualified name, an inheritance clause or a typed
//! receiver. Uncertain edges are excluded from every count, and how many were
//! excluded rides on the payload. The layering and geometry are the viewer's.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Instant;

use codegraph_core::types::EdgeKind;
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::wire::{
    UNCERTAIN_BELOW, WireList, is_test_file, locale_compare, to_posix_path, wire_list,
};
use crate::respond::{ApiError, ApiResult, Query};

/// The edge kinds that count as "module A reaches into module B". `contains`
/// is absent on purpose; `navigates` has no Rust edge kind yet.
const MAP_EDGE_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Imports,
    EdgeKind::References,
    EdgeKind::Instantiates,
    EdgeKind::Extends,
    EdgeKind::Implements,
];

/// The kinds whose symbol pairs the tooltip names.
const PAIR_EDGE_KINDS: &[EdgeKind] = &[EdgeKind::Calls, EdgeKind::Imports, EdgeKind::Instantiates];

/// Symbol pairs kept per link — the tooltip shows four.
const TOP_PAIRS_PER_LINK: usize = 4;
/// File paths listed per module; `total` stays the real number.
const MAX_FILES_PER_MODULE: usize = 40;
const MAX_FILE_CYCLES: usize = 40;
const MAX_CYCLE_LENGTH: usize = 12;
const DEFAULT_DEPTH: usize = 1;
const MAX_DEPTH: usize = 4;

/// Basenames that stay their own box when they sit loose in a module root.
const FACADE_STEMS: &[&str] = &["index", "main", "lib", "mod", "__init__", "init"];

/// A box holding more than this share of the mapped symbols IS the program…
const DOMINANT_SHARE: f64 = 0.4;
/// …but only if there is something inside it.
const DOMINANT_MIN_FILES: usize = 25;
/// Fewer boxes than this is a list, not a picture.
const MIN_MODULES: usize = 4;
/// More than this and a deeper grouping has traded one unreadable map for another.
const MAX_MODULES: usize = 60;

const ROOT_FILES: &str = "(root files)";

/// Id of the bucket loose files fall into.
pub fn root_files_id(root: &str) -> String {
    if root.is_empty() {
        ROOT_FILES.to_string()
    } else {
        format!("{root}/{ROOT_FILES}")
    }
}

/// One indexed file as the map reads it.
#[derive(Debug, Clone)]
pub struct MapFile {
    pub path: String,
    pub language: String,
    pub symbols: i64,
    pub test: bool,
    pub generated: bool,
}

/// Strip a trailing slash and any leading `./`, so `src/` and `src` are one root.
pub fn normalize_root(raw: &str) -> String {
    let mut root = raw.trim().replace('\\', "/");
    while let Some(rest) = root.strip_prefix("./") {
        root = rest.to_string();
    }
    while let Some(rest) = root.strip_suffix('/') {
        root = rest.to_string();
    }
    if root == "." || root == "/" {
        return String::new();
    }
    root
}

fn stem_of(basename: &str) -> &str {
    match basename.find('.') {
        Some(dot) if dot > 0 => &basename[..dot],
        _ => basename,
    }
}

fn join_nonempty<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Which module a file belongs to, or `None` when it is outside the root.
pub fn module_id_for(file_path: &str, root: &str, depth: usize) -> Option<(String, bool)> {
    let path = to_posix_path(file_path);
    let rel = if root.is_empty() {
        path.as_str()
    } else {
        path.strip_prefix(&format!("{root}/"))?
    };
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let last = *parts.last()?;
    if parts.len() <= depth {
        // A loose file. The directories it DOES have still qualify it, so
        // `src/a/b.ts` at depth 2 lands in `src/a/(root files)`.
        if FACADE_STEMS.contains(&stem_of(last)) {
            let id = join_nonempty(std::iter::once(root).chain(parts.iter().copied()));
            return Some((id, true));
        }
        let dir =
            join_nonempty(std::iter::once(root).chain(parts[..parts.len() - 1].iter().copied()));
        return Some((root_files_id(&dir), false));
    }
    let id = join_nonempty(std::iter::once(root).chain(parts[..depth].iter().copied()));
    Some((id, false))
}

fn top_dir(path: &str) -> Option<&str> {
    match path.find('/') {
        Some(slash) if slash > 0 => Some(&path[..slash]),
        _ => None,
    }
}

/// The root the map opens on: the directory holding a clear majority of the
/// non-test symbols, or the whole repository.
pub fn pick_default_root(files: &[MapFile]) -> String {
    let mut by_dir: HashMap<&str, i64> = HashMap::new();
    let mut total = 0;
    for file in files {
        if file.test {
            continue;
        }
        let Some(dir) = top_dir(&file.path) else {
            continue;
        };
        *by_dir.entry(dir).or_insert(0) += file.symbols;
        total += file.symbols;
    }
    if total == 0 {
        return String::new();
    }
    let mut dirs: Vec<(&str, i64)> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| locale_compare(a.0, b.0));
    let mut best = "";
    let mut best_symbols = 0;
    let mut second = 0;
    for (dir, symbols) in dirs {
        if symbols > best_symbols {
            second = best_symbols;
            best = dir;
            best_symbols = symbols;
        } else if symbols > second {
            second = symbols;
        }
    }
    // A second root holding a fifth of the code belongs on the picture: map
    // the whole project.
    if second * 5 >= total {
        return String::new();
    }
    if best_symbols * 2 > total {
        best.to_string()
    } else {
        String::new()
    }
}

/// The non-test modules a depth would draw, and how concentrated they are.
fn tally_modules(files: &[MapFile], root: &str, depth: usize) -> (usize, f64, usize) {
    let mut by_module: HashMap<String, (i64, usize)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut total = 0;
    for file in files {
        if file.test {
            continue;
        }
        let Some((id, _)) = module_id_for(&file.path, root, depth) else {
            continue;
        };
        let entry = by_module.entry(id.clone()).or_insert_with(|| {
            order.push(id);
            (0, 0)
        });
        entry.0 += file.symbols;
        entry.1 += 1;
        total += file.symbols;
    }
    // The first module to reach the largest symbol count, as upstream's Map walk.
    let mut largest = (0, 0);
    for id in &order {
        let entry = by_module[id];
        if entry.0 > largest.0 {
            largest = entry;
        }
    }
    let share = if total == 0 {
        0.0
    } else {
        largest.0 as f64 / total as f64
    };
    (by_module.len(), share, largest.1)
}

/// How many segments name a module, when the reader has not said: the
/// shallowest depth that is neither dominated by one box worth opening nor too
/// small to be a picture, stopping before a deeper one becomes a crowd.
pub fn pick_default_depth(files: &[MapFile], root: &str) -> usize {
    let mut deepest = DEFAULT_DEPTH;
    for file in files {
        if file.test {
            continue;
        }
        let path = to_posix_path(&file.path);
        let rel = if root.is_empty() {
            path.as_str()
        } else {
            match path.strip_prefix(&format!("{root}/")) {
                Some(rel) => rel,
                None => continue,
            }
        };
        let segments = rel.split('/').filter(|p| !p.is_empty()).count();
        deepest = deepest.max(segments.saturating_sub(1));
    }
    let mut fallback = DEFAULT_DEPTH;
    let mut fallback_count = 0;
    let mut depth = DEFAULT_DEPTH;
    while depth <= MAX_DEPTH.min(deepest) {
        let (count, share, largest_files) = tally_modules(files, root, depth);
        if count == 0 || count > MAX_MODULES {
            break;
        }
        let dominated = share > DOMINANT_SHARE && largest_files >= DOMINANT_MIN_FILES;
        if count >= MIN_MODULES && !dominated {
            return depth;
        }
        if count > fallback_count {
            fallback = depth;
            fallback_count = count;
        }
        depth += 1;
    }
    fallback
}

/// JavaScript `parseInt(raw, 10)`: optional leading whitespace and sign, then
/// the longest run of digits; `None` where JavaScript says `NaN`.
fn parse_int_js(raw: &str) -> Option<i64> {
    let s = raw.trim_start();
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return None;
    }
    // Saturate: anything past i64 is out of range either way.
    let value = digits[..end].parse::<i64>().unwrap_or(i64::MAX);
    Some(if negative { -value } else { value })
}

/// `None` for either field means "nobody said" — the answer picks.
pub fn parse_map_query(query: &Query) -> ApiResult<(Option<String>, Option<usize>)> {
    let depth = match query.get("depth") {
        Some(raw) if !raw.is_empty() => match parse_int_js(raw) {
            Some(value) if (1..=MAX_DEPTH as i64).contains(&value) => Some(value as usize),
            _ => {
                return Err(ApiError::bad_request(format!(
                    "depth must be a whole number from 1 to {MAX_DEPTH}."
                )));
            }
        },
        _ => None,
    };
    let root = query.get("root").map(normalize_root);
    Ok((root, depth))
}

/// Rename `x/(root files)` to `x` wherever the bucket is all `x` has.
fn collapse_lone_root_files(ids: &HashSet<String>) -> HashMap<String, String> {
    let suffix = format!("/{ROOT_FILES}");
    let mut renamed = HashMap::new();
    for id in ids {
        let Some(dir) = id.strip_suffix(&suffix) else {
            continue;
        };
        // A bucket at the very top has no directory to become.
        if dir.is_empty() {
            continue;
        }
        let prefix = format!("{dir}/");
        let alone = !ids
            .iter()
            .any(|other| other != id && other.starts_with(&prefix));
        if alone {
            renamed.insert(id.clone(), dir.to_string());
        }
    }
    renamed
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireMapModule {
    id: String,
    label: String,
    files: usize,
    symbols: i64,
    languages: Vec<Value>,
    test: bool,
    generated: usize,
    generated_files: Vec<String>,
    facade: bool,
    file_list: WireList<String>,
    dependents: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireMapLink {
    source: String,
    target: String,
    count: i64,
    declared: i64,
    #[serde(serialize_with = "kinds_as_objects")]
    by_kind: Vec<(EdgeKind, i64)>,
    top_pairs: Vec<Value>,
}

/// `byKind` on the wire is `[{ kind, count }]`, biggest first.
fn kinds_as_objects<S: serde::Serializer>(
    kinds: &[(EdgeKind, i64)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(kinds.len()))?;
    for (kind, count) in kinds {
        seq.serialize_element(&json!({ "kind": kind.as_str(), "count": count }))?;
    }
    seq.end()
}

#[derive(Default)]
struct ModuleEntry {
    facade: bool,
    files: usize,
    symbols: i64,
    test_files: usize,
    generated_files: usize,
    generated_paths: HashSet<String>,
    languages: HashMap<String, usize>,
    paths: Vec<String>,
}

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let started = Instant::now();
    let (requested_root, requested_depth) = parse_map_query(query)?;

    let files: Vec<MapFile> = ctx
        .store
        .all_files()?
        .into_iter()
        .map(|file| {
            let path = to_posix_path(&file.path);
            MapFile {
                test: is_test_file(&path),
                path,
                language: file.language.as_str().to_string(),
                symbols: file.node_count,
                generated: file.generated,
            }
        })
        .collect();

    let root = requested_root.unwrap_or_else(|| pick_default_root(&files));
    // Root first, then depth against THAT root.
    let depth = requested_depth.unwrap_or_else(|| pick_default_depth(&files, &root));
    let counts = ctx.store.counts()?;
    let last_indexed_at = ctx.store.last_indexed_at()?;
    let key = [
        ctx.project_root().display().to_string(),
        last_indexed_at.unwrap_or(0).to_string(),
        counts.edge_count.to_string(),
        counts.file_count.to_string(),
        root.clone(),
        depth.to_string(),
    ]
    .join("\u{0}");
    if let Some(mut hit) = ctx.state.caches.map.get(&key) {
        hit["timing"] =
            json!({ "elapsedMs": started.elapsed().as_millis() as u64, "cached": true });
        return Ok(hit);
    }

    let mut assigned: HashMap<&str, (String, bool)> = HashMap::new();
    for file in &files {
        if let Some(at) = module_id_for(&file.path, &root, depth) {
            assigned.insert(file.path.as_str(), at);
        }
    }
    let ids: HashSet<String> = assigned.values().map(|(id, _)| id.clone()).collect();
    let renamed = collapse_lone_root_files(&ids);

    let mut modules: BTreeMap<String, ModuleEntry> = BTreeMap::new();
    let mut module_of_file: HashMap<String, String> = HashMap::new();
    for file in &files {
        let Some((at_id, facade)) = assigned.get(file.path.as_str()) else {
            continue;
        };
        let id = renamed.get(at_id).cloned().unwrap_or_else(|| at_id.clone());
        module_of_file.insert(file.path.clone(), id.clone());
        let entry = modules.entry(id).or_insert_with(|| ModuleEntry {
            facade: *facade,
            ..ModuleEntry::default()
        });
        entry.files += 1;
        entry.paths.push(file.path.clone());
        entry.symbols += file.symbols;
        if file.test {
            entry.test_files += 1;
        }
        if file.generated {
            entry.generated_files += 1;
            entry.generated_paths.insert(file.path.clone());
        }
        *entry.languages.entry(file.language.clone()).or_insert(0) += 1;
    }

    let (links, uncertain_edges) = aggregate_links(ctx, &module_of_file)?;

    // ONE fetch of the file edge list, read twice: the cycle finder and the
    // dependent counts are both questions about it.
    let file_pairs: Vec<(String, String)> = ctx
        .store
        .cross_file_dependency_pairs(UNCERTAIN_BELOW)?
        .into_iter()
        .map(|(a, b)| (to_posix_path(&a), to_posix_path(&b)))
        .collect();
    let dependents = count_dependents(&file_pairs, &module_of_file);

    let mut module_rows: Vec<WireMapModule> = modules
        .into_iter()
        .map(|(id, entry)| {
            let mut shown = entry.paths.clone();
            shown.sort();
            shown.truncate(MAX_FILES_PER_MODULE);
            let mut languages: Vec<(String, usize)> = entry.languages.into_iter().collect();
            languages.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| locale_compare(&a.0, &b.0)));
            let label = match id.rfind('/') {
                Some(cut) if cut + 1 < id.len() => id[cut + 1..].to_string(),
                Some(_) => id.clone(),
                None => id.clone(),
            };
            WireMapModule {
                label,
                files: entry.files,
                symbols: entry.symbols,
                languages: languages
                    .into_iter()
                    .map(|(language, files)| json!({ "language": language, "files": files }))
                    .collect(),
                test: entry.test_files * 2 > entry.files,
                generated: entry.generated_files,
                facade: entry.facade,
                // Only the SHOWN paths, so the list the panel dims and the list
                // it draws are the same list.
                generated_files: shown
                    .iter()
                    .filter(|p| entry.generated_paths.contains(*p))
                    .cloned()
                    .collect(),
                file_list: wire_list(shown, entry.files),
                dependents: dependents
                    .get(&id)
                    .map(|(files, modules)| json!({ "files": files, "modules": modules }))
                    .unwrap_or_else(|| json!({ "files": 0, "modules": 0 })),
                id,
            }
        })
        .collect();
    // Sorted so two runs over one index produce identical payloads.
    module_rows.sort_by(|a, b| locale_compare(&a.id, &b.id));

    let mut payload = json!({
        "root": root,
        "depth": depth,
        "roots": root_options(&files),
        "modules": module_rows,
        "links": links,
        "cycles": file_cycles(&file_pairs, &module_of_file),
        "excluded": { "uncertainEdges": uncertain_edges, "confidenceBelow": UNCERTAIN_BELOW },
        "index": {
            "lastIndexedAt": last_indexed_at,
            "edges": counts.edge_count,
            "files": counts.file_count,
        },
    });
    payload["timing"] =
        json!({ "elapsedMs": started.elapsed().as_millis() as u64, "cached": false });
    ctx.state.caches.map.put(key, payload.clone());
    Ok(payload)
}

/// `(source module, target module, edge kind)`.
type LinkKey = (String, String, &'static str);
/// `(kind, count, declared, uncertain)` behind one [`LinkKey`].
type LinkTotals = (EdgeKind, i64, i64, i64);
/// `(from, to, count, declared)`: one named symbol pair behind a link.
type PairTotals = (String, String, i64, i64);

/// Cross-module links from the one grouped pass over the edge table, and the
/// number of edges the confidence floor left out.
fn aggregate_links(
    ctx: &Ctx<'_>,
    module_of_file: &HashMap<String, String>,
) -> ApiResult<(Vec<WireMapLink>, i64)> {
    // (source, target, kind) → (kind, count, declared, uncertain)
    let mut by_kind: BTreeMap<LinkKey, LinkTotals> = BTreeMap::new();
    // (source, target, from, to) → (count, declared)
    let mut pairs: BTreeMap<(String, String, String, String), (i64, i64)> = BTreeMap::new();
    for row in ctx
        .store
        .cross_file_edge_rows(MAP_EDGE_KINDS, UNCERTAIN_BELOW)?
    {
        let (Some(source), Some(target)) = (
            module_of_file.get(&to_posix_path(&row.source_file)),
            module_of_file.get(&to_posix_path(&row.target_file)),
        ) else {
            continue;
        };
        if source == target {
            continue;
        }
        let totals = by_kind
            .entry((source.clone(), target.clone(), row.kind.as_str()))
            .or_insert((row.kind, 0, 0, 0));
        totals.1 += row.count;
        totals.2 += row.declared;
        totals.3 += row.uncertain;
        // Only the confident half of a row can be named.
        if row.count == 0 || !PAIR_EDGE_KINDS.contains(&row.kind) {
            continue;
        }
        let pair = pairs
            .entry((source.clone(), target.clone(), row.from_name, row.to_name))
            .or_insert((0, 0));
        pair.0 += row.count;
        pair.1 += row.declared;
    }

    let mut links: BTreeMap<(String, String), WireMapLink> = BTreeMap::new();
    let mut uncertain_edges = 0;
    for ((source, target, _), (kind, count, declared, uncertain)) in by_kind {
        uncertain_edges += uncertain;
        if count == 0 {
            continue;
        }
        let link = links
            .entry((source.clone(), target.clone()))
            .or_insert_with(|| WireMapLink {
                source,
                target,
                count: 0,
                declared: 0,
                by_kind: Vec::new(),
                top_pairs: Vec::new(),
            });
        link.count += count;
        link.declared += declared;
        link.by_kind.push((kind, count));
    }
    for link in links.values_mut() {
        link.by_kind.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| locale_compare(a.0.as_str(), b.0.as_str()))
        });
    }

    let mut by_link: BTreeMap<(String, String), Vec<PairTotals>> = BTreeMap::new();
    for ((source, target, from, to), (count, declared)) in pairs {
        by_link
            .entry((source, target))
            .or_default()
            .push((from, to, count, declared));
    }
    for (key, mut list) in by_link {
        let Some(link) = links.get_mut(&key) else {
            continue;
        };
        list.sort_by(|a, b| {
            b.3.cmp(&a.3)
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| locale_compare(&a.0, &b.0))
                .then_with(|| locale_compare(&a.1, &b.1))
        });
        link.top_pairs = list
            .into_iter()
            .take(TOP_PAIRS_PER_LINK)
            .map(|(from, to, count, declared)| json!({ "from": from, "to": to, "count": count, "declared": declared }))
            .collect();
    }

    let mut out: Vec<WireMapLink> = links.into_values().collect();
    out.sort_by(|a, b| {
        locale_compare(&a.source, &b.source).then_with(|| locale_compare(&a.target, &b.target))
    });
    Ok((out, uncertain_edges))
}

/// Per module: how many files outside it reference into it, and across how
/// many modules those files sit. DISTINCT files, not references.
fn count_dependents(
    pairs: &[(String, String)],
    module_of_file: &HashMap<String, String>,
) -> HashMap<String, (usize, usize)> {
    let mut incoming: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (source, target) in pairs {
        let (Some(from), Some(to)) = (module_of_file.get(source), module_of_file.get(target))
        else {
            continue;
        };
        if from == to {
            continue;
        }
        incoming
            .entry(to.as_str())
            .or_default()
            .insert(source.as_str());
    }
    incoming
        .into_iter()
        .map(|(module, files)| {
            let modules: HashSet<&str> = files
                .iter()
                .filter_map(|f| module_of_file.get(*f).map(String::as_str))
                .collect();
            (module.to_string(), (files.len(), modules.len()))
        })
        .collect()
}

/// File-level circular dependencies, as strongly connected components.
fn file_cycles(pairs: &[(String, String)], module_of_file: &HashMap<String, String>) -> Value {
    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut all: BTreeSet<&str> = BTreeSet::new();
    for (source, target) in pairs {
        if !module_of_file.contains_key(source) || !module_of_file.contains_key(target) {
            continue;
        }
        adjacency
            .entry(source.as_str())
            .or_default()
            .push(target.as_str());
        all.insert(source.as_str());
        all.insert(target.as_str());
    }
    for list in adjacency.values_mut() {
        list.sort_unstable();
    }
    let nodes: Vec<&str> = all.into_iter().collect();
    let components = tarjan(&nodes, |id| {
        adjacency.get(id).map(Vec::as_slice).unwrap_or(&[])
    });
    let mut cycles: Vec<Vec<&str>> = components
        .into_iter()
        .filter(|c| c.len() > 1)
        .map(|mut c| {
            c.sort_unstable();
            c
        })
        .collect();
    cycles.sort_by(|a, b| {
        a.len().cmp(&b.len()).then_with(|| {
            locale_compare(
                a.first().copied().unwrap_or(""),
                b.first().copied().unwrap_or(""),
            )
        })
    });
    let items: Vec<Value> = cycles
        .iter()
        .take(MAX_FILE_CYCLES)
        .map(|files| {
            let modules: BTreeSet<&str> = files
                .iter()
                .map(|f| module_of_file.get(*f).map(String::as_str).unwrap_or(f))
                .collect();
            json!({
                "size": files.len(),
                "files": files.iter().take(MAX_CYCLE_LENGTH).collect::<Vec<_>>(),
                "modules": modules.into_iter().collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "total": cycles.len(),
        "shown": items.len(),
        "truncated": cycles.len() > items.len(),
        "items": items,
    })
}

/// Tarjan's strongly connected components, iterative so a deep graph cannot
/// blow the stack.
fn tarjan<'a, F>(nodes: &[&'a str], edges_of: F) -> Vec<Vec<&'a str>>
where
    F: Fn(&str) -> &'a [&'a str],
{
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut low: HashMap<&str, usize> = HashMap::new();
    let mut on_stack: HashSet<&str> = HashSet::new();
    let mut stack: Vec<&str> = Vec::new();
    let mut out: Vec<Vec<&str>> = Vec::new();
    let mut counter = 0;

    for &start in nodes {
        if index.contains_key(start) {
            continue;
        }
        let mut work: Vec<(&str, &[&str], usize)> = vec![(start, edges_of(start), 0)];
        index.insert(start, counter);
        low.insert(start, counter);
        counter += 1;
        stack.push(start);
        on_stack.insert(start);

        while let Some(frame) = work.last_mut() {
            let (id, edges, at) = (frame.0, frame.1, frame.2);
            if at < edges.len() {
                let next = edges[at];
                frame.2 += 1;
                if !index.contains_key(next) {
                    index.insert(next, counter);
                    low.insert(next, counter);
                    counter += 1;
                    stack.push(next);
                    on_stack.insert(next);
                    work.push((next, edges_of(next), 0));
                } else if on_stack.contains(next) {
                    let value = low[id].min(index[next]);
                    low.insert(id, value);
                }
                continue;
            }
            work.pop();
            if low[id] == index[id] {
                let mut component = Vec::new();
                while let Some(popped) = stack.pop() {
                    on_stack.remove(popped);
                    component.push(popped);
                    if popped == id {
                        break;
                    }
                }
                out.push(component);
            }
            if let Some(parent) = work.last() {
                let value = low[parent.0].min(low[id]);
                low.insert(parent.0, value);
            }
        }
    }
    out
}

/// The roots the selector offers: the repository root plus every top-level
/// directory holding indexed files, biggest first.
fn root_options(files: &[MapFile]) -> Vec<Value> {
    let mut by_dir: HashMap<&str, usize> = HashMap::new();
    for file in files {
        if let Some(dir) = top_dir(&file.path) {
            *by_dir.entry(dir).or_insert(0) += 1;
        }
    }
    let mut dirs: Vec<(&str, usize)> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| locale_compare(a.0, b.0)));
    let mut out = vec![json!({ "root": "", "label": "whole repository", "files": files.len() })];
    out.extend(
        dirs.into_iter()
            .map(|(root, count)| json!({ "root": root, "label": root, "files": count })),
    );
    out
}

#[cfg(test)]
mod tests {
    //! The pure half of upstream `__tests__/ui-map-api.test.ts`, case by case.
    use super::*;

    fn file(path: &str, symbols: i64, test: bool) -> MapFile {
        MapFile {
            path: path.to_string(),
            language: "typescript".to_string(),
            symbols,
            test,
            generated: false,
        }
    }

    /// `n` files under `dir`, each carrying `each` symbols.
    fn spread(dir: &str, n: usize, each: i64, test: bool) -> Vec<MapFile> {
        (0..n)
            .map(|i| file(&format!("{dir}/f{i}.ts"), each, test))
            .collect()
    }

    fn id(path: &str, root: &str, depth: usize) -> Option<(String, bool)> {
        module_id_for(path, root, depth)
    }

    #[test]
    fn names_a_module_after_the_first_depth_segments_under_the_root() {
        assert_eq!(
            id("src/core/engine.ts", "src", 1),
            Some(("src/core".into(), false))
        );
        assert_eq!(
            id("src/a/b/c.ts", "src", 2),
            Some(("src/a/b".into(), false))
        );
        assert_eq!(id("a/b/c.ts", "", 1), Some(("a".into(), false)));
    }

    #[test]
    fn keeps_a_facade_as_its_own_box_and_buckets_the_other_loose_files() {
        assert_eq!(
            id("src/index.ts", "src", 1),
            Some(("src/index.ts".into(), true))
        );
        assert_eq!(id("src/lib.rs", "src", 1).map(|(_, f)| f), Some(true));
        assert_eq!(id("pkg/__init__.py", "pkg", 1).map(|(_, f)| f), Some(true));
        assert_eq!(
            id("src/types.ts", "src", 1),
            Some(("src/(root files)".into(), false))
        );
        assert_eq!(id("types.ts", "", 1), Some(("(root files)".into(), false)));
        assert_eq!(stem_of(".eslintrc"), ".eslintrc");
        assert_eq!(stem_of("index.test.ts"), "index");
    }

    #[test]
    fn buckets_a_loose_file_into_the_directory_it_is_actually_in_not_the_top_one() {
        assert_eq!(
            id("src/a/loose.ts", "src", 2),
            Some(("src/a/(root files)".into(), false))
        );
        assert_eq!(id("src/a/b.ts", "", 2), Some(("src/a".into(), false)));
    }

    #[test]
    fn returns_null_for_a_file_outside_the_root() {
        assert_eq!(id("__tests__/x.test.ts", "src", 1), None);
        // A sibling whose name merely starts with the root is not under it.
        assert_eq!(id("srcx/y.ts", "src", 1), None);
    }

    #[test]
    fn treats_src_src_slash_and_dot_src_as_one_root() {
        assert_eq!(normalize_root("src"), "src");
        assert_eq!(normalize_root("src/"), "src");
        assert_eq!(normalize_root("./src"), "src");
        assert_eq!(normalize_root("src\\"), "src");
        assert_eq!(normalize_root(" ././a//"), "a");
    }

    #[test]
    fn treats_the_repository_root_as_the_empty_string_however_it_is_written() {
        for raw in ["", ".", "/"] {
            assert_eq!(normalize_root(raw), "", "{raw:?}");
        }
    }

    #[test]
    fn picks_the_directory_holding_a_clear_majority_of_the_non_test_symbols() {
        let files = vec![
            file("src/a.ts", 80, false),
            file("scripts/b.ts", 5, false),
            file("__tests__/c.ts", 900, true),
        ];
        assert_eq!(pick_default_root(&files), "src");
    }

    #[test]
    fn falls_back_to_the_repository_root_when_no_directory_dominates() {
        let files = vec![
            file("a/one.ts", 10, false),
            file("b/two.ts", 10, false),
            file("c/three.ts", 10, false),
        ];
        assert_eq!(pick_default_root(&files), "");
        assert_eq!(pick_default_root(&[file("flat.ts", 4, false)]), "");
        // A second root holding a fifth of the code belongs on the picture.
        assert_eq!(
            pick_default_root(&[file("src/a.ts", 60, false), file("ios/b.ts", 40, false)]),
            ""
        );
    }

    #[test]
    fn goes_deeper_when_one_box_holds_the_program() {
        let mut files = spread("src/components", 119, 12, false);
        files.extend(spread("src/app", 53, 21, false));
        files.extend(spread("src/api", 47, 7, false));
        files.extend(spread("src/utils", 24, 6, false));
        files.extend(spread("ios/CaptureView", 63, 28, false));
        files.extend(spread("ios/Camera", 3, 38, false));
        files.extend(spread(".github/workflows", 4, 0, false));
        assert_eq!(pick_default_depth(&files, ""), 2);
    }

    #[test]
    fn keeps_a_repository_whose_directories_are_its_modules_at_one_level() {
        let mut files = spread("src/db", 8, 40, false);
        files.extend(spread("src/graph", 9, 40, false));
        files.extend(spread("src/mcp", 7, 40, false));
        files.extend(spread("src/search", 5, 40, false));
        files.extend(spread("src/sync", 4, 40, false));
        assert_eq!(pick_default_depth(&files, "src"), 1);
    }

    #[test]
    fn does_not_open_a_dominant_box_that_has_nothing_in_it() {
        let mut files = spread("src/core", 3, 90, false);
        files.extend(spread("src/db", 2, 10, false));
        files.extend(spread("src/api", 2, 10, false));
        files.push(file("src/index.ts", 5, false));
        assert_eq!(pick_default_depth(&files, "src"), 1);
    }

    #[test]
    fn keeps_going_while_the_picture_is_still_one_box() {
        let mut files = spread("frontend/src/screens", 15, 10, false);
        files.extend(spread("frontend/src/components", 14, 10, false));
        files.extend(spread("frontend/src/hooks", 8, 10, false));
        files.extend(spread("frontend/src/api", 6, 10, false));
        files.extend(spread("backend/app", 5, 8, false));
        assert_eq!(pick_default_depth(&files, ""), 3);
    }

    #[test]
    fn stops_before_a_deeper_grouping_becomes_a_crowd() {
        let mut files = spread("src/a", 30, 10, false);
        files.extend((0..70).map(|i| file(&format!("src/b/m{i}/f.ts"), 1, false)));
        assert_eq!(pick_default_depth(&files, ""), 2);
    }

    #[test]
    fn does_not_chase_a_tree_that_has_no_more_levels_to_give() {
        let mut files = spread("src/a", 30, 10, false);
        files.extend(spread("src/b", 2, 1, false));
        assert_eq!(pick_default_depth(&files, "src"), 1);
    }

    #[test]
    fn counts_only_the_modules_the_map_draws_by_default() {
        let mut files = spread("src/app", 40, 10, false);
        files.extend(spread("src/__tests__/a", 12, 10, true));
        files.extend(spread("src/__tests__/b", 12, 10, true));
        files.extend(spread("src/__tests__/c", 12, 10, true));
        assert_eq!(pick_default_depth(&files, "src"), 1);
    }

    #[test]
    fn a_lone_bucket_takes_its_directory_name() {
        let ids: HashSet<String> = [
            "backend/controllers/(root files)",
            "src/(root files)",
            "src/api",
            "(root files)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let renamed = collapse_lone_root_files(&ids);
        assert_eq!(
            renamed
                .get("backend/controllers/(root files)")
                .map(String::as_str),
            Some("backend/controllers")
        );
        assert!(!renamed.contains_key("src/(root files)"));
        assert!(!renamed.contains_key("(root files)"));
    }

    #[test]
    fn js_parse_int_semantics() {
        assert_eq!(parse_int_js("2"), Some(2));
        assert_eq!(parse_int_js(" 3x"), Some(3));
        assert_eq!(parse_int_js("1.5"), Some(1));
        assert_eq!(parse_int_js("-1"), Some(-1));
        assert_eq!(parse_int_js("x"), None);
        assert_eq!(parse_int_js(""), None);
    }

    #[test]
    fn tarjan_finds_cycles_iteratively() {
        let adjacency: HashMap<&str, Vec<&str>> = [
            ("a", vec!["b"]),
            ("b", vec!["c"]),
            ("c", vec!["a"]),
            ("d", vec!["a"]),
        ]
        .into_iter()
        .collect();
        let nodes = vec!["a", "b", "c", "d"];
        let empty: Vec<&str> = Vec::new();
        let mut components = tarjan(&nodes, |id| {
            adjacency.get(id).map(Vec::as_slice).unwrap_or(&empty)
        });
        for c in &mut components {
            c.sort_unstable();
        }
        assert!(components.contains(&vec!["a", "b", "c"]));
        assert!(components.contains(&vec!["d"]));
    }
}
