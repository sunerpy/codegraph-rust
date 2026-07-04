use std::collections::HashSet;

use codegraph_core::types::NodeKind;

pub fn normalize_name_token(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

pub const STOP_WORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "and",
    "or",
    "but",
    "in",
    "on",
    "at",
    "to",
    "for",
    "of",
    "with",
    "by",
    "from",
    "is",
    "it",
    "that",
    "this",
    "are",
    "was",
    "be",
    "has",
    "had",
    "have",
    "do",
    "does",
    "did",
    "will",
    "would",
    "could",
    "should",
    "may",
    "might",
    "can",
    "shall",
    "not",
    "no",
    "all",
    "each",
    "every",
    "how",
    "what",
    "where",
    "when",
    "who",
    "which",
    "why",
    "i",
    "me",
    "my",
    "we",
    "our",
    "you",
    "your",
    "he",
    "she",
    "they",
    "show",
    "give",
    "tell",
    "been",
    "done",
    "made",
    "used",
    "using",
    "work",
    "works",
    "found",
    "also",
    "into",
    "then",
    "than",
    "just",
    "more",
    "some",
    "such",
    "over",
    "only",
    "out",
    "its",
    "so",
    "up",
    "as",
    "if",
    "look",
    "need",
    "needs",
    "want",
    "happen",
    "happens",
    "affect",
    "affected",
    "break",
    "breaks",
    "failing",
    "implemented",
    "implement",
    "code",
    "file",
    "files",
    "function",
    "method",
    "class",
    "type",
    "fix",
    "bug",
    "called",
];

fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.contains(&word)
}

pub fn get_stem_variants(term: &str) -> Vec<String> {
    let mut variants: Vec<String> = Vec::new();
    let push = |variants: &mut Vec<String>, value: String| {
        if !variants.contains(&value) {
            variants.push(value);
        }
    };
    let t = term.to_lowercase();
    let chars: Vec<char> = t.chars().collect();
    let len = chars.len();
    let slice = |from: usize, to: usize| -> String { chars[from..to].iter().collect() };

    if t.ends_with("ing") && len > 5 {
        let base = slice(0, len - 3);
        push(&mut variants, base.clone());
        push(&mut variants, format!("{base}e"));
        let base_chars: Vec<char> = base.chars().collect();
        if base_chars.len() >= 2
            && base_chars[base_chars.len() - 1] == base_chars[base_chars.len() - 2]
        {
            push(
                &mut variants,
                base_chars[..base_chars.len() - 1].iter().collect(),
            );
        }
    }

    if (t.ends_with("tion") || t.ends_with("sion")) && len > 5 {
        push(&mut variants, slice(0, len - 3));
    }

    if t.ends_with("ment") && len > 6 {
        push(&mut variants, slice(0, len - 4));
    }

    if t.ends_with("ies") && len > 4 {
        push(&mut variants, format!("{}y", slice(0, len - 3)));
    } else if t.ends_with("es") && len > 4 {
        push(&mut variants, slice(0, len - 2));
    } else if t.ends_with('s') && !t.ends_with("ss") && len > 4 {
        push(&mut variants, slice(0, len - 1));
    }

    if t.ends_with("ed") && !t.ends_with("eed") && len > 4 {
        push(&mut variants, slice(0, len - 1));
        push(&mut variants, slice(0, len - 2));
        if t.ends_with("ied") && len > 5 {
            push(&mut variants, format!("{}y", slice(0, len - 3)));
        }
    }

    if t.ends_with("er") && len > 4 {
        let base = slice(0, len - 2);
        push(&mut variants, base.clone());
        push(&mut variants, format!("{base}e"));
        let base_chars: Vec<char> = base.chars().collect();
        if base_chars.len() >= 2
            && base_chars[base_chars.len() - 1] == base_chars[base_chars.len() - 2]
        {
            push(
                &mut variants,
                base_chars[..base_chars.len() - 1].iter().collect(),
            );
        }
    }

    variants
        .into_iter()
        .filter(|v| v.chars().count() >= 3 && v != &t)
        .collect()
}

fn insert_camel_boundaries(query: &str) -> String {
    let chars: Vec<char> = query.chars().collect();
    let mut out = String::new();
    for i in 0..chars.len() {
        let c = chars[i];
        if i > 0 {
            let prev = chars[i - 1];
            let lower_to_upper = prev.is_ascii_lowercase() && c.is_ascii_uppercase();
            let acronym_boundary = prev.is_ascii_uppercase()
                && c.is_ascii_uppercase()
                && i + 1 < chars.len()
                && chars[i + 1].is_ascii_lowercase();
            if lower_to_upper || acronym_boundary {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out
}

fn find_compound_identifiers(query: &str) -> Vec<String> {
    let chars: Vec<char> = query.chars().collect();
    let n = chars.len();
    let is_word = |c: char| c.is_ascii_alphanumeric();
    let mut results = Vec::new();
    let mut i = 0usize;
    while i < n {
        let at_boundary = i == 0 || !is_word(chars[i - 1]);
        if at_boundary && chars[i].is_ascii_alphabetic() {
            let start = i;
            let mut j = i;
            while j < n && is_word(chars[j]) {
                j += 1;
            }
            let ends_at_boundary = j >= n || !is_word(chars[j]);
            if ends_at_boundary {
                let word: String = chars[start..j].iter().collect();
                if is_compound_word(&word) && word.chars().count() >= 3 {
                    results.push(word.to_lowercase());
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    results
}

fn is_compound_word(word: &str) -> bool {
    let wc: Vec<char> = word.chars().collect();
    let n = wc.len();
    if n == 0 || !wc[0].is_ascii_alphabetic() {
        return false;
    }
    if wc[0].is_ascii_lowercase() {
        let mut k = 1usize;
        while k < n {
            if wc[k].is_ascii_uppercase() {
                let mut m = k + 1;
                let mut count = 0usize;
                while m < n && wc[m].is_ascii_lowercase() {
                    m += 1;
                    count += 1;
                }
                if count >= 1 {
                    return true;
                }
                k = m;
            } else {
                k += 1;
            }
        }
        false
    } else {
        let mut idx = 1usize;
        while idx < n && wc[idx].is_ascii_lowercase() {
            idx += 1;
        }
        if idx == 1 {
            return false;
        }
        idx < n && wc[idx].is_ascii_uppercase()
    }
}

fn find_snake_identifiers(query: &str) -> Vec<String> {
    let chars: Vec<char> = query.chars().collect();
    let n = chars.len();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut results = Vec::new();
    let mut i = 0usize;
    while i < n {
        let at_boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        if at_boundary && chars[i].is_ascii_alphabetic() {
            let start = i;
            let mut j = i;
            while j < n && is_word(chars[j]) {
                j += 1;
            }
            let word: String = chars[start..j].iter().collect();
            let wc: Vec<char> = word.chars().collect();
            if wc.contains(&'_')
                && wc[0] != '_'
                && wc[wc.len() - 1] != '_'
                && word.chars().count() >= 3
            {
                results.push(word.to_lowercase());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    results
}

pub fn extract_search_terms(query: &str, include_stems: bool) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let add = |tokens: &mut Vec<String>, seen: &mut HashSet<String>, value: String| {
        if seen.insert(value.clone()) {
            tokens.push(value);
        }
    };

    for compound in find_compound_identifiers(query) {
        add(&mut tokens, &mut seen, compound);
    }
    for snake in find_snake_identifiers(query) {
        add(&mut tokens, &mut seen, snake);
    }

    let camel_split = insert_camel_boundaries(query);
    let normalised: String = camel_split
        .chars()
        .map(|c| if c == '_' || c == '.' { ' ' } else { c })
        .collect();

    for word in normalised.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        if lower.chars().count() < 3 {
            continue;
        }
        if is_stop_word(&lower) {
            continue;
        }
        add(&mut tokens, &mut seen, lower);
    }

    if include_stems {
        let mut stems: Vec<String> = Vec::new();
        let mut stem_seen: HashSet<String> = HashSet::new();
        for token in &tokens {
            for variant in get_stem_variants(token) {
                if !seen.contains(&variant)
                    && !is_stop_word(&variant)
                    && stem_seen.insert(variant.clone())
                {
                    stems.push(variant);
                }
            }
        }
        for stem in stems {
            add(&mut tokens, &mut seen, stem);
        }
    }

    tokens
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

fn dirname(path: &str) -> String {
    let normalized = path.trim_end_matches(['/', '\\']);
    match normalized.rfind(['/', '\\']) {
        Some(idx) => {
            if idx == 0 {
                "/".to_string()
            } else {
                normalized[..idx].to_string()
            }
        }
        None => ".".to_string(),
    }
}

pub fn score_path_relevance(
    file_path: &str,
    query: &str,
    project_name_tokens: &HashSet<String>,
) -> f64 {
    let path_lower = file_path.to_lowercase();
    let file_name = basename(file_path).to_lowercase();
    let dir_name = dirname(file_path).to_lowercase();
    let mut score = 0.0f64;

    let all_words: Vec<&str> = query.split_whitespace().filter(|w| !w.is_empty()).collect();
    if all_words.is_empty() {
        return 0.0;
    }

    let filtered: Vec<&str> = if !project_name_tokens.is_empty() {
        all_words
            .iter()
            .copied()
            .filter(|w| !project_name_tokens.contains(&normalize_name_token(w)))
            .collect()
    } else {
        all_words.clone()
    };
    let scored = if filtered.is_empty() {
        all_words
    } else {
        filtered
    };

    for word in scored {
        let subtokens = extract_search_terms(word, false);
        if subtokens.is_empty() {
            continue;
        }
        if subtokens.iter().any(|t| file_name.contains(t.as_str())) {
            score += 10.0;
        }
        if subtokens.iter().any(|t| dir_name.contains(t.as_str())) {
            score += 5.0;
        } else if subtokens.iter().any(|t| path_lower.contains(t.as_str())) {
            score += 3.0;
        }
    }

    let query_lower = query.to_lowercase();
    let is_test_query = query_lower.contains("test") || query_lower.contains("spec");
    if !is_test_query && is_test_file(file_path) {
        score -= 15.0;
    }

    score
}

const NON_PRODUCTION_DIRS: &[&str] = &[
    "integration",
    "sample",
    "samples",
    "example",
    "examples",
    "fixture",
    "fixtures",
    "benchmark",
    "benchmarks",
    "demo",
    "demos",
];

fn matches_non_production_dir(lower_path: &str) -> bool {
    NON_PRODUCTION_DIRS.iter().any(|dir| {
        lower_path.contains(&format!("/{dir}/")) || lower_path.starts_with(&format!("{dir}/"))
    })
}

pub fn is_test_file(file_path: &str) -> bool {
    let lower = file_path.to_lowercase();
    let file_name = basename(file_path);
    let lower_name = file_name.to_lowercase();

    if lower_name.starts_with("test_")
        || lower_name.starts_with("test.")
        || matches_separator_test(&lower_name)
        || matches_camel_test_suffix(&file_name)
    {
        return true;
    }

    if lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || lower.contains("/specs/")
        || lower.contains("/testlib/")
        || lower.contains("/testing/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
        || lower.starts_with("spec/")
        || lower.starts_with("specs/")
        || matches_camel_test_dir(file_path)
    {
        return true;
    }

    matches_non_production_dir(&lower)
}

fn matches_separator_test(lower_name: &str) -> bool {
    let chars: Vec<char> = lower_name.chars().collect();
    let dot = match lower_name.rfind('.') {
        Some(idx) => idx,
        None => return false,
    };
    let ext: String = lower_name[dot + 1..].to_string();
    if ext.is_empty()
        || !ext
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return false;
    }
    let stem = &lower_name[..dot];
    let stem_chars: Vec<char> = stem.chars().collect();
    for marker in ["test", "tests", "spec", "specs"] {
        if let Some(pos) = stem.rfind(marker)
            && pos + marker.len() == stem.len()
            && pos > 0
        {
            let sep = stem_chars[pos - 1];
            if sep == '.' || sep == '_' || sep == '-' {
                return true;
            }
        }
    }
    let _ = chars;
    false
}

fn matches_camel_test_suffix(file_name: &str) -> bool {
    let dot = match file_name.rfind('.') {
        Some(idx) => idx,
        None => return false,
    };
    let ext = &file_name[dot + 1..];
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let stem = &file_name[..dot];
    for marker in ["TestCase", "Tests", "Tester", "Test", "Specs", "Spec"] {
        if stem.ends_with(marker) {
            return true;
        }
    }
    false
}

fn matches_camel_test_dir(file_path: &str) -> bool {
    let chars: Vec<char> = file_path.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    while i < n {
        let at_segment_start = i == 0 || chars[i - 1] == '/';
        if at_segment_start {
            let mut j = i;
            while j < n && chars[j].is_ascii_alphanumeric() {
                j += 1;
            }
            if j < n && chars[j] == '/' {
                let segment: String = chars[i..j].iter().collect();
                for marker in ["Tests", "Test", "Spec"] {
                    if segment.ends_with(marker) {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

pub fn name_match_bonus(node_name: &str, query: &str) -> f64 {
    let name_lower = node_name.to_lowercase();

    let camel = insert_camel_boundaries(query);
    let raw_terms: Vec<String> = camel
        .split(|c: char| c.is_whitespace() || c == '_' || c == '.' || c == '-')
        .map(|t| t.to_lowercase())
        .filter(|t| t.chars().count() >= 2)
        .collect();

    let query_tokens: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| t.chars().count() >= 2)
        .collect();

    let query_lower: String = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();

    if name_lower == query_lower {
        return 80.0;
    }

    if query_tokens.len() > 1 && query_tokens.iter().any(|t| t == &name_lower) {
        return 60.0;
    }

    if name_lower.starts_with(&query_lower) && !query_lower.is_empty() {
        let ratio = query_lower.chars().count() as f64 / name_lower.chars().count() as f64;
        return (10.0 + 30.0 * ratio).round();
    }

    if raw_terms.len() > 1 {
        let all_match = raw_terms.iter().all(|t| name_lower.contains(t.as_str()));
        if all_match {
            return 15.0;
        }
    }

    if !query_lower.is_empty() && name_lower.contains(&query_lower) {
        return 10.0;
    }

    0.0
}

pub fn kind_bonus(kind: NodeKind) -> f64 {
    match kind {
        NodeKind::Function => 10.0,
        NodeKind::Method => 10.0,
        NodeKind::Class => 8.0,
        NodeKind::Interface => 9.0,
        NodeKind::TypeAlias => 6.0,
        NodeKind::Struct => 6.0,
        NodeKind::Trait => 9.0,
        NodeKind::Enum => 5.0,
        NodeKind::Component => 8.0,
        NodeKind::Route => 9.0,
        NodeKind::Module => 4.0,
        NodeKind::Property => 3.0,
        NodeKind::Field => 3.0,
        NodeKind::Variable => 2.0,
        NodeKind::Constant => 3.0,
        NodeKind::Import => 1.0,
        NodeKind::Export => 1.0,
        NodeKind::Parameter => 0.0,
        NodeKind::Namespace => 4.0,
        NodeKind::File => 0.0,
        NodeKind::Protocol => 9.0,
        NodeKind::EnumMember => 3.0,
    }
}

pub fn is_distinctive_identifier(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if token.chars().any(|c| c == '_' || c.is_ascii_digit()) {
        return true;
    }
    token.chars().skip(1).any(|c| c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(strs: &[&str]) -> HashSet<String> {
        strs.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn normalize_name_token_lowercases_and_strips_non_alnum() {
        assert_eq!(normalize_name_token("My-App_2!"), "myapp2");
        assert_eq!(normalize_name_token(""), "");
        assert_eq!(normalize_name_token("...--"), "");
    }

    #[test]
    fn stem_variants_ing_drops_suffix_adds_e_and_dedup() {
        // "running" > 5, ends ing → base "runn", "runne", plus doubled-consonant "run".
        let v = get_stem_variants("running");
        assert!(v.contains(&"runn".to_string()));
        assert!(v.contains(&"runne".to_string()));
        assert!(v.contains(&"run".to_string()));
        // never returns the original term
        assert!(!v.contains(&"running".to_string()));
    }

    #[test]
    fn stem_variants_tion_sion_ment_and_plurals() {
        assert!(get_stem_variants("creation").contains(&"creat".to_string()));
        assert!(get_stem_variants("decision").contains(&"decis".to_string()));
        assert!(get_stem_variants("agreement").contains(&"agree".to_string()));
        // ies → y
        assert!(get_stem_variants("queries").contains(&"query".to_string()));
        // es → drop
        assert!(get_stem_variants("boxes").contains(&"box".to_string()));
        // trailing s (not ss) → drop
        assert!(get_stem_variants("tokens").contains(&"token".to_string()));
        // ss is NOT stripped
        assert!(!get_stem_variants("class").contains(&"clas".to_string()));
    }

    #[test]
    fn stem_variants_ed_and_ied_and_er() {
        // ed (not eed): drops 1 and 2 → "walke"/"walk"
        let ed = get_stem_variants("walked");
        assert!(ed.contains(&"walk".to_string()));
        // ied → y
        assert!(get_stem_variants("copied").contains(&"copy".to_string()));
        // er variants: "parser" → "pars"/"parse"
        let er = get_stem_variants("parser");
        assert!(er.contains(&"parse".to_string()));
        // doubled-consonant "er": "runner" → "run"
        assert!(get_stem_variants("runner").contains(&"run".to_string()));
        // "eed" excluded
        assert!(!get_stem_variants("feed").contains(&"fe".to_string()));
    }

    #[test]
    fn stem_variants_too_short_returns_empty() {
        assert!(get_stem_variants("ing").is_empty());
        assert!(get_stem_variants("er").is_empty());
    }

    #[test]
    fn extract_search_terms_splits_camel_snake_and_compound() {
        // camelCase → get + user + data (compound "getUserData" too)
        let t = extract_search_terms("getUserData", false);
        assert!(t.contains(&"getuserdata".to_string()));
        assert!(t.contains(&"user".to_string()));
        assert!(t.contains(&"data".to_string()));
    }

    #[test]
    fn extract_search_terms_snake_case_identifier() {
        let t = extract_search_terms("parse_query_string", false);
        assert!(t.contains(&"parse_query_string".to_string()));
        assert!(t.contains(&"parse".to_string()));
        assert!(t.contains(&"query".to_string()));
        assert!(t.contains(&"string".to_string()));
    }

    #[test]
    fn extract_search_terms_drops_stop_words_and_short_tokens() {
        // "the" and "of" are stop words; "at" is < 3 chars.
        let t = extract_search_terms("the size of at", false);
        assert!(!t.contains(&"the".to_string()));
        assert!(!t.contains(&"of".to_string()));
        assert!(!t.contains(&"at".to_string()));
        assert!(t.contains(&"size".to_string()));
    }

    #[test]
    fn extract_search_terms_acronym_boundary_split() {
        // "HTTPServer" → acronym boundary before "Server".
        let t = extract_search_terms("HTTPServer", false);
        assert!(t.contains(&"http".to_string()));
        assert!(t.contains(&"server".to_string()));
    }

    #[test]
    fn extract_search_terms_with_stems_adds_variants() {
        let with = extract_search_terms("running", true);
        let without = extract_search_terms("running", false);
        assert!(with.len() >= without.len());
        assert!(with.contains(&"run".to_string()));
    }

    #[test]
    fn extract_search_terms_empty_query_is_empty() {
        assert!(extract_search_terms("", false).is_empty());
        assert!(extract_search_terms("   ", false).is_empty());
    }

    #[test]
    fn score_path_relevance_filename_beats_dir_beats_path() {
        let empty = HashSet::new();
        // token in filename → +10
        let fname = score_path_relevance("src/auth/login.ts", "login", &empty);
        // token in dir → +5
        let dir = score_path_relevance("src/login/handler.ts", "login", &empty);
        assert!(
            fname > dir,
            "filename hit outranks dir hit: {fname} vs {dir}"
        );
        assert!(dir > 0.0);
    }

    #[test]
    fn score_path_relevance_path_only_hit_scores_three() {
        let empty = HashSet::new();
        // "widget" appears only deep in the path segment, not filename/immediate dir.
        let score = score_path_relevance("app/widget/nested/main.ts", "widget", &empty);
        assert!(score >= 3.0);
    }

    #[test]
    fn score_path_relevance_empty_query_is_zero() {
        let empty = HashSet::new();
        assert_eq!(score_path_relevance("src/a.ts", "", &empty), 0.0);
        assert_eq!(score_path_relevance("src/a.ts", "   ", &empty), 0.0);
    }

    #[test]
    fn score_path_relevance_test_file_penalized_unless_test_query() {
        let empty = HashSet::new();
        // Non-test query on a test file → -15 penalty pushes it negative.
        let penalized = score_path_relevance("src/tests/foo.rs", "foo", &empty);
        assert!(penalized < 10.0, "test file penalized: {penalized}");
        // Test query on a test file → no penalty.
        let ok = score_path_relevance("src/tests/foo.rs", "test foo", &empty);
        assert!(ok >= penalized);
    }

    #[test]
    fn score_path_relevance_project_tokens_filtered_out() {
        // If every query word is a project token, fall back to all_words.
        let project = tokens(&["myproj"]);
        let s = score_path_relevance("src/myproj/login.ts", "myproj login", &project);
        assert!(s > 0.0);
    }

    #[test]
    fn is_test_file_prefix_and_separator_and_camel() {
        assert!(is_test_file("test_foo.py"));
        assert!(is_test_file("foo_test.go"));
        assert!(is_test_file("foo.test.ts"));
        assert!(is_test_file("foo-spec.js"));
        assert!(is_test_file("FooTest.java"));
        assert!(is_test_file("FooSpec.scala"));
        assert!(is_test_file("MyTestCase.cs"));
    }

    #[test]
    fn is_test_file_directory_markers() {
        assert!(is_test_file("src/tests/mod.rs"));
        assert!(is_test_file("a/__tests__/b.ts"));
        assert!(is_test_file("spec/foo.rb"));
        assert!(is_test_file("app/UnitTests/Thing.cs"));
        assert!(is_test_file("examples/demo.rs"));
        assert!(is_test_file("fixtures/data.json"));
    }

    #[test]
    fn is_test_file_production_is_false() {
        assert!(!is_test_file("src/main.rs"));
        assert!(!is_test_file("lib/handler.ts"));
        // "attest" contains "test" but is neither prefix nor separator-suffixed.
        assert!(!is_test_file("src/attestation.rs"));
    }

    #[test]
    fn name_match_bonus_exact_and_multiword_and_prefix() {
        // exact (whitespace stripped) → 80
        assert_eq!(name_match_bonus("getUser", "get user"), 80.0);
        // multiword query, one token equals name → 60
        assert_eq!(name_match_bonus("login", "the login flow"), 60.0);
        // prefix match → 10 + 30*ratio
        let pref = name_match_bonus("authenticate", "auth");
        assert!(pref > 10.0 && pref < 40.0);
    }

    #[test]
    fn name_match_bonus_all_terms_contained_and_substring_and_none() {
        // multi-term all contained (not exact/prefix) → 15
        assert_eq!(name_match_bonus("userProfileCard", "profile card"), 15.0);
        // plain substring → 10
        assert_eq!(name_match_bonus("handleLoginRequest", "login"), 10.0);
        // no match → 0
        assert_eq!(name_match_bonus("foo", "zzz"), 0.0);
    }

    #[test]
    fn kind_bonus_covers_every_variant() {
        for kind in NodeKind::ALL {
            let b = kind_bonus(kind);
            assert!((0.0..=10.0).contains(&b), "{kind:?} bonus in range");
        }
        assert_eq!(kind_bonus(NodeKind::Function), 10.0);
        assert_eq!(kind_bonus(NodeKind::File), 0.0);
        assert_eq!(kind_bonus(NodeKind::Interface), 9.0);
    }

    #[test]
    fn is_distinctive_identifier_rules() {
        assert!(!is_distinctive_identifier(""));
        assert!(is_distinctive_identifier("snake_case"));
        assert!(is_distinctive_identifier("var2"));
        assert!(is_distinctive_identifier("camelCase"));
        // all-lowercase, no digit/underscore → not distinctive
        assert!(!is_distinctive_identifier("plain"));
        // leading uppercase only → not distinctive (skip(1) finds no upper)
        assert!(!is_distinctive_identifier("Plain"));
    }
}
