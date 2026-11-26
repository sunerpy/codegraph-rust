//! What each synthesis pass reads (W9-00 row 27, OD-B90): its footprint,
//! recorded as it runs, and the records a stage run writes beside its edges.
//!
//! A pass reads the base graph only through [`super::stage::View`] and the
//! [`super::context::ViewContext`] it hands out, so those two record what
//! each call read, without the pass's code taking part:
//!
//! - **keys**: each lookup by id, file, name, kind, language, edge end, site
//!   or file list names the rows that would change its answer. A key is a
//!   64-bit hash of its parts; a key of a row a sync changed (both its old
//!   and its new image) that hashes the same as a recorded one reruns the
//!   pass, so a collision can only rerun a pass, never keep one.
//! - **predicates**: the pattern lookups (`GLOB`, `LIKE`, a signature that
//!   holds a string), kept whole and evaluated over the changed rows.
//! - **gates**: a pass that reads every file of a scope through
//!   [`super::stage::View::gated_texts`] records the files its per-file text
//!   check passed (the contributors); a file that fails the check is read
//!   and dropped, and only the check decides whether a changed file matters.
//! - **slices**: a pass that reads a node's lines through
//!   [`super::stage::View::text_lines`] records the lines and their hash.
//! - **unindexed reads**: the files the index does not hold, with what they
//!   read as (`synthesis_unindexed_inputs`, per pass).
//! - **untracked reads**: a probe on disk, a directory listing, a table the
//!   view cannot see into: the pass is not kept.
//! - **taint**: an indexed file read as drifted (H2): the pass is not kept.
//!
//! A route table a link pass builds is shared by every pass that reads it:
//! what its build read is kept with the table and joins each user's
//! footprint, whichever pass built it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::sync::{Mutex, PoisonError};

use codegraph_core::node_id::hash_content;
use codegraph_core::types::{Edge, Language};
use serde::{Deserialize, Serialize};

/// FNV-1a over `parts`, each followed by `0xff`, which no UTF-8 text holds.
pub(super) fn key(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0xff)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// The key families: what a lookup reads, and what a changed row of the
/// base graph moves.
pub(super) mod keys {
    use super::key;

    /// The node of `id`.
    pub(in crate::synthesis) fn node(id: &str) -> u64 {
        key(&["N", id])
    }
    /// Every node of `file`.
    pub(in crate::synthesis) fn file_nodes(file: &str) -> u64 {
        key(&["F", file])
    }
    /// The nodes of `kind` in `file`.
    pub(in crate::synthesis) fn file_kind(file: &str, kind: &str) -> u64 {
        key(&["FK", file, kind])
    }
    /// The nodes named `name`.
    pub(in crate::synthesis) fn name(name: &str) -> u64 {
        key(&["NM", name])
    }
    /// The nodes whose qualified name is `qualified`.
    pub(in crate::synthesis) fn qualified(qualified: &str) -> u64 {
        key(&["QN", qualified])
    }
    /// The nodes whose name SQLite's `lower` folds to `lower`.
    pub(in crate::synthesis) fn lower_name(lower: &str) -> u64 {
        key(&["LN", lower])
    }
    /// The nodes of `kind`, any language.
    pub(in crate::synthesis) fn kind(kind: &str) -> u64 {
        key(&["K", kind])
    }
    /// The nodes of `kind` written in `language`.
    pub(in crate::synthesis) fn kind_in(kind: &str, language: &str) -> u64 {
        key(&["KL", kind, language])
    }
    /// The list of every node name.
    pub(in crate::synthesis) fn names() -> u64 {
        key(&["NAMES"])
    }
    /// A node whose language moved under the same id.
    pub(in crate::synthesis) fn language_move() -> u64 {
        key(&["LANGMOVE"])
    }
    /// The base edges of `kind` into `target`.
    pub(in crate::synthesis) fn edges_into(target: &str, kind: &str) -> u64 {
        key(&["EI", target, kind])
    }
    /// The base edges of `kind` out of `source`.
    pub(in crate::synthesis) fn edges_from(source: &str, kind: &str) -> u64 {
        key(&["EO", source, kind])
    }
    /// Every base edge of `kind`.
    pub(in crate::synthesis) fn edges_of_kind(kind: &str) -> u64 {
        key(&["EK", kind])
    }
    /// Every unresolved reference of `kind`.
    pub(in crate::synthesis) fn refs_of_kind(kind: &str) -> u64 {
        key(&["UK", kind])
    }
    /// The stored references (unresolved and resolved) at a site.
    pub(in crate::synthesis) fn site(from: &str, line: i64, col: i64) -> u64 {
        key(&["SITE", from, &line.to_string(), &col.to_string()])
    }
    /// The list of indexed files.
    pub(in crate::synthesis) fn files() -> u64 {
        key(&["FILES"])
    }
    /// The indexed files of `language`.
    pub(in crate::synthesis) fn files_of(language: &str) -> u64 {
        key(&["FL", language])
    }
    /// The files-table row of `path`: whether it is indexed, its language
    /// and its `generated` flag.
    pub(in crate::synthesis) fn file_row(path: &str) -> u64 {
        key(&["FR", path])
    }
    /// The text of `path`, as the view reads it.
    pub(in crate::synthesis) fn text(path: &str) -> u64 {
        key(&["TX", path])
    }
}

/// A pattern lookup, kept whole and evaluated over the changed rows with
/// SQLite's own operators.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "lookup", rename_all = "camelCase")]
pub(super) enum Predicate {
    /// Functions and methods whose name matches a `GLOB`, a `LIKE` or one of
    /// the names ([`codegraph_store::Store::callables_named_like`]).
    Callables {
        globs: Vec<String>,
        likes: Vec<String>,
        names: Vec<String>,
    },
    /// Nodes of `kind` whose signature holds `text`
    /// (`instr(signature, text) > 0`).
    #[serde(rename_all = "camelCase")]
    Signature { node_kind: String, text: String },
    /// Nodes of `language` whose decorators match `pattern` under `LIKE`.
    Decorator { language: String, pattern: String },
}

/// A node's lines a pass read, and the hash of what it read there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct Slice {
    pub(super) path: String,
    pub(super) start: i64,
    pub(super) end: i64,
    /// One line read as `split('\n').nth(start - 1)`, rather than
    /// [`super::stage::slice_lines`] over `start..=end`.
    pub(super) line: bool,
    pub(super) hash: Option<String>,
}

/// The files a gate passed in one scan of a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct GateUse {
    pub(super) gate: String,
    pub(super) flags: u32,
    pub(super) contributors: Vec<String>,
}

/// What a gate reads of one file: its text, and what the pass knew when it
/// scanned (`flags`).
pub(super) struct GateInput<'a> {
    pub(super) text: &'a str,
    pub(super) flags: u32,
}

/// A pass's per-file text check: `scope` takes a file by its path and
/// language before its text is read, `check` by what [`GateInput`] holds. A
/// file it fails holds nothing the pass reads further.
pub(super) struct TextGate {
    pub(super) name: &'static str,
    pub(super) scope: fn(&str, Language) -> bool,
    pub(super) check: fn(&GateInput<'_>) -> bool,
}

/// What one pass (or one route table's build) read.
#[derive(Debug, Clone, Default)]
pub(super) struct Footprint {
    pub(super) keys: Vec<u64>,
    pub(super) predicates: BTreeSet<Predicate>,
    pub(super) slices: BTreeSet<Slice>,
    pub(super) gates: BTreeMap<(String, u32), BTreeSet<String>>,
    pub(super) unindexed: BTreeMap<String, Option<String>>,
    pub(super) untracked: BTreeSet<&'static str>,
    pub(super) tainted: bool,
}

impl Footprint {
    fn absorb(&mut self, other: &Footprint) {
        self.keys.extend_from_slice(&other.keys);
        self.predicates.extend(other.predicates.iter().cloned());
        self.slices.extend(other.slices.iter().cloned());
        for (gate, contributors) in &other.gates {
            self.gates
                .entry(gate.clone())
                .or_default()
                .extend(contributors.iter().cloned());
        }
        self.unindexed.extend(
            other
                .unindexed
                .iter()
                .map(|(path, hash)| (path.clone(), hash.clone())),
        );
        self.untracked.extend(other.untracked.iter().copied());
        self.tainted |= other.tainted;
    }

    /// Whether the pass's output may be kept by a later stage run.
    pub(super) fn reusable(&self) -> bool {
        self.untracked.is_empty() && !self.tainted
    }
}

/// Where a stage run's reads go: the pass running, and every route table
/// being built, which the pass reads through.
#[derive(Default)]
struct RecorderState {
    pass: Option<Footprint>,
    building: Vec<(&'static str, Footprint)>,
    tables: HashMap<&'static str, Footprint>,
}

/// The recorder of one stage run's reads (see the module documentation).
#[derive(Default)]
pub(super) struct Recorder {
    state: Mutex<RecorderState>,
}

impl Recorder {
    fn with(&self, record: impl Fn(&mut Footprint)) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(pass) = state.pass.as_mut() {
            record(pass);
        }
        for (_, table) in &mut state.building {
            record(table);
        }
    }

    pub(super) fn key(&self, key: u64) {
        self.with(|footprint| footprint.keys.push(key));
    }

    pub(super) fn keys(&self, keys: &[u64]) {
        self.with(|footprint| footprint.keys.extend_from_slice(keys));
    }

    pub(super) fn predicate(&self, predicate: Predicate) {
        self.with(|footprint| {
            footprint.predicates.insert(predicate.clone());
        });
    }

    pub(super) fn slice(&self, slice: Slice) {
        self.with(|footprint| {
            footprint.slices.insert(slice.clone());
        });
    }

    pub(super) fn gate(&self, gate: &str, flags: u32, contributors: &[String]) {
        self.with(|footprint| {
            footprint
                .gates
                .entry((gate.to_string(), flags))
                .or_default()
                .extend(contributors.iter().cloned());
        });
    }

    pub(super) fn unindexed(&self, path: &str, hash: Option<&str>) {
        self.with(|footprint| {
            footprint
                .unindexed
                .insert(path.to_string(), hash.map(str::to_string));
        });
    }

    pub(super) fn untracked(&self, what: &'static str) {
        self.with(|footprint| {
            footprint.untracked.insert(what);
        });
    }

    pub(super) fn taint(&self) {
        self.with(|footprint| footprint.tainted = true);
    }

    /// Start recording a pass.
    pub(super) fn begin_pass(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pass = Some(Footprint::default());
    }

    /// What the pass begun last read.
    pub(super) fn end_pass(&self) -> Footprint {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pass.take().unwrap_or_default()
    }

    /// A route table's build begins: what it reads is its own as well.
    pub(super) fn table_building(&self, router: &'static str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.building.push((router, Footprint::default()));
    }

    /// A route table's build ended.
    pub(super) fn table_built(&self, router: &'static str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = state
            .building
            .iter()
            .rposition(|(building, _)| *building == router)
        {
            let (_, footprint) = state.building.remove(at);
            state.tables.insert(router, footprint);
        }
    }

    /// A built route table is read again: what its build read joins the
    /// reader's footprint.
    pub(super) fn table_reused(&self, router: &'static str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(table) = state.tables.get(router).cloned() else {
            return;
        };
        if let Some(pass) = state.pass.as_mut() {
            pass.absorb(&table);
        }
        for (_, building) in &mut state.building {
            building.absorb(&table);
        }
    }
}

impl crate::frameworks::route_href::RouteTableObserver for Recorder {
    fn building(&self, router: &'static str) {
        self.table_building(router);
    }

    fn built(&self, router: &'static str) {
        self.table_built(router);
    }

    fn reused(&self, router: &'static str) {
        self.table_reused(router);
    }
}

/// `keys` sorted, deduplicated and written as fixed-width hex, so a key is
/// found by a binary search over the text.
pub(super) fn encode_keys(keys: &[u64]) -> String {
    let mut sorted = keys.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut text = String::with_capacity(sorted.len() * 16);
    for key in sorted {
        let _ = write!(text, "{key:016x}");
    }
    text
}

/// Whether [`encode_keys`]' `text` holds `key`; text of another shape holds
/// every key, so a record that does not read as written reruns its step.
pub(super) fn encoded_holds(text: &str, key: u64) -> bool {
    if !text.len().is_multiple_of(16) || !text.is_ascii() {
        return true;
    }
    let wanted = format!("{key:016x}");
    let (mut low, mut high) = (0, text.len() / 16);
    while low < high {
        let middle = (low + high) / 2;
        match text[middle * 16..middle * 16 + 16].cmp(wanted.as_str()) {
            std::cmp::Ordering::Less => low = middle + 1,
            std::cmp::Ordering::Greater => high = middle,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

/// The version of the pass records' layout and of what they hold: 2 since
/// each step's raw output has a row of its own (W9-00 row 27's fix round).
/// A head of another version, the single-row layout of version 1 among
/// them, runs the stage whole.
pub(super) const PASS_RECORD_VERSION: u32 = 2;

/// One step's record as a run makes it: its raw output in the order it
/// emitted it, and what it read. It is written as two rows
/// ([`Self::to_rows`]): its [`PassRecordHead`], `synthesis_pass:{pass}`,
/// and its raw output, `synthesis_pass:{pass}:edges`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PassRecord {
    /// Whether a later stage run may keep the output: no untracked read, no
    /// drifted file, and the rows within [`MAX_RECORD_BYTES`].
    pub(super) reusable: bool,
    pub(super) edges: Vec<Edge>,
    pub(super) keys: String,
    pub(super) predicates: Vec<Predicate>,
    pub(super) gates: Vec<GateUse>,
    pub(super) slices: Vec<Slice>,
    pub(super) unindexed: BTreeMap<String, Option<String>>,
    pub(super) untracked: Vec<String>,
}

/// A step's `synthesis_pass:{pass}` row: what the step read, whether it may
/// be kept, and the byte length and SHA-256 of the `:edges` row's stored text
/// written with it. A sync decides on the heads alone; it uses an `:edges`
/// row only after [`Self::holds`] says it is that row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PassRecordHead {
    pub(super) version: u32,
    pub(super) reusable: bool,
    pub(super) keys: String,
    pub(super) predicates: Vec<Predicate>,
    pub(super) gates: Vec<GateUse>,
    pub(super) slices: Vec<Slice>,
    pub(super) unindexed: BTreeMap<String, Option<String>>,
    pub(super) untracked: Vec<String>,
    pub(super) edges_bytes: u64,
    pub(super) edges_hash: String,
}

impl PassRecordHead {
    /// Whether `row` is the `:edges` row written with this head: its length
    /// and its SHA-256.
    pub(super) fn holds(&self, row: &str) -> bool {
        u64::try_from(row.len()).is_ok_and(|bytes| bytes == self.edges_bytes)
            && hash_content(row) == self.edges_hash
    }
}

/// A record's two rows, and its head as written.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RecordRows {
    pub(super) head: PassRecordHead,
    pub(super) head_row: String,
    pub(super) edges_row: String,
}

/// The size past which a pass's record keeps no output and is not reused:
/// its head and its output's JSON together.
pub(super) const MAX_RECORD_BYTES: usize = 32 << 20;

/// The deflate level of the output rows, fixed with the codec (miniz_oxide,
/// whatever backend another crate picks for flate2) so equal outputs give
/// byte-identical rows.
const OUTPUT_LEVEL: u8 = 6;

/// An `:edges` row's text for an output's JSON: the raw deflate stream of it
/// (miniz_oxide at [`OUTPUT_LEVEL`]) in standard base64 with padding.
#[doc(hidden)]
pub fn encode_output_row(json: &str) -> String {
    base64_encode(&miniz_oxide::deflate::compress_to_vec(
        json.as_bytes(),
        OUTPUT_LEVEL,
    ))
}

/// The output's JSON an `:edges` row holds: `None` for text that is not
/// base64, a stream that does not inflate within [`MAX_RECORD_BYTES`], or
/// bytes that are not UTF-8.
#[doc(hidden)]
pub fn decode_output_row(row: &str) -> Option<String> {
    let deflated = base64_decode(row)?;
    let json =
        miniz_oxide::inflate::decompress_to_vec_with_limit(&deflated, MAX_RECORD_BYTES).ok()?;
    String::from_utf8(json).ok()
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let at = |index: usize| u32::from(chunk.get(index).copied().unwrap_or(0));
        let group = (at(0) << 16) | (at(1) << 8) | at(2);
        let digit = |shift: u32| char::from(BASE64[((group >> shift) & 63) as usize]);
        text.push(digit(18));
        text.push(digit(12));
        text.push(if chunk.len() > 1 { digit(6) } else { '=' });
        text.push(if chunk.len() > 2 { digit(0) } else { '=' });
    }
    text
}

/// Standard base64 with padding; `None` for any other text.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(4) {
        return None;
    }
    let value = |digit: u8| -> Option<u32> {
        match digit {
            b'A'..=b'Z' => Some(u32::from(digit - b'A')),
            b'a'..=b'z' => Some(u32::from(digit - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(digit - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let groups = digits.len() / 4;
    let mut bytes = Vec::with_capacity(groups * 3);
    for (index, group) in digits.chunks(4).enumerate() {
        let padding = group
            .iter()
            .rev()
            .take_while(|digit| **digit == b'=')
            .count();
        if padding > 2 || (padding > 0 && index + 1 != groups) {
            return None;
        }
        let mut bits = 0_u32;
        for digit in &group[..4 - padding] {
            bits = (bits << 6) | value(*digit)?;
        }
        bits <<= 6 * padding as u32;
        let [_, first, second, third] = bits.to_be_bytes();
        bytes.push(first);
        if padding < 2 {
            bytes.push(second);
        }
        if padding < 1 {
            bytes.push(third);
        }
    }
    Some(bytes)
}

impl PassRecord {
    /// The record of a pass that emitted `edges` reading `footprint`.
    pub(super) fn new(edges: &[Edge], footprint: &Footprint) -> Self {
        Self {
            reusable: footprint.reusable(),
            edges: edges.to_vec(),
            keys: encode_keys(&footprint.keys),
            predicates: footprint.predicates.iter().cloned().collect(),
            gates: footprint
                .gates
                .iter()
                .map(|((gate, flags), contributors)| GateUse {
                    gate: gate.clone(),
                    flags: *flags,
                    contributors: contributors.iter().cloned().collect(),
                })
                .collect(),
            slices: footprint.slices.iter().cloned().collect(),
            unindexed: footprint.unindexed.clone(),
            untracked: footprint
                .untracked
                .iter()
                .map(|what| (*what).to_string())
                .collect(),
        }
    }

    /// The record's rows: its head and its `:edges` row (the output's JSON
    /// through [`encode_output_row`]), or, when the head and that JSON pass
    /// [`MAX_RECORD_BYTES`] together, a head that keeps nothing and is not
    /// reused, with the row of an empty output.
    pub(super) fn to_rows(&self) -> serde_json::Result<RecordRows> {
        let json = serde_json::to_string(&self.edges)?;
        let edges_row = encode_output_row(&json);
        let head = self.head_with(&edges_row);
        let head_row = serde_json::to_string(&head)?;
        if head_row.len() + json.len() <= MAX_RECORD_BYTES {
            return Ok(RecordRows {
                head,
                head_row,
                edges_row,
            });
        }
        let edges_row = encode_output_row("[]");
        let head = Self {
            reusable: false,
            edges: Vec::new(),
            keys: String::new(),
            predicates: Vec::new(),
            gates: Vec::new(),
            slices: Vec::new(),
            unindexed: self.unindexed.clone(),
            untracked: vec!["record-size".to_string()],
        }
        .head_with(&edges_row);
        Ok(RecordRows {
            head_row: serde_json::to_string(&head)?,
            head,
            edges_row,
        })
    }

    /// The head written with `edges_row`, this record's `:edges` row.
    fn head_with(&self, edges_row: &str) -> PassRecordHead {
        PassRecordHead {
            version: PASS_RECORD_VERSION,
            reusable: self.reusable,
            keys: self.keys.clone(),
            predicates: self.predicates.clone(),
            gates: self.gates.clone(),
            slices: self.slices.clone(),
            unindexed: self.unindexed.clone(),
            untracked: self.untracked.clone(),
            edges_bytes: u64::try_from(edges_row.len()).unwrap_or(u64::MAX),
            edges_hash: hash_content(edges_row),
        }
    }
}

/// The scheme of the `synthesis_passes` row.
pub(super) const STAGE_RECORD_SCHEME: u32 = 1;

/// The `synthesis_passes` row: what the pass records were written by and
/// for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StageRecord {
    pub(super) scheme: u32,
    /// The engine's marker value ([`super::VERSION`]).
    pub(super) engine: String,
    /// The build that wrote them ([`super::BUILD_IDENTITY`], H3).
    pub(super) build: String,
    /// The index schema they were written over (H3).
    pub(super) schema: i64,
    /// The digest of the options the stage reads.
    pub(super) options: String,
    /// The indexed files' languages, which open the passes' gates.
    pub(super) languages: Vec<String>,
    /// The prerequisite steps and passes that ran, in order.
    pub(super) passes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record is written as two rows (W9-00 row 27's fix round): its head,
    /// which holds no output and names the `:edges` row's stored text by
    /// length and SHA-256, and the output in the order the step emitted it,
    /// its JSON deflated and in base64. A row of the same length with other
    /// text is not the one; past the size bound the head keeps nothing, is
    /// not reused, and names the row of an empty output.
    #[test]
    fn a_record_head_names_its_output_row() {
        use codegraph_core::types::EdgeKind;
        let edge = |source: &str, target: &str| Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind: EdgeKind::Calls,
            metadata: Some(serde_json::json!({ "synthesizedBy": "callback" })),
            line: None,
            col: None,
            provenance: Some("heuristic".to_string()),
        };
        let record = PassRecord {
            reusable: true,
            edges: vec![edge("method:b", "method:a"), edge("method:a", "method:c")],
            keys: encode_keys(&[keys::node("method:a")]),
            predicates: Vec::new(),
            gates: Vec::new(),
            slices: Vec::new(),
            unindexed: BTreeMap::new(),
            untracked: Vec::new(),
        };
        let rows = record.to_rows().unwrap();
        assert_eq!(
            decode_output_row(&rows.edges_row).unwrap(),
            serde_json::to_string(&record.edges).unwrap()
        );
        assert_eq!(record.to_rows().unwrap(), rows, "equal input, equal rows");
        assert_eq!(
            serde_json::from_str::<PassRecordHead>(&rows.head_row).unwrap(),
            rows.head
        );
        assert_eq!(rows.head.version, PASS_RECORD_VERSION);
        assert!(rows.head.reusable);
        assert!(!rows.head_row.contains("method:c"), "{}", rows.head_row);
        assert!(rows.head.holds(&rows.edges_row));
        let mut other = rows.edges_row.clone().into_bytes();
        other[4] = if other[4] == b'A' { b'B' } else { b'A' };
        let other = String::from_utf8(other).unwrap();
        assert_eq!(other.len(), rows.edges_row.len());
        assert!(!rows.head.holds(&other));
        assert!(!rows.head.holds(&encode_output_row("[]")));

        let oversized = PassRecord {
            keys: "0".repeat(MAX_RECORD_BYTES),
            ..record
        };
        let rows = oversized.to_rows().unwrap();
        assert_eq!(decode_output_row(&rows.edges_row).as_deref(), Some("[]"));
        assert!(!rows.head.reusable);
        assert_eq!(rows.head.untracked, ["record-size"]);
        assert!(rows.head.keys.is_empty());
        assert!(rows.head.holds(&encode_output_row("[]")));
    }

    /// Standard base64 with padding (RFC 4648's vectors), and nothing else:
    /// a wrong length, a digit outside the alphabet, padding of three or
    /// before the last group.
    #[test]
    fn output_rows_are_standard_base64() {
        for (bytes, text) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(bytes.as_bytes()), text);
            assert_eq!(base64_decode(text).as_deref(), Some(bytes.as_bytes()));
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&all)), Some(all));
        for bad in ["Zg=", "Zm9v!", "Z===", "Zg==Zg==", "Zm=v"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }

    /// A row that does not decode reads as nothing: not base64, a stream
    /// that does not inflate (a reserved block type), bytes that are not
    /// UTF-8.
    #[test]
    fn an_output_row_that_does_not_decode_reads_as_nothing() {
        assert_eq!(decode_output_row("not base64!"), None);
        assert_eq!(decode_output_row(&base64_encode(&[0x07, 0, 0])), None);
        let not_utf8 = miniz_oxide::deflate::compress_to_vec(&[0xff, 0xfe], OUTPUT_LEVEL);
        assert_eq!(decode_output_row(&base64_encode(&not_utf8)), None);
        assert_eq!(
            decode_output_row(&encode_output_row("[]")).as_deref(),
            Some("[]")
        );
    }

    /// The codec's output is pinned (a known answer): a miniz_oxide that
    /// deflates these bytes otherwise fails here, beside moving the build
    /// identity the records are written under.
    #[test]
    fn the_output_codec_gives_a_known_answer() {
        let json = r#"[{"id":null,"source":"method:a","target":"method:b","kind":"calls","metadata":{"synthesizedBy":"callback"},"line":null,"col":null,"provenance":"heuristic"}]"#;
        assert_eq!(encode_output_row(json), KNOWN_ANSWER);
        assert_eq!(decode_output_row(KNOWN_ANSWER).as_deref(), Some(json));
    }

    /// The test's JSON deflated by miniz_oxide 0.8.9 at level 6, in base64
    /// (zlib's raw inflate reads it back to the same JSON).
    const KNOWN_ANSWER: &str = "RY1NCsIwEIXv8tY5QZZeQ1xMk8EMHSeSTIRaeneDWNy9X77rDsmINlQDeh0tMSIe7KXmSAhwanf2f7bMbBWbHyRS7dPOhjI5Ie7om3nhLm/Ol+23WSitOAJUjE9UqnrKZ6svNrIvufBo0l0SjtsH";

    /// A key is found in its encoding and a key that is not is not; text
    /// that does not read as an encoding holds every key.
    #[test]
    fn encoded_keys_answer_membership() {
        let keys = [keys::node("a"), keys::name("b"), keys::node("a"), 7, 0];
        let text = encode_keys(&keys);
        for key in keys {
            assert!(encoded_holds(&text, key));
        }
        assert!(!encoded_holds(&text, keys::node("c")));
        assert!(!encoded_holds("", keys::node("c")));
        assert!(encoded_holds("not hex", keys::node("c")));
    }

    /// Keys are written sorted, once each, sixteen hex digits a key.
    #[test]
    fn encoded_keys_are_sorted_and_distinct() {
        let text = encode_keys(&[keys::node("a"), 7, keys::node("a"), 0]);
        assert_eq!(text.len(), 3 * 16);
        let written: Vec<&str> = (0..3).map(|at| &text[at * 16..at * 16 + 16]).collect();
        assert_eq!(written[0], "0000000000000000");
        assert_eq!(written[1], "0000000000000007");
        assert!(written[1] < written[2]);
    }

    /// The key families do not collide on shared text.
    #[test]
    fn key_families_are_distinct() {
        let all = [
            keys::node("x"),
            keys::file_nodes("x"),
            keys::name("x"),
            keys::qualified("x"),
            keys::lower_name("x"),
            keys::kind("x"),
            keys::files_of("x"),
            keys::file_row("x"),
            keys::text("x"),
            keys::refs_of_kind("x"),
            keys::edges_of_kind("x"),
            keys::file_kind("x", "y"),
            keys::kind_in("x", "y"),
            keys::edges_into("x", "y"),
            keys::edges_from("x", "y"),
        ];
        let distinct: BTreeSet<u64> = all.iter().copied().collect();
        assert_eq!(distinct.len(), all.len());
        assert_ne!(key(&["ab", "c"]), key(&["a", "bc"]));
    }

    /// What a route table's build reads joins the pass that built it and
    /// every pass that reads it later.
    #[test]
    fn a_route_table_footprint_joins_each_reader() {
        let recorder = Recorder::default();
        recorder.begin_pass();
        recorder.key(1);
        recorder.table_building("next");
        recorder.key(2);
        recorder.table_built("next");
        let builder = recorder.end_pass();
        assert_eq!(builder.keys, [1, 2]);
        recorder.begin_pass();
        recorder.table_reused("next");
        recorder.key(3);
        let reader = recorder.end_pass();
        assert_eq!(reader.keys, [2, 3]);
    }
}
