//! A deterministic stand-in for JavaScript's `String.prototype.localeCompare`,
//! so lists ported from upstream sort the way upstream sorts them.

use std::cmp::Ordering;

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
pub fn locale_compare(a: &str, b: &str) -> Ordering {
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

/// One primary weight of [`locale_compare_numeric`]: a character, or a run of
/// ASCII digits read as one number, its leading zeros dropped.
enum Weight<'s> {
    Char(char),
    Number(&'s str),
}

impl Ord for Weight<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        let group = |weight: &Weight<'_>| match weight {
            Weight::Char(c) => primary_weight(*c),
            Weight::Number(_) => (3, 0),
        };
        match (self, other) {
            (Weight::Number(a), Weight::Number(b)) => (a.len(), *a).cmp(&(b.len(), *b)),
            _ => group(self).cmp(&group(other)),
        }
    }
}

impl PartialOrd for Weight<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Weight<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Weight<'_> {}

/// `text`'s weights: each character, each run of ASCII digits one number.
fn numeric_weights(text: &str) -> Vec<Weight<'_>> {
    let mut weights = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c.is_ascii_digit() {
            let run = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let digits = rest[..run].trim_start_matches('0');
            weights.push(Weight::Number(if digits.is_empty() {
                &rest[run - 1..run]
            } else {
                digits
            }));
            rest = &rest[run..];
        } else {
            weights.push(Weight::Char(c));
            rest = &rest[c.len_utf8()..];
        }
    }
    weights
}

/// `text` with each run of ASCII digits written without its leading zeros:
/// what the case and code-point levels read, since ICU's numeric collation
/// ignores those zeros at every level.
fn without_leading_zeros(text: &str) -> String {
    numeric_weights(text)
        .into_iter()
        .map(|weight| match weight {
            Weight::Char(c) => c.to_string(),
            Weight::Number(digits) => digits.to_string(),
        })
        .collect()
}

/// `a.localeCompare(b, 'en', { numeric: true })`: [`locale_compare`] with
/// each run of ASCII digits compared as the number it writes (`file:9`
/// before `file:10`), and `a01` equal to `a1`.
pub fn locale_compare_numeric(a: &str, b: &str) -> Ordering {
    numeric_weights(a).cmp(&numeric_weights(b)).then_with(|| {
        let (a, b) = (without_leading_zeros(a), without_leading_zeros(b));
        a.chars()
            .map(char::is_uppercase)
            .cmp(b.chars().map(char::is_uppercase))
            .then_with(|| a.cmp(&b))
    })
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

    #[test]
    fn locale_compare_numeric_follows_icu_numeric_order() {
        // Node 26.5.0 / ICU 78.3: a stable sort of `input` by
        // `a.localeCompare(b, 'en', { numeric: true })`.
        let input = [
            "discovery/aws/ec2.go:9",
            "discovery/aws/ec2.go:10",
            "discovery/aws/ec2.go:100",
            "discovery/aws/ec2.go:09",
            "discovery/aws/ec2_test.go:5",
            "discovery/aws/ec2.go:221",
            "a1",
            "a01",
            "a10",
            "a2",
            "A1",
            "a_1",
            "a-1",
            "a.1",
            "a 1",
            "a1b",
            "a1B",
            "a1.5",
            "a1.10",
            "file:9",
            "file:10",
            "x/y.go:2",
            "x/y.go:12",
            "x/yz.go:1",
            "x/y_test.go:3",
            "b",
            "B",
            "1",
            "01",
            "10",
            "9",
            "0",
            "00",
            "_x",
            "~x",
            "#x",
            "src/a.go:7",
            "src/A.go:7",
            "v1.2.10",
            "v1.2.9",
            "v1.10.0",
        ];
        let expected = [
            "_x",
            "#x",
            "~x",
            "0",
            "00",
            "1",
            "01",
            "9",
            "10",
            "a 1",
            "a_1",
            "a-1",
            "a.1",
            "a1",
            "a01",
            "A1",
            "a1.5",
            "a1.10",
            "a1b",
            "a1B",
            "a2",
            "a10",
            "b",
            "B",
            "discovery/aws/ec2_test.go:5",
            "discovery/aws/ec2.go:9",
            "discovery/aws/ec2.go:09",
            "discovery/aws/ec2.go:10",
            "discovery/aws/ec2.go:100",
            "discovery/aws/ec2.go:221",
            "file:9",
            "file:10",
            "src/a.go:7",
            "src/A.go:7",
            "v1.2.9",
            "v1.2.10",
            "v1.10.0",
            "x/y_test.go:3",
            "x/y.go:2",
            "x/y.go:12",
            "x/yz.go:1",
        ];
        let mut sorted = input.to_vec();
        sorted.sort_by(|a, b| locale_compare_numeric(a, b));
        assert_eq!(sorted, expected);
        for (a, b) in [
            ("a01", "a1"),
            ("0", "00"),
            ("x:09", "x:9"),
            ("a1b", "a01b"),
            ("1", "01"),
        ] {
            assert_eq!(locale_compare_numeric(a, b), Ordering::Equal, "{a} {b}");
        }
        assert_eq!(locale_compare_numeric("a1", "A1"), Ordering::Less);
    }
}
