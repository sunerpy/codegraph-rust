use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::canonicalize::{CanonicalDb, CanonicalRow};

/// Every surface name `diff_canonical` can put on a `DiffEntry`, enumerated from
/// its `compare_*` call sites (nodes/files via `compare_tier1_rows`, schema via
/// `compare_schema`, edges/unresolved_refs via `compare_tier2_rows`). A RULE that
/// names anything else can never match a real diff, so it is rejected at parse
/// time: a typo'd `surface=` would otherwise sit in the document forever looking
/// like an active decision while silently allowing nothing — a LOUD error is
/// strictly better than an inert rule nobody notices is inert.
const DIFF_SURFACES: [&str; 5] = ["nodes", "files", "schema", "edges", "unresolved_refs"];

/// The field names a RULE line may carry. Anything else is a typo.
const RULE_FIELDS: [&str; 4] = ["tier", "surface", "key", "justification"];

/// A `DiffEntry` legitimately carries ANY tier — `diff_canonical` mints Tier-1
/// (nodes/files/schema) and Tier-2 (edges/unresolved_refs) entries today, and
/// Tier-3 exists for behavioral surfaces. The asymmetry lives on the allowlist
/// side, not here: only Tier-3 may appear on a `RULE` line (`parse_rule`) and
/// only a Tier-3 entry can ever be allowed (`KnownDiffs::allows`). So all three
/// variants stay.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Tier {
    Tier1,
    Tier2,
    Tier3,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffEntry {
    pub tier: Tier,
    pub surface: String,
    pub key: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffError {
    entries: Vec<DiffEntry>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KnownDiffs {
    rules: Vec<KnownDiffRule>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KnownDiffRule {
    tier: Tier,
    surface: String,
    key_pattern: String,
    justification: String,
}

pub fn diff_canonical(
    expected: &CanonicalDb,
    actual: &CanonicalDb,
    known_diffs: Option<&KnownDiffs>,
) -> Result<(), DiffError> {
    let mut entries = Vec::new();
    compare_tier1_rows(&mut entries, "nodes", "id", &expected.nodes, &actual.nodes);
    compare_tier1_rows(
        &mut entries,
        "files",
        "path",
        &expected.files,
        &actual.files,
    );
    compare_schema(&mut entries, &expected.schema, &actual.schema);
    compare_tier2_rows(
        &mut entries,
        "edges",
        edge_key,
        &expected.edges,
        &actual.edges,
    );
    compare_tier2_rows(
        &mut entries,
        "unresolved_refs",
        ref_key,
        &expected.unresolved_refs,
        &actual.unresolved_refs,
    );

    if let Some(known_diffs) = known_diffs {
        entries.retain(|entry| !known_diffs.allows(entry));
    }

    if entries.is_empty() {
        Ok(())
    } else {
        Err(DiffError { entries })
    }
}

impl DiffError {
    pub fn entries(&self) -> &[DiffEntry] {
        &self.entries
    }
}

impl std::error::Error for DiffError {}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "canonical equivalence failed with {} diff(s)",
            self.entries.len()
        )?;
        for entry in &self.entries {
            writeln!(
                f,
                "- {:?} {} key={}\n  expected: {}\n  actual:   {}",
                entry.tier,
                entry.surface,
                entry.key,
                truncate(&entry.expected),
                truncate(&entry.actual)
            )?;
        }
        Ok(())
    }
}

impl KnownDiffs {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn repo_doc_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("codegraph-bench lives under crates/")
            .join("docs/upstream-sync/KNOWN_DIFFS.md")
    }

    /// The committed allowlist, parsed. A syntactically invalid `KNOWN_DIFFS.md`
    /// therefore FAILS every equivalence assertion instead of being silently
    /// ignored, which is what made the document decorative before this wiring.
    pub fn load_repo_doc() -> Result<Self> {
        Self::load(&Self::repo_doc_path())
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Lines inside a fenced code block are DOCUMENTATION, not rules: the "Rule
    /// format" section of `KNOWN_DIFFS.md` shows the template
    /// `RULE tier=3 surface=<surface> …` inside a ```text fence, and the
    /// pre-fail-closed parser accepted it as an ACTIVE rule (tier=3, surface
    /// `<surface>`) — an allowlist entry nobody wrote on purpose, inert only
    /// because that placeholder surface never matches a real diff.
    pub fn parse(text: &str) -> Result<Self> {
        let mut rules = Vec::new();
        let mut in_fence = false;
        for (line_number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || line.is_empty() || line.starts_with('#') || !line.starts_with("RULE ") {
                continue;
            }
            rules.push(parse_rule(line).with_context(|| {
                format!("parsing KNOWN_DIFFS.md line {}: {line}", line_number + 1)
            })?);
        }
        if in_fence {
            anyhow::bail!(
                "unterminated code fence: every RULE line after it would be silently skipped"
            );
        }
        Ok(Self { rules })
    }

    pub fn allows(&self, entry: &DiffEntry) -> bool {
        if entry.tier != Tier::Tier3 {
            return false;
        }
        self.rules.iter().any(|rule| {
            rule.tier == entry.tier
                && rule.surface == entry.surface
                && (rule.key_pattern == "*" || entry.key.contains(&rule.key_pattern))
        })
    }
}

/// Fail-closed RULE parser. Every rejection below exists because the permissive
/// version accepted a line that LOOKED like an active allowlist entry and then
/// silently did nothing (or, for `tier=1|2`, looked like it waved through a
/// Tier-1 golden difference):
///
/// - a token without `=` was dropped, so `RULE garbage tier=3 …` parsed clean;
/// - an unknown field name (`surfce=nodes`) was dropped, so the rule silently
///   fell back to "missing surface" or to a stale value;
/// - a duplicate field silently kept the last occurrence, so two contradictory
///   `key=` values resolved by position alone;
/// - `tier=1` / `tier=2` parsed fine and were discarded only later by `allows`,
///   so the document's promise that Tier-1/Tier-2 are never allowlisted held by
///   downstream accident rather than at parse time;
/// - a `surface=` outside `DIFF_SURFACES` can never match a real diff.
fn parse_rule(line: &str) -> Result<KnownDiffRule> {
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    for token in line.trim_start_matches("RULE ").split_whitespace() {
        let (key, value) = token
            .split_once('=')
            .with_context(|| format!("token {token} is not key=value"))?;
        if key.is_empty() || value.is_empty() {
            anyhow::bail!("token {token} has an empty key or value");
        }
        if !RULE_FIELDS.contains(&key) {
            anyhow::bail!("unknown field {key}; allowed fields are {RULE_FIELDS:?}");
        }
        if fields.insert(key, value).is_some() {
            anyhow::bail!("duplicate field {key}");
        }
    }

    let tier_token = *fields.get("tier").context("missing tier")?;
    let tier = match tier_token {
        "3" | "Tier3" | "tier3" => Tier::Tier3,
        "1" | "Tier1" | "tier1" | "2" | "Tier2" | "tier2" => anyhow::bail!(
            "tier={tier_token} may not be allowlisted; only Tier-3 differences can be allowed"
        ),
        value => anyhow::bail!("unknown tier {value}"),
    };

    let surface = *fields.get("surface").context("missing surface")?;
    if !DIFF_SURFACES.contains(&surface) {
        anyhow::bail!("unknown surface {surface}; the differ reports {DIFF_SURFACES:?}");
    }

    Ok(KnownDiffRule {
        tier,
        surface: surface.to_string(),
        key_pattern: fields
            .get("key")
            .context("missing key")?
            .trim_matches('`')
            .to_string(),
        justification: fields
            .get("justification")
            .context("missing justification")?
            .to_string(),
    })
}

fn compare_tier1_rows(
    entries: &mut Vec<DiffEntry>,
    surface: &str,
    key_column: &str,
    expected: &[CanonicalRow],
    actual: &[CanonicalRow],
) {
    let expected_by_key = rows_by_key(expected, key_column);
    let actual_by_key = rows_by_key(actual, key_column);
    let keys = expected_by_key
        .keys()
        .chain(actual_by_key.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in keys {
        match (expected_by_key.get(&key), actual_by_key.get(&key)) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(expected), Some(actual)) => entries.push(entry(
                Tier::Tier1,
                surface,
                &key,
                row_json(expected),
                row_json(actual),
            )),
            (Some(expected), None) => entries.push(entry(
                Tier::Tier1,
                surface,
                &key,
                row_json(expected),
                "<missing>".to_string(),
            )),
            (None, Some(actual)) => entries.push(entry(
                Tier::Tier1,
                surface,
                &key,
                "<missing>".to_string(),
                row_json(actual),
            )),
            (None, None) => {}
        }
    }
}

fn compare_schema(entries: &mut Vec<DiffEntry>, expected: &str, actual: &str) {
    if expected != actual {
        entries.push(entry(
            Tier::Tier1,
            "schema",
            ".schema",
            expected.to_string(),
            actual.to_string(),
        ));
    }
}

fn compare_tier2_rows(
    entries: &mut Vec<DiffEntry>,
    surface: &str,
    key_fn: fn(&CanonicalRow) -> String,
    expected: &[CanonicalRow],
    actual: &[CanonicalRow],
) {
    let expected_counts = multiset(expected);
    let actual_counts = multiset(actual);
    let keys = expected_counts
        .keys()
        .chain(actual_counts.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in keys {
        let expected_count = expected_counts.get(&key).copied().unwrap_or(0);
        let actual_count = actual_counts.get(&key).copied().unwrap_or(0);
        if expected_count != actual_count {
            let display_key = expected
                .iter()
                .chain(actual.iter())
                .find(|row| row_json(row) == *key)
                .map(key_fn)
                .unwrap_or_else(|| key.clone());
            entries.push(entry(
                Tier::Tier2,
                surface,
                &display_key,
                format!("count={expected_count} row={key}"),
                format!("count={actual_count} row={key}"),
            ));
        }
    }
}

fn rows_by_key<'a>(
    rows: &'a [CanonicalRow],
    key_column: &str,
) -> BTreeMap<String, &'a CanonicalRow> {
    rows.iter()
        .map(|row| (row_string(row, key_column), row))
        .collect()
}

fn multiset(rows: &[CanonicalRow]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for row in rows {
        *counts.entry(row_json(row)).or_insert(0) += 1;
    }
    counts
}

fn edge_key(row: &CanonicalRow) -> String {
    format!(
        "({}, {}, {})",
        row_string(row, "source"),
        row_string(row, "target"),
        row_string(row, "kind")
    )
}

fn ref_key(row: &CanonicalRow) -> String {
    format!(
        "({}, {}, {}, {}, {})",
        row_string(row, "from_node_id"),
        row_string(row, "reference_name"),
        row_string(row, "reference_kind"),
        row_string(row, "line"),
        row_string(row, "col")
    )
}

fn row_string(row: &CanonicalRow, key: &str) -> String {
    row.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            row.get(key)
                .map_or_else(String::new, serde_json::Value::to_string)
        })
}

fn row_json(row: &CanonicalRow) -> String {
    serde_json::to_string(row).unwrap_or_else(|_| "<unserializable>".to_string())
}

fn entry(tier: Tier, surface: &str, key: &str, expected: String, actual: String) -> DiffEntry {
    DiffEntry {
        tier,
        surface: surface.to_string(),
        key: key.to_string(),
        expected,
        actual,
    }
}

fn truncate(value: &str) -> String {
    const MAX: usize = 600;
    if value.len() <= MAX {
        value.to_string()
    } else {
        format!("{}…<truncated {} bytes>", &value[..MAX], value.len() - MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIER3_RULE: &str =
        "RULE tier=3 surface=nodes key=alpha justification=documented-behavioral-drift";

    fn tier3_entry(surface: &str, key: &str) -> DiffEntry {
        entry(
            Tier::Tier3,
            surface,
            key,
            "expected".to_string(),
            "actual".to_string(),
        )
    }

    fn parsed(text: &str) -> KnownDiffs {
        KnownDiffs::parse(text).expect("rule parses")
    }

    fn parse_error(text: &str) -> String {
        let error = KnownDiffs::parse(text).expect_err("rule must be rejected");
        format!("{error:#}")
    }

    #[test]
    fn tier3_rule_allows_its_matching_tier3_diff() {
        let known = parsed(TIER3_RULE);
        assert_eq!(known.rule_count(), 1);
        assert!(known.allows(&tier3_entry("nodes", "function:alpha")));
    }

    #[test]
    fn tier3_rule_does_not_allow_a_different_surface() {
        let known = parsed(TIER3_RULE);
        assert!(!known.allows(&tier3_entry("edges", "function:alpha")));
    }

    #[test]
    fn tier3_rule_does_not_allow_a_non_matching_key() {
        let known = parsed(TIER3_RULE);
        assert!(!known.allows(&tier3_entry("nodes", "function:beta")));
    }

    #[test]
    fn tier3_rule_does_not_allow_the_same_key_at_tier1() {
        let known = parsed(TIER3_RULE);
        let tier1 = entry(
            Tier::Tier1,
            "nodes",
            "function:alpha",
            "expected".to_string(),
            "actual".to_string(),
        );
        assert!(!known.allows(&tier1));
    }

    #[test]
    fn tier3_rule_does_not_allow_the_same_key_at_tier2() {
        let known = parsed(TIER3_RULE);
        let tier2 = entry(
            Tier::Tier2,
            "nodes",
            "function:alpha",
            "expected".to_string(),
            "actual".to_string(),
        );
        assert!(!known.allows(&tier2));
    }

    #[test]
    fn wildcard_rule_never_allows_a_tier1_golden_difference() {
        let known = parsed("RULE tier=3 surface=nodes key=* justification=wildcard");
        let tier1 = entry(
            Tier::Tier1,
            "nodes",
            "function:anything",
            "expected".to_string(),
            "actual".to_string(),
        );
        assert!(!known.allows(&tier1));
        assert!(known.allows(&tier3_entry("nodes", "function:anything")));
    }

    #[test]
    fn tier1_and_tier2_rules_are_rejected_at_parse_time() {
        for token in ["1", "Tier1", "tier1", "2", "Tier2", "tier2"] {
            let line =
                format!("RULE tier={token} surface=nodes key=* justification=must-not-be-allowed");
            let message = parse_error(&line);
            assert!(
                message.contains("may not be allowlisted"),
                "tier={token} must be rejected at parse time, got: {message}"
            );
            assert!(
                message.contains(&line),
                "the error must name the offending line, got: {message}"
            );
        }
    }

    #[test]
    fn token_without_equals_is_rejected() {
        let message = parse_error("RULE garbage tier=3 surface=nodes key=* justification=x");
        assert!(
            message.contains("token garbage is not key=value"),
            "got: {message}"
        );
    }

    #[test]
    fn unknown_field_name_is_rejected() {
        let message = parse_error("RULE tier=3 surfce=nodes key=* justification=x");
        assert!(message.contains("unknown field surfce"), "got: {message}");
    }

    #[test]
    fn duplicate_field_is_rejected() {
        let message = parse_error("RULE tier=3 surface=nodes key=a key=b justification=x");
        assert!(message.contains("duplicate field key"), "got: {message}");
    }

    #[test]
    fn unknown_surface_is_rejected() {
        let message = parse_error("RULE tier=3 surface=noodes key=* justification=x");
        assert!(message.contains("unknown surface noodes"), "got: {message}");
        for surface in DIFF_SURFACES {
            let line = format!("RULE tier=3 surface={surface} key=* justification=x");
            assert_eq!(parsed(&line).rule_count(), 1, "{surface} must be accepted");
        }
    }

    #[test]
    fn missing_field_is_rejected() {
        for line in [
            "RULE surface=nodes key=* justification=x",
            "RULE tier=3 key=* justification=x",
            "RULE tier=3 surface=nodes justification=x",
            "RULE tier=3 surface=nodes key=*",
        ] {
            let message = parse_error(line);
            assert!(message.contains("missing"), "{line} -> {message}");
        }
    }

    #[test]
    fn fenced_template_and_prose_lines_are_not_rules() {
        let known = parsed(
            "# Known CodeGraph Equivalence Differences\n\nNo Tier-3 rules are active yet.\n\n\
             ```text\nRULE tier=3 surface=<surface> key=<substring-or-*> \
             justification=<short-token>\n```\n",
        );
        assert_eq!(known.rule_count(), 0);
    }

    #[test]
    fn a_rule_after_a_closed_fence_is_still_active() {
        let known = parsed(&format!(
            "```text\nRULE tier=3 surface=<x> key=* j=1\n```\n\n{TIER3_RULE}\n"
        ));
        assert_eq!(known.rule_count(), 1);
        assert!(known.allows(&tier3_entry("nodes", "function:alpha")));
    }

    #[test]
    fn unterminated_fence_is_rejected() {
        let message = parse_error(&format!("```text\n{TIER3_RULE}\n"));
        assert!(
            message.contains("unterminated code fence"),
            "got: {message}"
        );
    }

    #[test]
    fn committed_known_diffs_doc_parses_and_has_zero_active_rules() {
        let path = KnownDiffs::repo_doc_path();
        let known = KnownDiffs::load(&path).unwrap_or_else(|error| {
            panic!("{} must parse: {error:#}", path.display());
        });
        assert_eq!(
            known.rule_count(),
            0,
            "{} must have zero active Tier-3 rules; adding one silently widens \
             golden adjudication",
            path.display()
        );
    }

    #[test]
    fn invalid_known_diffs_file_fails_to_load() {
        let dir = std::env::temp_dir().join(format!(
            "codegraph-bench-known-diffs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir(&dir).expect("temp dir");
        let path = dir.join("KNOWN_DIFFS.md");
        fs::write(
            &path,
            "RULE tier=1 surface=nodes key=* justification=sneaky\n",
        )
        .expect("write fixture");

        let message = format!("{:#}", KnownDiffs::load(&path).expect_err("must fail"));
        let _ = fs::remove_dir_all(&dir);

        assert!(message.contains("may not be allowlisted"), "got: {message}");
    }

    #[test]
    fn tier1_entries_survive_the_allowlist_in_diff_canonical() {
        let expected = CanonicalDb {
            nodes: vec![row(&[("id", "function:alpha"), ("name", "alpha")])],
            edges: Vec::new(),
            unresolved_refs: Vec::new(),
            files: Vec::new(),
            schema: String::new(),
        };
        let mut actual = expected.clone();
        actual.nodes = vec![row(&[("id", "function:alpha"), ("name", "DRIFTED")])];

        let known = parsed("RULE tier=3 surface=nodes key=* justification=wildcard");
        let error = diff_canonical(&expected, &actual, Some(&known))
            .expect_err("a Tier-1 node drift is never allowlisted");
        assert!(
            error
                .entries()
                .iter()
                .any(|entry| entry.tier == Tier::Tier1 && entry.surface == "nodes")
        );
    }

    fn row(pairs: &[(&str, &str)]) -> CanonicalRow {
        pairs
            .iter()
            .map(|(key, value)| {
                (
                    (*key).to_string(),
                    serde_json::Value::String((*value).to_string()),
                )
            })
            .collect()
    }
}
