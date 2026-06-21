use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::canonicalize::{CanonicalDb, CanonicalRow};

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

    pub fn parse(text: &str) -> Result<Self> {
        let mut rules = Vec::new();
        for (line_number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || !line.starts_with("RULE ") {
                continue;
            }
            rules.push(parse_rule(line).with_context(|| {
                format!("parsing KNOWN_DIFFS.md line {}: {line}", line_number + 1)
            })?);
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

fn parse_rule(line: &str) -> Result<KnownDiffRule> {
    let mut fields = BTreeMap::new();
    for token in line.trim_start_matches("RULE ").split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            fields.insert(key, value);
        }
    }
    let tier = match *fields.get("tier").context("missing tier")? {
        "3" | "Tier3" | "tier3" => Tier::Tier3,
        "2" | "Tier2" | "tier2" => Tier::Tier2,
        "1" | "Tier1" | "tier1" => Tier::Tier1,
        value => anyhow::bail!("unknown tier {value}"),
    };
    Ok(KnownDiffRule {
        tier,
        surface: fields
            .get("surface")
            .context("missing surface")?
            .to_string(),
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
