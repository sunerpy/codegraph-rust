//! The members of a Vue Options API component (upstream #2140
//! `vue-options-api.ts`): `export default { methods: { login() {…} },
//! computed: {…}, watch: {…}, mounted() {…} }`.
//!
//! A `<script>` block goes through the TypeScript/JavaScript extractor, which
//! makes no node for a function written in the default export's object
//! literal. Every Vue 2 app, and every Vue 3 app not on `<script setup>`,
//! keeps its code there: vue-element-admin's login screen had no
//! `handleLogin`, so its calls and its `this.$router.push` belonged to the
//! file.
//!
//! This reads the options object the default export is, directly or through
//! `defineComponent({…})`, `defineNuxtComponent({…})`, `Vue.extend({…})` or
//! `Vue.component('x', {…})`, and names each function in it: the `methods`,
//! `computed` and `watch` entries, and the component's own functions
//! (`data`, `setup`, `render`, the lifecycle hooks, Nuxt 2's `asyncData` /
//! `fetch` / `head` / `validate` / `middleware`). It reads the text, as
//! upstream does, skipping strings, template literals and comments; offsets
//! are bytes of the script.

use std::sync::OnceLock;

use regex::Regex;

/// One function-valued member of the options object, as byte offsets into
/// the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptionsMember {
    pub name: String,
    /// Where the member's key starts.
    pub start: usize,
    /// One past where its value ends.
    pub end: usize,
}

/// The options a component declares as functions of its own.
const COMPONENT_FUNCTIONS: [&str; 24] = [
    "data",
    "setup",
    "render",
    "beforeCreate",
    "created",
    "beforeMount",
    "mounted",
    "beforeUpdate",
    "updated",
    "activated",
    "deactivated",
    "beforeDestroy",
    "destroyed",
    "beforeUnmount",
    "unmounted",
    "errorCaptured",
    "renderTracked",
    "renderTriggered",
    "serverPrefetch",
    // Nuxt 2
    "asyncData",
    "fetch",
    "head",
    "validate",
    "middleware",
];

/// The groups whose every entry is a function (or, for `computed` and
/// `watch`, an object of them).
const MEMBER_GROUPS: [&str; 3] = ["methods", "computed", "watch"];

fn byte(s: &str, at: usize) -> Option<u8> {
    s.as_bytes().get(at).copied()
}

/// The index after a string or template literal starting at `at`; `None` if
/// unterminated.
fn skip_quoted(s: &str, at: usize) -> Option<usize> {
    let quote = byte(s, at)?;
    let mut i = at + 1;
    while i < s.len() {
        let ch = s.as_bytes()[i];
        if ch == b'\\' {
            i += 2;
            continue;
        }
        if quote == b'`' && ch == b'$' && byte(s, i + 1) == Some(b'{') {
            i = match_close(s, i + 1)? + 1;
            continue;
        }
        if ch == quote {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// The index of the bracket closing the one at `open`; `None` if unbalanced.
fn match_close(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < s.len() {
        let ch = s.as_bytes()[i];
        match ch {
            b'"' | b'\'' | b'`' => {
                i = skip_quoted(s, i)?;
                continue;
            }
            b'/' if byte(s, i + 1) == Some(b'/') => {
                i = s[i..].find('\n').map_or(s.len(), |nl| i + nl) + 1;
                continue;
            }
            b'/' if byte(s, i + 1) == Some(b'*') => {
                i = s[i + 2..]
                    .find("*/")
                    .map_or(s.len(), |close| i + 2 + close + 2);
                continue;
            }
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

struct RawMember {
    name: String,
    start: usize,
    /// Where the value starts (after `:`), or the `(` of a method shorthand.
    value_at: usize,
    end: usize,
    shorthand: bool,
}

/// The separators, whitespace and comments at the start of `text`, as a
/// length (upstream `/^(?:\s|,|\/\/[^\n]*|\/\*[\s\S]*?\*\/)+/`).
fn separators(text: &str) -> usize {
    let mut at = 0;
    loop {
        let rest = &text[at..];
        if let Some(c) = rest.chars().next()
            && (c == ',' || codegraph_core::generated_header::is_js_whitespace(c))
        {
            at += c.len_utf8();
        } else if let Some(line) = rest.strip_prefix("//") {
            at += 2 + line.find('\n').unwrap_or(line.len());
        } else if let Some(block) = rest.strip_prefix("/*") {
            match block.find("*/") {
                Some(close) => at += 2 + close + 2,
                None => return at,
            }
        } else {
            return at;
        }
    }
}

/// A member's head (upstream `/^(?:(?:async|get|set)\s+(?=[\w$'"[]))?\*?\s*
/// (?:([A-Za-z_$][\w$]*)|(['"])((?:(?!\2).)*)\2|(\[)|(\.\.\.))/`): its length,
/// and its static name, `None` for a computed key or a spread.
enum Head {
    Named(usize, String),
    Computed(usize),
    Spread(usize),
}

fn member_head(text: &str) -> Option<Head> {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let mut at = 0;
    // `async`, `get`, `set` before a name.
    for keyword in ["async", "get", "set"] {
        if let Some(rest) = text.strip_prefix(keyword) {
            let spaced =
                rest.trim_start_matches(codegraph_core::generated_header::is_js_whitespace);
            if spaced.len() < rest.len()
                && spaced
                    .chars()
                    .next()
                    .is_some_and(|c| is_word(c) || matches!(c, '\'' | '"' | '['))
            {
                at = text.len() - spaced.len();
            }
            break;
        }
    }
    let rest = &text[at..];
    let rest = rest.strip_prefix('*').unwrap_or(rest);
    let rest = rest.trim_start_matches(codegraph_core::generated_header::is_js_whitespace);
    let at = text.len() - rest.len();
    let first = rest.chars().next()?;
    if first.is_ascii_alphabetic() || first == '_' || first == '$' {
        let len = rest.find(|c: char| !is_word(c)).unwrap_or(rest.len());
        return Some(Head::Named(at + len, rest[..len].to_string()));
    }
    if first == '\'' || first == '"' {
        let inner = &rest[1..];
        let close = inner.find([first, '\n', '\r', '\u{2028}', '\u{2029}'])?;
        if !inner[close..].starts_with(first) {
            return None;
        }
        return Some(Head::Named(at + 1 + close + 1, inner[..close].to_string()));
    }
    if first == '[' {
        return Some(Head::Computed(at + 1));
    }
    rest.starts_with("...").then_some(Head::Spread(at + 3))
}

/// The members of the object literal whose `{` is at `open`.
fn object_members(s: &str, open: usize) -> Vec<RawMember> {
    static COLON: OnceLock<Regex> = OnceLock::new();
    let colon = COLON.get_or_init(|| Regex::new(r"^\s*:").expect("member colon"));
    let Some(close) = match_close(s, open) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i = open + 1;
    while i < close {
        let skipped = separators(&s[i..close]);
        if skipped > 0 {
            i += skipped;
            continue;
        }
        let start = i;
        let Some(head) = member_head(&s[i..close]) else {
            i += s[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let (j, name) = match head {
            Head::Named(len, name) => (i + len, Some(name)),
            Head::Computed(len) => {
                // A computed key, `[KEY]() {}`, has no static name: step over it.
                let Some(key_close) = match_close(s, i + len - 1) else {
                    return out;
                };
                (key_close + 1, None)
            }
            Head::Spread(len) => (i + len, None),
        };
        // Where this member ends: the next depth-0 comma, or the object's end.
        let mut k = j;
        while k < close {
            let ch = s.as_bytes()[k];
            match ch {
                b'"' | b'\'' | b'`' => {
                    let Some(end) = skip_quoted(s, k) else {
                        return out;
                    };
                    k = end;
                    continue;
                }
                b'/' if matches!(byte(s, k + 1), Some(b'/' | b'*')) => {
                    let found = if byte(s, k + 1) == Some(b'/') {
                        s[k..].find('\n').map(|nl| k + nl)
                    } else {
                        s[k + 2..].find("*/").map(|at| k + 2 + at + 1)
                    };
                    k = found.map_or(close, |end| end + 1);
                    continue;
                }
                b'{' | b'[' | b'(' => {
                    let Some(end) = match_close(s, k) else {
                        return out;
                    };
                    k = end + 1;
                    continue;
                }
                b',' => break,
                _ => k += 1,
            }
        }
        let rest = &s[j..k];
        if let Some(name) = name {
            let colon_len = colon.find(rest).map(|found| found.end());
            out.push(RawMember {
                name,
                start,
                value_at: colon_len.map_or(j, |len| j + len),
                end: k,
                shorthand: colon_len.is_none() && rest.trim_start().starts_with('('),
            });
        }
        i = k + 1;
    }
    out
}

/// Whether a member's value is a function: a shorthand method, `function
/// (…) {…}`, or an arrow.
fn is_function_value(s: &str, member: &RawMember) -> bool {
    static FUNCTION: OnceLock<Regex> = OnceLock::new();
    static ARROW: OnceLock<Regex> = OnceLock::new();
    if member.shorthand {
        return true;
    }
    let value = s[member.value_at..member.end].trim_start();
    FUNCTION
        .get_or_init(|| Regex::new(r"^(?:async\s+)?function(?-u:\b)").expect("function value"))
        .is_match(value)
        || ARROW
            .get_or_init(|| {
                Regex::new(r"^(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][0-9A-Za-z_$]*)\s*=>")
                    .expect("arrow value")
            })
            .is_match(value)
}

/// The `{` of the options object the default export is.
fn options_object_start(script: &str) -> Option<usize> {
    static EXPORT: OnceLock<Regex> = OnceLock::new();
    EXPORT
        .get_or_init(|| {
            Regex::new(
                r#"(?-u:\b)export\s+default\s+(?:(?:defineComponent|defineNuxtComponent|Vue\s*\.\s*extend)\s*\(\s*|Vue\s*\.\s*component\s*\(\s*(?:'[^'"]*'|"[^'"]*")\s*,\s*)?\{"#,
            )
            .expect("options export")
        })
        .find(script)
        .map(|found| found.end() - 1)
}

/// Every function a Vue Options API component declares, in source order.
pub(crate) fn vue_options_members(script: &str) -> Vec<OptionsMember> {
    let Some(open) = options_object_start(script) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for member in object_members(script, open) {
        if COMPONENT_FUNCTIONS.contains(&member.name.as_str()) && is_function_value(script, &member)
        {
            out.push(OptionsMember {
                name: member.name,
                start: member.start,
                end: member.end,
            });
            continue;
        }
        if !MEMBER_GROUPS.contains(&member.name.as_str()) || member.shorthand {
            continue;
        }
        let Some(value_open) = script[member.value_at..]
            .find('{')
            .map(|at| member.value_at + at)
        else {
            continue;
        };
        if value_open >= member.end || !script[member.value_at..value_open].trim().is_empty() {
            continue;
        }
        for entry in object_members(script, value_open) {
            // `computed: { x: { get() {}, set() {} } }` and `watch: { y: {
            // handler() {} } }` are functions too; a watcher written as a
            // method NAME (`z: 'onZ'`) is not.
            let value = script[entry.value_at..entry.end].trim_start();
            if is_function_value(script, &entry)
                || (member.name != "methods" && value.starts_with('{'))
            {
                out.push(OptionsMember {
                    name: entry.name,
                    start: entry.start,
                    end: entry.end,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::vue_options_members;

    fn names(script: &str) -> Vec<String> {
        vue_options_members(script)
            .into_iter()
            .map(|member| member.name)
            .collect()
    }

    #[test]
    fn names_every_function_an_options_object_declares() {
        let script = "
import { mapActions } from 'vuex'
export default {
  name: 'Login',
  props: { redirect: String },
  data() {
    return { loading: false }
  },
  computed: {
    title() { return 'Sign in' },
    fullName: {
      get() { return this.first },
      set(v) { this.first = v }
    },
    ...mapGetters(['user'])
  },
  watch: {
    $route: { handler(route) { this.redirect = route.query.redirect }, immediate: true },
    'form.email'(value) { this.check(value) },
    loading: 'onLoading'
  },
  created() { this.init() },
  mounted: function () { this.focus() },
  methods: {
    ...mapActions(['login']),
    async handleLogin() { await this.login() },
    validate: (value) => value.length > 0,
    reset: function () {},
    [DYNAMIC]() {}
  }
}
";
        assert_eq!(
            names(script),
            [
                "data",
                "title",
                "fullName",
                "$route",
                "form.email",
                "created",
                "mounted",
                "handleLogin",
                "validate",
                "reset"
            ]
        );
        // Offsets are the key's start and the value's end.
        let members = vue_options_members(script);
        let created = members
            .iter()
            .find(|m| m.name == "created")
            .expect("created");
        assert_eq!(
            &script[created.start..created.end],
            "created() { this.init() }"
        );
    }

    #[test]
    fn reads_define_component_vue_extend_and_vue_component_only() {
        assert_eq!(
            names("export default defineComponent({ setup() { return {} } })"),
            ["setup"]
        );
        assert_eq!(
            names("export default Vue.extend({ methods: { go() {} } })"),
            ["go"]
        );
        assert_eq!(
            names("export default Vue.component('x-btn', { methods: { tap() {} } })"),
            ["tap"]
        );
        assert!(names("const x = { methods: { go() {} } }\nexport default x").is_empty());
        // Strings, template literals and comments hold no members.
        assert_eq!(
            names(
                "export default { methods: { /* a() {} */ b() { return `}${'{'}` }, // c() {}\n d() {} } }"
            ),
            ["b", "d"]
        );
    }
}
