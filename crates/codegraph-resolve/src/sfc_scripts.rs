//! What a Svelte or Vue single-file component's own scripts declare (upstream
//! #2227 `isSfcPrivate`).
//!
//! A Svelte component's instance `<script>` and a Vue SFC's `<script setup>`
//! are private to the component: no other file reaches what they declare by
//! name. A Svelte `<script module>` (`context="module"`), a Vue `<script>`
//! that is not `setup`, and a type a `<script setup>` exports (Vue hoists
//! those) stay reachable.

use std::sync::OnceLock;

use codegraph_core::types::{Node, NodeKind};
use regex::Regex;

use crate::source_facts::SourceFacts;
use crate::types::ResolutionContext;

/// One `<script>` block: its first and last lines (1-based) and whether what
/// it declares can be exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptBlock {
    pub start: i64,
    pub end: i64,
    pub exported: bool,
}

/// Whether `node`, declared in a `.svelte` or `.vue` file, is the component's
/// own: anything but the component (or file) itself, unless it sits in a
/// block that can export, or is a type a Vue `<script setup>` exports.
pub(crate) fn is_sfc_private(node: &Node, context: &dyn ResolutionContext) -> bool {
    let svelte = node.file_path.ends_with(".svelte");
    if (!svelte && !node.file_path.ends_with(".vue"))
        || matches!(node.kind, NodeKind::Component | NodeKind::File)
    {
        return false;
    }
    let Some(facts) = context.source_facts(&node.file_path) else {
        return false;
    };
    let blocks = facts.sfc_script_blocks(|facts| read_script_blocks(facts, svelte));
    let block = blocks
        .iter()
        .find(|block| block.start <= node.start_line && node.start_line <= block.end);
    if block.is_some_and(|block| block.exported) {
        return false;
    }
    // Vue hoists the types `<script setup>` exports: mealie imports
    // CrudTable.vue's `TableConfig`.
    if !svelte && block.is_some() {
        static EXPORTED_TYPE: OnceLock<Regex> = OnceLock::new();
        let line = usize::try_from(node.start_line)
            .ok()
            .and_then(|line| line.checked_sub(1))
            .and_then(|index| facts.raw_line(index))
            .unwrap_or_default();
        if EXPORTED_TYPE
            .get_or_init(|| {
                Regex::new(r"^\s*export\s+(?:declare\s+)?(?:interface|type|enum)(?-u:\b)")
                    .expect("exported type")
            })
            .is_match(line)
        {
            return false;
        }
    }
    true
}

/// The `<script>` blocks of a component's source, in order: a block opens at
/// a line with `<script …>` (none open) and closes at the first line after,
/// or on, it with `</script>`.
pub fn read_script_blocks(facts: &SourceFacts, svelte: bool) -> Vec<ScriptBlock> {
    static OPEN: OnceLock<Regex> = OnceLock::new();
    static CLOSE: OnceLock<Regex> = OnceLock::new();
    static MODULE: OnceLock<Regex> = OnceLock::new();
    static SETUP: OnceLock<Regex> = OnceLock::new();
    let open_tag = OPEN.get_or_init(|| Regex::new(r"(?i)<script(?-u:\b)([^>]*)>").expect("open"));
    let close_tag = CLOSE.get_or_init(|| Regex::new(r"(?i)</script\s*>").expect("close"));
    let module = MODULE.get_or_init(|| {
        Regex::new(r#"(?-u:\b)module(?-u:\b)|context\s*=\s*["']module["']"#).expect("module")
    });
    let setup = SETUP.get_or_init(|| Regex::new(r"(?-u:\b)setup(?-u:\b)").expect("setup"));
    let mut blocks = Vec::new();
    let mut open: Option<(i64, bool)> = None;
    for index in 0..facts.line_count() {
        let Some(text) = facts.raw_line(index) else {
            continue;
        };
        let line = i64::try_from(index).unwrap_or(i64::MAX).saturating_add(1);
        if open.is_none()
            && let Some(tag) = open_tag.captures(text)
        {
            let attrs = tag.get(1).map_or("", |attrs| attrs.as_str());
            let exported = if svelte {
                module.is_match(attrs)
            } else {
                !setup.is_match(attrs)
            };
            open = Some((line, exported));
        }
        if let Some((start, exported)) = open
            && close_tag.is_match(text)
        {
            blocks.push(ScriptBlock {
                start,
                end: line,
                exported,
            });
            open = None;
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{ScriptBlock, read_script_blocks};
    use crate::source_facts::SourceFacts;

    fn blocks(source: &str, svelte: bool) -> Vec<ScriptBlock> {
        read_script_blocks(&SourceFacts::new(Arc::from(source)), svelte)
    }

    #[test]
    fn svelte_module_scripts_export_and_instance_scripts_do_not() {
        let source = "<script context=\"module\">\n  export const shared = 1;\n</script>\n<script lang=\"ts\">\n  let local = 2;\n</script>\n<SCRIPT module>x</SCRIPT>\n";
        assert_eq!(
            blocks(source, true),
            [
                ScriptBlock {
                    start: 1,
                    end: 3,
                    exported: true
                },
                ScriptBlock {
                    start: 4,
                    end: 6,
                    exported: false
                },
                ScriptBlock {
                    start: 7,
                    end: 7,
                    exported: true
                },
            ]
        );
    }

    #[test]
    fn vue_plain_scripts_export_and_setup_scripts_do_not() {
        let source = "<template><p /></template>\n<script lang=\"ts\">\nexport default {};\n</script>\n<script setup lang=\"ts\">\nconst x = 1;\n</script>\n";
        assert_eq!(
            blocks(source, false),
            [
                ScriptBlock {
                    start: 2,
                    end: 4,
                    exported: true
                },
                ScriptBlock {
                    start: 5,
                    end: 7,
                    exported: false
                },
            ]
        );
    }
}
