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
        if let Some(pos) = stem.rfind(marker) {
            if pos + marker.len() == stem.len() && pos > 0 {
                let sep = stem_chars[pos - 1];
                if sep == '.' || sep == '_' || sep == '-' {
                    return true;
                }
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
