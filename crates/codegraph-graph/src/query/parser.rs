use codegraph_core::types::{Language, NodeKind};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub text: String,
    pub kinds: Vec<NodeKind>,
    pub languages: Vec<Language>,
    pub path_filters: Vec<String>,
    pub name_filters: Vec<String>,
}

fn node_kind_from_str(value: &str) -> Option<NodeKind> {
    NodeKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == value)
}

fn language_from_str(value: &str) -> Option<Language> {
    Language::ALL
        .into_iter()
        .find(|language| language.as_str() == value)
}

fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn tokenize(raw: &str) -> Vec<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            if chars[i] == '"' {
                match chars[i + 1..].iter().position(|&c| c == '"') {
                    Some(rel) => {
                        i = i + 1 + rel + 1;
                        continue;
                    }
                    None => {
                        i = chars.len();
                        break;
                    }
                }
            }
            i += 1;
        }
        tokens.push(chars[start..i].iter().collect());
    }
    tokens
}

pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut out = ParsedQuery::default();
    let tokens = tokenize(raw);
    let mut text_parts: Vec<String> = Vec::new();

    for tok in &tokens {
        let colon = tok.find(':');
        let colon = match colon {
            Some(idx) if idx > 0 && idx != tok.len() - 1 => idx,
            _ => {
                text_parts.push(tok.clone());
                continue;
            }
        };
        let key = tok[..colon].to_lowercase();
        let value_raw = unquote(&tok[colon + 1..]).to_string();
        if value_raw.is_empty() {
            text_parts.push(tok.clone());
            continue;
        }
        match key.as_str() {
            "kind" => match node_kind_from_str(&value_raw) {
                Some(kind) => out.kinds.push(kind),
                None => text_parts.push(tok.clone()),
            },
            "lang" | "language" => {
                let lower = value_raw.to_lowercase();
                match language_from_str(&lower) {
                    Some(language) => out.languages.push(language),
                    None => text_parts.push(tok.clone()),
                }
            }
            "path" => out.path_filters.push(value_raw),
            "name" => out.name_filters.push(value_raw),
            _ => text_parts.push(tok.clone()),
        }
    }

    out.text = text_parts.join(" ").trim().to_string();
    out
}

pub fn bounded_edit_distance(a: &str, b: &str, max_dist: usize) -> usize {
    if a == b {
        return 0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let al = a.len();
    let bl = b.len();
    if al.abs_diff(bl) > max_dist {
        return max_dist + 1;
    }
    if al == 0 {
        return bl;
    }
    if bl == 0 {
        return al;
    }

    let mut prev: Vec<usize> = (0..=bl).collect();
    let mut cur: Vec<usize> = vec![0; bl + 1];

    for i in 1..=al {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=bl {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let insertion = cur[j - 1] + 1;
            let deletion = prev[j] + 1;
            let substitution = prev[j - 1] + cost;
            cur[j] = insertion.min(deletion).min(substitution);
            if cur[j] < row_min {
                row_min = cur[j];
            }
        }
        if row_min > max_dist {
            return max_dist + 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[bl]
}
