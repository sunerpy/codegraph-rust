//! Identifier-segment utilities for the prompt hook's graph-derived MEDIUM tier.
//!
//! Rust port of the PURE functions from upstream `src/search/identifier-segments.ts`
//! (#1136 `e699ee9` + #1145 plural-folding correctness from `35611b9`). NO
//! DB table: this project derives the MEDIUM-tier matches at QUERY time from the
//! existing indexed node names, so the golden `.schema` stays byte-identical
//! (no `name_segment_vocab` table, no migration).
//!
//! Symbol names are split into the words a human would use for them in prose,
//! and prompt prose is normalized into candidate words to look those segments
//! up with. "OrderStateMachine" → order / state / machine — so a prose prompt
//! naming the concept can be verified against the graph without a keyword list
//! ever knowing the words.

use std::collections::BTreeSet;

use regex::Regex;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

/// Bounds keep degenerate identifiers (minified names, hashes) from bloating
/// the segment set: segments outside them carry no prose signal anyway.
const MIN_SEGMENT_CHARS: usize = 2;
const MAX_SEGMENT_CHARS: usize = 32;
const MAX_SEGMENTS_PER_NAME: usize = 12;

/// Candidate cap + prose-word length bounds.
const MAX_PROSE_CANDIDATES: usize = 16;
const MIN_PROSE_CHARS: usize = 4; // "the"/"des"/"une"/"fix" out; "auth"/"flow"/"path" in
const MAX_PROSE_CHARS: usize = 24; // an unsegmented-script sentence is one giant run — skip it

/// Letter/digit run (Unicode).
static WORD_RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").expect("word-run regex is valid"));

/// Digit-only check.
static ALL_DIGITS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{N}+$").expect("all-digits regex is valid"));

/// Split a symbol or file name into lowercase word segments.
///
/// Handles camelCase / PascalCase (inner lower→Upper), acronym runs
/// ("HTMLParser" → html/parser), snake_case / kebab-case / dotted file names
/// (non-alphanumerics separate), and keeps digits glued to their word
/// ("base64Encode" → base64/encode). Digit-only fragments are dropped.
///
/// Upstream uses lookbehind/lookahead splits
/// (`(?<=[\p{Ll}\p{N}])(?=\p{Lu})|(?<=\p{Lu})(?=\p{Lu}\p{Ll})`). Rust `regex`
/// has no lookaround, so the same boundaries are computed by a manual
/// char-class scan over each `[\p{L}\p{N}]+` run.
pub fn split_identifier_segments(name: &str) -> Vec<String> {
    if name.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for run_match in WORD_RUN_RE.find_iter(name) {
        for part in split_run(run_match.as_str()) {
            if out.len() >= MAX_SEGMENTS_PER_NAME {
                return out;
            }
            let seg = part.to_lowercase();
            let len = seg.chars().count();
            if !(MIN_SEGMENT_CHARS..=MAX_SEGMENT_CHARS).contains(&len) {
                continue;
            }
            if ALL_DIGITS_RE.is_match(&seg) {
                continue;
            }
            if seen.insert(seg.clone()) {
                out.push(seg);
            }
        }
    }
    out
}

/// Split one alphanumeric run at camelCase humps and acronym→word boundaries,
/// emulating upstream's two lookaround alternatives:
/// - `(?<=[\p{Ll}\p{N}])(?=\p{Lu})` — a lowercase/digit followed by an uppercase
///   (camelCase hump: `orderState` → order|State).
/// - `(?<=\p{Lu})(?=\p{Lu}\p{Ll})` — an uppercase followed by uppercase+lowercase
///   (last acronym letter starts a new word: `HTMLParser` → HTML|Parser).
fn split_run(run: &str) -> Vec<String> {
    let chars: Vec<char> = run.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    for i in 0..chars.len() {
        if i > 0 {
            let prev = chars[i - 1];
            let cur = chars[i];
            let hump = (prev.is_lowercase() || prev.is_numeric()) && cur.is_uppercase();
            let acronym = prev.is_uppercase()
                && cur.is_uppercase()
                && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if hump || acronym {
                parts.push(std::mem::take(&mut current));
            }
        }
        current.push(chars[i]);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Combining-mark stripper (regex `\p{M}+`) — applied after NFD.
static COMBINING_MARK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{M}+").expect("combining-mark regex is valid"));

/// Normalize a prose word for segment lookup: NFD + strip combining marks +
/// lowercase, so "références" matches the segment "references" and "résolution"
/// matches "resolution". Identifier segments are overwhelmingly ASCII, so this
/// is what buys Latin-script languages their cross-lingual reach on loanwords.
pub fn normalize_prose_word(word: &str) -> String {
    let decomposed: String = word.nfd().collect();
    let stripped = COMBINING_MARK_RE.replace_all(&decomposed, "");
    stripped.to_lowercase()
}

/// English prompt words that are never evidence a symbol was NAMED, however
/// rare their segment happens to be in a given repo: function words, filler,
/// hyper-common dev verbs, and words ABOUT code rather than OF it. Measured FPs
/// that motivated this: "fix THIS typo" matched `resolveDeferredThisMemberRefs`,
/// "WRITE a haiku" matched `writeConfig`.
///
/// English-only ON PURPOSE — identifiers are written in English, so only English
/// prose words can accidentally collide with segments. Domain nouns ("state",
/// "checkout", "order") stay OUT — they are exactly the signal. Verbatim from
/// `identifier-segments.ts` `ENGLISH_PROSE_STOPWORDS`.
const ENGLISH_PROSE_STOPWORDS: &[&str] = &[
    "about",
    "above",
    "actually",
    "after",
    "again",
    "against",
    "almost",
    "along",
    "also",
    "always",
    "another",
    "anything",
    "around",
    "away",
    "back",
    "because",
    "been",
    "before",
    "behind",
    "being",
    "below",
    "best",
    "better",
    "between",
    "both",
    "cannot",
    "come",
    "could",
    "does",
    "doing",
    "done",
    "down",
    "each",
    "either",
    "else",
    "even",
    "ever",
    "every",
    "everything",
    "fine",
    "first",
    "from",
    "getting",
    "give",
    "goes",
    "going",
    "gone",
    "good",
    "great",
    "have",
    "having",
    "help",
    "here",
    "inside",
    "instead",
    "into",
    "just",
    "keep",
    "know",
    "last",
    "least",
    "less",
    "like",
    "likely",
    "little",
    "look",
    "looking",
    "made",
    "make",
    "making",
    "many",
    "maybe",
    "mind",
    "more",
    "most",
    "much",
    "must",
    "need",
    "needs",
    "never",
    "next",
    "nice",
    "none",
    "nothing",
    "okay",
    "only",
    "onto",
    "other",
    "otherwise",
    "over",
    "please",
    "pretty",
    "probably",
    "quite",
    "rather",
    "really",
    "right",
    "same",
    "seem",
    "seems",
    "should",
    "show",
    "since",
    "some",
    "someone",
    "something",
    "somewhere",
    "soon",
    "still",
    "such",
    "sure",
    "take",
    "than",
    "thank",
    "thanks",
    "that",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "things",
    "think",
    "this",
    "those",
    "though",
    "tried",
    "tries",
    "trying",
    "under",
    "until",
    "upon",
    "very",
    "want",
    "wants",
    "well",
    "went",
    "were",
    "what",
    "when",
    "which",
    "while",
    "will",
    "wish",
    "with",
    "within",
    "without",
    "would",
    "wrong",
    "your",
    "yours",
    // words ABOUT code, not OF it — present in a huge share of prompts while
    // almost never naming the symbol the user means
    "again",
    "change",
    "changes",
    "check",
    "class",
    "classes",
    "code",
    "detail",
    "details",
    "directory",
    "error",
    "errors",
    "example",
    "examples",
    "file",
    "files",
    "folder",
    "function",
    "functions",
    "issue",
    "issues",
    "line",
    "lines",
    "method",
    "methods",
    "name",
    "names",
    "problem",
    "problems",
    "project",
    "question",
    "questions",
    "rename",
    "test",
    "tests",
    "type",
    "types",
    "update",
    "value",
    "values",
    "warning",
    "warnings",
    "work",
    "working",
    "write",
    "writing",
];

fn is_stopword(word: &str) -> bool {
    ENGLISH_PROSE_STOPWORDS.contains(&word)
}

/// Candidate words from a prompt for segment lookup, in order of appearance:
/// Unicode letter/digit runs, normalized via [`normalize_prose_word`],
/// length-bounded, digit-only dropped, stopwords dropped, deduped, capped.
pub fn extract_prose_candidates(prompt: &str) -> Vec<String> {
    if prompt.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for run_match in WORD_RUN_RE.find_iter(prompt) {
        if out.len() >= MAX_PROSE_CANDIDATES {
            break;
        }
        let run = run_match.as_str();
        if run.chars().count() > MAX_PROSE_CHARS {
            continue;
        }
        let w = normalize_prose_word(run);
        let len = w.chars().count();
        if !(MIN_PROSE_CHARS..=MAX_PROSE_CHARS).contains(&len) {
            continue;
        }
        if ALL_DIGITS_RE.is_match(&w) {
            continue;
        }
        if is_stopword(&w) {
            continue;
        }
        if seen.insert(w.clone()) {
            out.push(w);
        }
    }
    out
}

/// Lookup variants for a prose word: the word itself plus light English-plural
/// folding, so common plurals still hit their singular segment. Keyed on
/// English plural spelling (#1145), in three classes:
/// - UNAMBIGUOUS `-es` (after x/sh/ss/zz: boxes, hashes, classes, quizzes) —
///   strip 2 only. Stripping 1 minted a bogus sibling ("classes" → classe).
/// - AMBIGUOUS endings (`-ches`/`-ses`/`-zes`/`-oes`): spelling alone can't
///   split patches(+es) from caches(+s) — emit BOTH candidate keys.
/// - Everything else ending in `-s` — a bare `-s` plural (services, machines):
///   strip 1 only. Stripping 2 minted "services" → servic.
///
/// A trailing `-ss` is a singular (class, process), not a plural: no strip.
pub fn segment_lookup_variants(word: &str) -> Vec<String> {
    let mut variants = vec![word.to_string()];
    let len = word.chars().count();
    let can_strip2 = len >= MIN_PROSE_CHARS + 2;
    let can_strip1 = len > MIN_PROSE_CHARS;
    // Byte-slice is safe: these suffixes are ASCII, so trailing 1/2 chars are
    // single-byte; guard on char count above avoids over-stripping short words.
    let strip = |n: usize| -> String {
        let mut chars: Vec<char> = word.chars().collect();
        chars.truncate(chars.len().saturating_sub(n));
        chars.into_iter().collect()
    };
    if ends_with_any(word, &["xes", "shes", "sses", "zzes"]) {
        if can_strip2 {
            variants.push(strip(2));
        }
    } else if ends_with_any(word, &["ches", "ses", "zes", "oes"]) {
        if can_strip2 {
            variants.push(strip(2));
        }
        if can_strip1 {
            variants.push(strip(1));
        }
    } else if word.ends_with('s') && !word.ends_with("ss") && can_strip1 {
        variants.push(strip(1));
    }
    variants
}

fn ends_with_any(word: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|s| word.ends_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_camel_and_acronym() {
        assert_eq!(
            split_identifier_segments("OrderStateMachine"),
            vec!["order", "state", "machine"]
        );
        assert_eq!(
            split_identifier_segments("HTMLParser"),
            vec!["html", "parser"]
        );
        assert_eq!(split_identifier_segments("get_user"), vec!["get", "user"]);
        assert_eq!(
            split_identifier_segments("base64Encode"),
            vec!["base64", "encode"]
        );
    }

    #[test]
    fn plural_folding_correctness() {
        // bare-s plural folds to singular, not -es sibling
        assert!(segment_lookup_variants("services").contains(&"service".to_string()));
        assert!(!segment_lookup_variants("services").contains(&"servic".to_string()));
        // unambiguous sibilant -es: strip 2 only, no bogus -s sibling
        assert!(segment_lookup_variants("classes").contains(&"class".to_string()));
        assert!(!segment_lookup_variants("classes").contains(&"classe".to_string()));
        // trailing -ss singular: no strip
        let class = segment_lookup_variants("class");
        assert_eq!(class, vec!["class".to_string()]);
        assert!(!class.contains(&"clas".to_string()));
        // ambiguous -ses: both keys
        let dbs = segment_lookup_variants("databases");
        assert!(dbs.contains(&"database".to_string()));
        assert!(dbs.contains(&"databases".to_string()) || dbs.contains(&"databas".to_string()));
    }

    #[test]
    fn prose_candidates_drop_stopwords() {
        let cands = extract_prose_candidates("fix this typo");
        // "fix" < 4 chars, "this" is a stopword, "typo" survives
        assert!(!cands.contains(&"this".to_string()));
        assert!(!cands.contains(&"fix".to_string()));
        assert!(cands.contains(&"typo".to_string()));
    }

    #[test]
    fn prose_candidates_normalize_diacritics() {
        let cands = extract_prose_candidates("références resolution");
        assert!(cands.contains(&"references".to_string()));
        assert!(cands.contains(&"resolution".to_string()));
    }

    #[test]
    fn prose_candidates_keep_domain_words() {
        let cands = extract_prose_candidates("checkout state machine");
        assert!(cands.contains(&"checkout".to_string()));
        assert!(cands.contains(&"state".to_string()));
        assert!(cands.contains(&"machine".to_string()));
    }
}
