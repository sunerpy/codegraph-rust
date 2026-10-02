//! A deterministic stand-in for JavaScript's `String.prototype.localeCompare`,
//! so lists ported from upstream sort the way upstream's viewer sorts them.

/// CLDR root order for ASCII punctuation and symbols. Whitespace sorts before
/// all of them; digits, then letters, after.
const PUNCTUATION_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary_weight(c: char) -> (u8, u32) {
    if c.is_whitespace() {
        (0, c as u32)
    } else if let Some(at) = PUNCTUATION_ORDER.find(c) {
        (1, at as u32)
    } else if c.is_ascii_digit() {
        (3, c as u32)
    } else if c.is_alphanumeric() {
        (4, c.to_lowercase().next().unwrap_or(c) as u32)
    } else {
        (2, c as u32)
    }
}

/// `String.prototype.localeCompare` (ICU root collation) for the paths and
/// names the viewer sorts: whitespace, punctuation, digits, then letters with
/// case ignored; then lower case before upper case; then code points. Accented
/// letters are not folded onto their base letter — they sort after `z`.
pub fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars()
        .map(primary_weight)
        .cmp(b.chars().map(primary_weight))
        .then_with(|| {
            a.chars()
                .map(char::is_uppercase)
                .cmp(b.chars().map(char::is_uppercase))
        })
        .then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_compare_follows_icu_root_order() {
        // `node -e` with String.prototype.localeCompare, Node 26 / ICU.
        let expected = [
            "_x",
            "-x",
            "#x",
            "~x",
            "1",
            "a",
            "A",
            "a b",
            "a_b",
            "a-b",
            "a.b",
            "a/b",
            "a1",
            "a10",
            "a2",
            "ab",
            "b",
            "B",
            "f",
            "src/_a",
            "src/(root files)",
            "src/a",
            "src/A",
            "z",
            "Z",
        ];
        let mut shuffled = expected.to_vec();
        shuffled.reverse();
        shuffled.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(shuffled, expected);
    }
}
