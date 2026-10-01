use std::cell::RefCell;
use std::sync::OnceLock;

use codegraph_core::node_id::{NodeIdAllocator, utf16_column};
use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use regex::Regex;

use crate::embedded::shared::{
    contains_edge, default_node, empty_result, file_like_node, line_number_for_offset, line_starts,
    unresolved_ref,
};

/// The two mapper dialects (upstream `mybatis-extractor.ts`, #1182).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// MyBatis 3 `<mapper namespace="…">`.
    MyBatis,
    /// iBatis 2 `<sqlMap>`, whose namespace is optional and which adds the
    /// generic `<statement>` and `<procedure>` verbs.
    IBatis,
}

pub struct MyBatisExtractor<'a> {
    file_path: &'a str,
    /// The source as written: the file node and identity columns read it.
    original: &'a str,
    /// The source with every XML comment blanked byte for byte, so the scans
    /// never see a commented-out statement or include while every byte offset
    /// and line still maps to [`Self::original`].
    source: String,
    line_starts: Vec<usize>,
    node_ids: RefCell<NodeIdAllocator>,
}

impl<'a> MyBatisExtractor<'a> {
    pub fn new(file_path: &'a str, source: &'a str) -> Self {
        let stripped = strip_xml_comments(source);
        Self {
            file_path,
            original: source,
            line_starts: line_starts(&stripped),
            source: stripped,
            node_ids: RefCell::new(NodeIdAllocator::default()),
        }
    }

    pub fn extract(self) -> ExtractionResult {
        let start = std::time::Instant::now();
        let mut result = empty_result(0);
        let file_node = file_like_node(self.file_path, self.original, Language::Xml);
        let file_id = file_node.id.clone();
        result.nodes.push(file_node);

        if let Some((dialect, namespace, body_start, body_end)) = self.find_mapper_root() {
            self.extract_mapper(
                &mut result,
                &file_id,
                dialect,
                &namespace,
                body_start,
                body_end,
            );
        }
        result.duration_ms += start.elapsed().as_millis() as i64;
        result
    }

    /// Locates the mapper root element, its dialect and its namespace.
    ///
    /// A MyBatis `<mapper>` needs a namespace. Failing that, an iBatis 2
    /// `<sqlMap>` root is accepted with or without one: a namespace-less map
    /// qualifies each statement through its `Map.statement` id (upstream
    /// #1182). `\b` keeps `<sqlMapConfig>` — the iBatis *config* file, which
    /// holds no statements — from being mistaken for a statement map. The body
    /// window ends at the matching root close, or the end of the file.
    fn find_mapper_root(&self) -> Option<(Dialect, String, usize, usize)> {
        static MAPPER: OnceLock<Regex> = OnceLock::new();
        static SQL_MAP: OnceLock<Regex> = OnceLock::new();
        let body = |open: regex::Captures<'_>, close_tag: &str| {
            let body_start = open.get(0).map_or(0, |m| m.end());
            let body_end = self.source[body_start..]
                .find(close_tag)
                .map_or(self.source.len(), |idx| body_start + idx);
            (body_start, body_end)
        };

        let mapper = MAPPER.get_or_init(|| Regex::new(r"<mapper\b([^>]*)>").unwrap());
        if let Some(open) = mapper.captures(&self.source) {
            let attrs = open.get(1).map_or("", |m| m.as_str());
            if let Some(namespace) = attribute(attrs, Attribute::Namespace) {
                let namespace = namespace.to_string();
                let (body_start, body_end) = body(open, "</mapper>");
                return Some((Dialect::MyBatis, namespace, body_start, body_end));
            }
        }
        let sql_map = SQL_MAP.get_or_init(|| Regex::new(r"<sqlMap\b([^>]*)>").unwrap());
        // KEEP-RUST: a self-closing `<sqlMap resource="…"/>` is how a
        // `<sqlMapConfig>` lists its maps, not a map; it holds no statements,
        // so it is never the root (upstream would read the config's tail as one).
        let open = sql_map.captures_iter(&self.source).find(|open| {
            !open
                .get(1)
                .is_some_and(|attrs| attrs.as_str().trim_end().ends_with('/'))
        })?;
        let attrs = open.get(1).map_or("", |m| m.as_str());
        let namespace = attribute(attrs, Attribute::Namespace)
            .unwrap_or_default()
            .to_string();
        let (body_start, body_end) = body(open, "</sqlMap>");
        Some((Dialect::IBatis, namespace, body_start, body_end))
    }

    fn extract_mapper(
        &self,
        result: &mut ExtractionResult,
        file_id: &str,
        dialect: Dialect,
        namespace: &str,
        body_start: usize,
        body_end: usize,
    ) {
        static MYBATIS_OPEN: OnceLock<Regex> = OnceLock::new();
        static IBATIS_OPEN: OnceLock<Regex> = OnceLock::new();
        static CLOSE: OnceLock<Regex> = OnceLock::new();
        let open_re = match dialect {
            Dialect::MyBatis => MYBATIS_OPEN.get_or_init(|| {
                Regex::new(r"<(select|insert|update|delete|sql)\b([^>]*)>").unwrap()
            }),
            Dialect::IBatis => IBATIS_OPEN.get_or_init(|| {
                Regex::new(r"<(select|insert|update|delete|sql|statement|procedure)\b([^>]*)>")
                    .unwrap()
            }),
        };
        // A close tag may carry whitespace before its `>` (`</select >`, legal
        // XML); missing it let the statement run on to the next close and
        // swallow the statement in between (upstream #1222).
        let close_re = CLOSE.get_or_init(|| {
            Regex::new(r"</(select|insert|update|delete|sql|statement|procedure)\s*>").unwrap()
        });
        let body = &self.source[body_start..body_end];
        let mut search_start = 0;
        while let Some(captures) = open_re.captures(&body[search_start..]) {
            let open = captures.get(0).unwrap();
            let open_abs_body = search_start + open.start();
            let elem_type = captures.get(1).unwrap().as_str();
            let attrs = captures.get(2).map_or("", |m| m.as_str());
            let content_start_body = search_start + open.end();
            let Some(close) = close_re
                .captures_iter(&body[content_start_body..])
                .find(|close| close.get(1).is_some_and(|verb| verb.as_str() == elem_type))
                .and_then(|close| close.get(0))
            else {
                search_start += open.end();
                continue;
            };
            let close_abs_body = content_start_body + close.start();
            let full_end_body = content_start_body + close.end();
            search_start = full_end_body;

            let Some(id) = attribute(attrs, Attribute::Id) else {
                continue;
            };
            let absolute_index = body_start + open_abs_body;
            let start_line = line_number_for_offset(&self.line_starts, absolute_index);
            let end_line = line_number_for_offset(&self.line_starts, body_start + full_end_body);
            let (qualified, name) = qualify_statement(namespace, id);
            let elem_body = &body[content_start_body..close_abs_body];
            let mut node = default_node(
                self.file_path,
                Language::Xml,
                NodeKind::Method,
                name,
                qualified,
                start_line,
                end_line,
                0,
                0,
            );
            // A vendor-split pair (`databaseId`) written on one line shares a
            // name and a line; the allocator keeps the second (#1349).
            node.id = self.node_ids.borrow_mut().generate(
                self.file_path,
                node.kind,
                &node.name,
                start_line.max(1) as u32,
                utf16_column(self.original, absolute_index),
            );
            node.signature = Some(build_signature(elem_type, attrs));
            node.docstring = Some(preview_sql(elem_body));
            let node_id = node.id.clone();
            result.nodes.push(node);
            result.edges.push(contains_edge(file_id, &node_id));
            self.extract_includes(
                result,
                &node_id,
                namespace,
                elem_body,
                body_start + content_start_body,
            );
        }
    }

    fn extract_includes(
        &self,
        result: &mut ExtractionResult,
        node_id: &str,
        namespace: &str,
        elem_body: &str,
        elem_body_abs: usize,
    ) {
        static INCLUDE: OnceLock<Regex> = OnceLock::new();
        let include_re = INCLUDE.get_or_init(|| {
            Regex::new(r#"<include\b[^>]*\brefid\s*=\s*(?:"([^"']+)"|'([^"']+)')"#).unwrap()
        });
        for cap in include_re.captures_iter(elem_body) {
            let full = cap.get(0).unwrap();
            let Some(refid) = cap.get(1).or_else(|| cap.get(2)).map(|m| m.as_str()) else {
                continue;
            };
            let ref_qualified = qualify_refid(namespace, refid);
            let line = line_number_for_offset(&self.line_starts, elem_body_abs + full.start());
            result.unresolved_references.push(unresolved_ref(
                node_id,
                ref_qualified,
                EdgeKind::References,
                line,
                0,
                self.file_path,
                Language::Xml,
            ));
        }
    }
}

/// Blanks every `<!-- … -->` comment byte for byte (newlines kept), so a
/// commented-out statement or include is never matched while offsets and
/// lines are unchanged. `<![CDATA[ … ]]>` is skipped intact: a `<!--` inside
/// it is SQL data, not a comment. Comment delimiters are ASCII, so blanking a
/// whole comment keeps the text valid UTF-8.
fn strip_xml_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"<![CDATA[") {
            index = find_bytes(bytes, b"]]>", index + 9).map_or(bytes.len(), |end| end + 3);
            continue;
        }
        if bytes[index..].starts_with(b"<!--") {
            let stop = find_bytes(bytes, b"-->", index + 4).map_or(bytes.len(), |end| end + 3);
            for byte in &mut out[index..stop] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
            index = stop;
            continue;
        }
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| from + position)
}

#[derive(Debug, Clone, Copy)]
enum Attribute {
    Namespace,
    Id,
    ResultType,
    ParameterType,
    DatabaseId,
}

/// The value of attribute `which` in `attrs`, in either quote style. The
/// identifier-shaped values read here are Java names, statement ids or type
/// aliases and never contain a quote, so the value excludes both.
fn attribute(attrs: &str, which: Attribute) -> Option<&str> {
    static PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            "namespace",
            "id",
            "resultType",
            "parameterType",
            "databaseId",
        ]
        .map(|name| Regex::new(&format!(r#"\b{name}\s*=\s*(?:"([^"']+)"|'([^"']+)')"#)).unwrap())
    });
    let captures = patterns[which as usize].captures(attrs)?;
    captures
        .get(1)
        .or_else(|| captures.get(2))
        .map(|m| m.as_str())
}

/// The `{namespace}::{id}` qualified name the MyBatis synthesizer
/// suffix-matches against a Java `{Class}::{method}`, and the display name. A
/// namespace-less iBatis map carries the qualifier in the id itself
/// (`Account.getById` → `Account::getById`, named `getById`).
fn qualify_statement(namespace: &str, id: &str) -> (String, String) {
    if !namespace.is_empty() {
        return (format!("{namespace}::{id}"), id.to_string());
    }
    match id.rsplit_once('.') {
        Some((owner, name)) => (format!("{owner}::{name}"), name.to_string()),
        None => (id.to_string(), id.to_string()),
    }
}

/// Builds the `{namespace}::{id}` reference name a `<sql>` fragment node
/// carries, from an `<include refid="…">` value (upstream #1209).
///
/// A bare `refid` is local to the enclosing mapper. A QUALIFIED `refid` names
/// the owning namespace, and only its LAST dotted segment is the fragment id —
/// the rest is the namespace, whose own dots are preserved, because that is
/// exactly the `qualified_name` the fragment node stores
/// (`com.example.UserMapper` + `::` + `baseColumns`). Rewriting every dot to
/// `::` would produce a name no node carries, and dropping the namespace would
/// let a same-`id` fragment in another namespace answer the include. In a
/// namespace-less iBatis map a bare `refid` names its fragment as is.
fn qualify_refid(namespace: &str, refid: &str) -> String {
    match refid.rsplit_once('.') {
        Some((owner, fragment)) if !owner.is_empty() && !fragment.is_empty() => {
            format!("{owner}::{fragment}")
        }
        _ if namespace.is_empty() => refid.to_string(),
        _ => format!("{namespace}::{refid}"),
    }
}

fn build_signature(elem_type: &str, attrs: &str) -> String {
    if elem_type == "sql" {
        return "<sql>".to_string();
    }
    let mut parts = vec![elem_type.to_ascii_uppercase()];
    if let Some(param) = attribute(attrs, Attribute::ParameterType) {
        parts.push(format!("param={param}"));
    }
    if let Some(result) = attribute(attrs, Attribute::ResultType) {
        parts.push(format!("result={result}"));
    }
    // A vendor-split statement carries `databaseId`; surface it so the two
    // otherwise identical `{namespace}::{id}` nodes are distinguishable.
    if let Some(database) = attribute(attrs, Attribute::DatabaseId) {
        parts.push(format!("databaseId={database}"));
    }
    parts.join(" ")
}

fn preview_sql(body: &str) -> String {
    Regex::new(r"<[^>]+>")
        .unwrap()
        .replace_all(body, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}
