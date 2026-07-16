use serde::{Deserialize, Serialize};
use std::fmt;

// Exact strings from upstream types.ts:
// NodeKind lines 18-41: file, module, class, struct, interface, trait,
// protocol, function, method, property, field, variable, constant, enum,
// enum_member, type_alias, namespace, parameter, import, export, route,
// component.
// EdgeKind lines 48-60: contains, calls, imports, exports, extends,
// implements, references, type_of, returns, instantiates, overrides,
// decorates.
pub const NODE_KIND_STRINGS: [&str; 22] = [
    "file",
    "module",
    "class",
    "struct",
    "interface",
    "trait",
    "protocol",
    "function",
    "method",
    "property",
    "field",
    "variable",
    "constant",
    "enum",
    "enum_member",
    "type_alias",
    "namespace",
    "parameter",
    "import",
    "export",
    "route",
    "component",
];

pub const EDGE_KIND_STRINGS: [&str; 12] = [
    "contains",
    "calls",
    "imports",
    "exports",
    "extends",
    "implements",
    "references",
    "type_of",
    "returns",
    "instantiates",
    "overrides",
    "decorates",
];

pub const LANGUAGE_STRINGS: [&str; 41] = [
    "typescript",
    "javascript",
    "tsx",
    "jsx",
    "arkts",
    "python",
    "go",
    "rust",
    "java",
    "c",
    "cpp",
    "csharp",
    "razor",
    "php",
    "ruby",
    "swift",
    "kotlin",
    "dart",
    "svelte",
    "vue",
    "astro",
    "liquid",
    "pascal",
    "scala",
    "lua",
    "luau",
    "objc",
    "r",
    "solidity",
    "nix",
    "terraform",
    "erlang",
    "yaml",
    "twig",
    "xml",
    "properties",
    "gdscript",
    "godot_scene",
    "godot_resource",
    "godot_project",
    "unknown",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "module")]
    Module,
    #[serde(rename = "class")]
    Class,
    #[serde(rename = "struct")]
    Struct,
    #[serde(rename = "interface")]
    Interface,
    #[serde(rename = "trait")]
    Trait,
    #[serde(rename = "protocol")]
    Protocol,
    #[serde(rename = "function")]
    Function,
    #[serde(rename = "method")]
    Method,
    #[serde(rename = "property")]
    Property,
    #[serde(rename = "field")]
    Field,
    #[serde(rename = "variable")]
    Variable,
    #[serde(rename = "constant")]
    Constant,
    #[serde(rename = "enum")]
    Enum,
    #[serde(rename = "enum_member")]
    EnumMember,
    #[serde(rename = "type_alias")]
    TypeAlias,
    #[serde(rename = "namespace")]
    Namespace,
    #[serde(rename = "parameter")]
    Parameter,
    #[serde(rename = "import")]
    Import,
    #[serde(rename = "export")]
    Export,
    #[serde(rename = "route")]
    Route,
    #[serde(rename = "component")]
    Component,
}

impl NodeKind {
    pub const ALL: [Self; 22] = [
        Self::File,
        Self::Module,
        Self::Class,
        Self::Struct,
        Self::Interface,
        Self::Trait,
        Self::Protocol,
        Self::Function,
        Self::Method,
        Self::Property,
        Self::Field,
        Self::Variable,
        Self::Constant,
        Self::Enum,
        Self::EnumMember,
        Self::TypeAlias,
        Self::Namespace,
        Self::Parameter,
        Self::Import,
        Self::Export,
        Self::Route,
        Self::Component,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Protocol => "protocol",
            Self::Function => "function",
            Self::Method => "method",
            Self::Property => "property",
            Self::Field => "field",
            Self::Variable => "variable",
            Self::Constant => "constant",
            Self::Enum => "enum",
            Self::EnumMember => "enum_member",
            Self::TypeAlias => "type_alias",
            Self::Namespace => "namespace",
            Self::Parameter => "parameter",
            Self::Import => "import",
            Self::Export => "export",
            Self::Route => "route",
            Self::Component => "component",
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    #[serde(rename = "contains")]
    Contains,
    #[serde(rename = "calls")]
    Calls,
    #[serde(rename = "imports")]
    Imports,
    #[serde(rename = "exports")]
    Exports,
    #[serde(rename = "extends")]
    Extends,
    #[serde(rename = "implements")]
    Implements,
    #[serde(rename = "references")]
    References,
    #[serde(rename = "type_of")]
    TypeOf,
    #[serde(rename = "returns")]
    Returns,
    #[serde(rename = "instantiates")]
    Instantiates,
    #[serde(rename = "overrides")]
    Overrides,
    #[serde(rename = "decorates")]
    Decorates,
}

impl EdgeKind {
    pub const ALL: [Self; 12] = [
        Self::Contains,
        Self::Calls,
        Self::Imports,
        Self::Exports,
        Self::Extends,
        Self::Implements,
        Self::References,
        Self::TypeOf,
        Self::Returns,
        Self::Instantiates,
        Self::Overrides,
        Self::Decorates,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Exports => "exports",
            Self::Extends => "extends",
            Self::Implements => "implements",
            Self::References => "references",
            Self::TypeOf => "type_of",
            Self::Returns => "returns",
            Self::Instantiates => "instantiates",
            Self::Overrides => "overrides",
            Self::Decorates => "decorates",
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "typescript")]
    TypeScript,
    #[serde(rename = "javascript")]
    JavaScript,
    #[serde(rename = "tsx")]
    Tsx,
    #[serde(rename = "jsx")]
    Jsx,
    #[serde(rename = "arkts")]
    ArkTs,
    #[serde(rename = "python")]
    Python,
    #[serde(rename = "go")]
    Go,
    #[serde(rename = "rust")]
    Rust,
    #[serde(rename = "java")]
    Java,
    #[serde(rename = "c")]
    C,
    #[serde(rename = "cpp")]
    Cpp,
    #[serde(rename = "csharp")]
    CSharp,
    #[serde(rename = "razor")]
    Razor,
    #[serde(rename = "php")]
    Php,
    #[serde(rename = "ruby")]
    Ruby,
    #[serde(rename = "swift")]
    Swift,
    #[serde(rename = "kotlin")]
    Kotlin,
    #[serde(rename = "dart")]
    Dart,
    #[serde(rename = "svelte")]
    Svelte,
    #[serde(rename = "vue")]
    Vue,
    #[serde(rename = "astro")]
    Astro,
    #[serde(rename = "liquid")]
    Liquid,
    #[serde(rename = "pascal")]
    Pascal,
    #[serde(rename = "scala")]
    Scala,
    #[serde(rename = "lua")]
    Lua,
    #[serde(rename = "luau")]
    Luau,
    #[serde(rename = "objc")]
    ObjC,
    #[serde(rename = "r")]
    R,
    #[serde(rename = "solidity")]
    Solidity,
    #[serde(rename = "nix")]
    Nix,
    #[serde(rename = "terraform")]
    Terraform,
    #[serde(rename = "erlang")]
    Erlang,
    #[serde(rename = "yaml")]
    Yaml,
    #[serde(rename = "twig")]
    Twig,
    #[serde(rename = "xml")]
    Xml,
    #[serde(rename = "properties")]
    Properties,
    #[serde(rename = "gdscript")]
    Gdscript,
    #[serde(rename = "godot_scene")]
    GodotScene,
    #[serde(rename = "godot_resource")]
    GodotResource,
    #[serde(rename = "godot_project")]
    GodotProject,
    #[serde(rename = "unknown")]
    Unknown,
}

impl Language {
    pub const ALL: [Self; 41] = [
        Self::TypeScript,
        Self::JavaScript,
        Self::Tsx,
        Self::Jsx,
        Self::ArkTs,
        Self::Python,
        Self::Go,
        Self::Rust,
        Self::Java,
        Self::C,
        Self::Cpp,
        Self::CSharp,
        Self::Razor,
        Self::Php,
        Self::Ruby,
        Self::Swift,
        Self::Kotlin,
        Self::Dart,
        Self::Svelte,
        Self::Vue,
        Self::Astro,
        Self::Liquid,
        Self::Pascal,
        Self::Scala,
        Self::Lua,
        Self::Luau,
        Self::ObjC,
        Self::R,
        Self::Solidity,
        Self::Nix,
        Self::Terraform,
        Self::Erlang,
        Self::Yaml,
        Self::Twig,
        Self::Xml,
        Self::Properties,
        Self::Gdscript,
        Self::GodotScene,
        Self::GodotResource,
        Self::GodotProject,
        Self::Unknown,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Tsx => "tsx",
            Self::Jsx => "jsx",
            Self::ArkTs => "arkts",
            Self::Python => "python",
            Self::Go => "go",
            Self::Rust => "rust",
            Self::Java => "java",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Razor => "razor",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Swift => "swift",
            Self::Kotlin => "kotlin",
            Self::Dart => "dart",
            Self::Svelte => "svelte",
            Self::Vue => "vue",
            Self::Astro => "astro",
            Self::Liquid => "liquid",
            Self::Pascal => "pascal",
            Self::Scala => "scala",
            Self::Lua => "lua",
            Self::Luau => "luau",
            Self::ObjC => "objc",
            Self::R => "r",
            Self::Solidity => "solidity",
            Self::Nix => "nix",
            Self::Terraform => "terraform",
            Self::Erlang => "erlang",
            Self::Yaml => "yaml",
            Self::Twig => "twig",
            Self::Xml => "xml",
            Self::Properties => "properties",
            Self::Gdscript => "gdscript",
            Self::GodotScene => "godot_scene",
            Self::GodotResource => "godot_resource",
            Self::GodotProject => "godot_project",
            Self::Unknown => "unknown",
        }
    }

    /// True only for `.tscn` / `.tres` / `project.godot`. `Gdscript` is
    /// deliberately excluded so a `.gd`→`.gd` link stays an ordinary static
    /// reference; the dynamic-reachability signal fires only for links
    /// originating in these engine-driven Godot files.
    pub const fn is_godot_non_script_file(self) -> bool {
        matches!(
            self,
            Self::GodotScene | Self::GodotResource | Self::GodotProject
        )
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub language: Language,
    pub start_line: i64,
    pub end_line: i64,
    pub start_column: i64,
    pub end_column: i64,
    pub docstring: Option<String>,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    pub is_exported: bool,
    pub is_async: bool,
    pub is_static: bool,
    pub is_abstract: bool,
    pub decorators: Vec<String>,
    pub type_parameters: Vec<String>,
    pub return_type: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub id: Option<i64>,
    pub source: String,
    pub target: String,
    pub kind: EdgeKind,
    pub metadata: Option<serde_json::Value>,
    pub line: Option<i64>,
    pub col: Option<i64>,
    pub provenance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRecord {
    pub path: String,
    pub content_hash: String,
    pub language: Language,
    pub size: i64,
    pub modified_at: i64,
    pub indexed_at: i64,
    pub node_count: i64,
    pub errors: Vec<String>,
}

/// Structural label for HOW a reference was extracted, orthogonal to the typed
/// [`EdgeKind`]. Godot-only today: it distinguishes the otherwise-opaque
/// `references`/`instantiates` edges a `.tscn`/`.tres` parser emits. It is NOT a
/// domain label and never extends [`EdgeKind`]; it rides a separate nullable
/// column / `edge.metadata.subkind`, mirroring the `is_function_ref`/`fnRef`
/// channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceSubkind {
    ScriptAttach,
    SceneInstance,
    ExtResource,
    GroupMember,
    SignalMethod,
    GdscriptLoadPath,
    Autoload,
}

impl ReferenceSubkind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScriptAttach => "script_attach",
            Self::SceneInstance => "scene_instance",
            Self::ExtResource => "ext_resource",
            Self::GroupMember => "group_member",
            Self::SignalMethod => "signal_method",
            Self::GdscriptLoadPath => "gdscript_load_path",
            Self::Autoload => "autoload",
        }
    }
}

impl fmt::Display for ReferenceSubkind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedRef {
    pub id: Option<i64>,
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: EdgeKind,
    pub line: i64,
    pub col: i64,
    pub candidates: Option<Vec<String>>,
    pub file_path: String,
    pub language: Language,
    /// Marks an upstream `function_ref` reference (function-as-value callback
    /// registration, types.ts:289). Internal-only: it carries `reference_kind`
    /// `references` and never persists as a distinct edge kind, but resolution
    /// tags the produced edge with `fnRef: true` metadata.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_function_ref: bool,
    /// Finer structural extraction label (Godot only), persisted to the nullable
    /// `unresolved_refs.reference_subkind` column. `None` for every non-Godot ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_subkind: Option<ReferenceSubkind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved_references: Vec<UnresolvedRef>,
    pub errors: Vec<String>,
    pub duration_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn sample_node() -> Node {
        Node {
            id: "function:abc123".to_string(),
            kind: NodeKind::Function,
            name: "calculateTotal".to_string(),
            qualified_name: "src/utils.ts::MathHelper.calculateTotal".to_string(),
            file_path: "src/utils.ts".to_string(),
            language: Language::TypeScript,
            start_line: 10,
            end_line: 20,
            start_column: 2,
            end_column: 4,
            docstring: Some("Adds totals".to_string()),
            signature: Some("calculateTotal(items: Item[]): number".to_string()),
            visibility: Some("public".to_string()),
            is_exported: true,
            is_async: false,
            is_static: true,
            is_abstract: false,
            decorators: vec!["memoize".to_string()],
            type_parameters: vec!["T".to_string()],
            return_type: Some("number".to_string()),
            updated_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn node_serializes_with_upstream_camel_case_keys() {
        let value = serde_json::to_value(sample_node()).expect("node serializes");
        let object = value.as_object().expect("node is a JSON object");

        assert!(object.contains_key("qualifiedName"));
        assert!(object.contains_key("filePath"));
        assert!(object.contains_key("startLine"));
        assert_eq!(
            value["qualifiedName"],
            "src/utils.ts::MathHelper.calculateTotal"
        );
        assert_eq!(value["filePath"], "src/utils.ts");
        assert_eq!(value["typeParameters"], json!(["T"]));
        assert!(!object.contains_key("qualified_name"));
        assert!(!object.contains_key("file_path"));
    }

    #[test]
    fn node_round_trips_upstream_json_shape() {
        let node = sample_node();
        let value = serde_json::to_value(&node).expect("node serializes");
        let round_tripped: Node = serde_json::from_value(value).expect("node deserializes");

        assert_eq!(round_tripped, node);
    }

    #[test]
    fn node_kind_function_serializes_to_upstream_exact_string() {
        assert_eq!(NodeKind::Function.as_str(), "function");
        assert_eq!(NodeKind::Function.to_string(), "function");
        assert_eq!(
            serde_json::to_value(NodeKind::Function).unwrap(),
            json!("function")
        );
    }

    #[test]
    fn edge_round_trips_with_upstream_kind_and_sqlite_id() {
        let edge = Edge {
            id: Some(42),
            source: "function:source".to_string(),
            target: "function:target".to_string(),
            kind: EdgeKind::Calls,
            metadata: Some(json!({ "receiver": "client" })),
            line: Some(12),
            col: Some(8),
            provenance: Some("tree-sitter".to_string()),
        };

        let value = serde_json::to_value(&edge).expect("edge serializes");
        assert_eq!(value["kind"], json!("calls"));
        assert_eq!(value["col"], json!(8));

        let round_tripped: Edge = serde_json::from_value(value).expect("edge deserializes");
        assert_eq!(round_tripped, edge);
    }

    #[test]
    fn file_record_and_unresolved_ref_use_upstream_camel_case() {
        let file = FileRecord {
            path: "src/utils.ts".to_string(),
            content_hash: "sha256".to_string(),
            language: Language::TypeScript,
            size: 1234,
            modified_at: 1_700_000_000_000,
            indexed_at: 1_700_000_001_000,
            node_count: 3,
            errors: vec!["warning".to_string()],
        };
        let file_value = serde_json::to_value(file).expect("file record serializes");
        assert_eq!(file_value["contentHash"], json!("sha256"));
        assert_eq!(file_value["modifiedAt"], json!(1_700_000_000_000_i64));
        assert_eq!(file_value["nodeCount"], json!(3));

        let unresolved = UnresolvedRef {
            id: None,
            from_node_id: "function:source".to_string(),
            reference_name: "makeClient".to_string(),
            reference_kind: EdgeKind::References,
            line: 11,
            col: 6,
            candidates: Some(vec!["src/client.ts::makeClient".to_string()]),
            file_path: "src/utils.ts".to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None,
        };
        let unresolved_value = serde_json::to_value(unresolved).expect("unresolved ref serializes");
        assert_eq!(unresolved_value["fromNodeId"], json!("function:source"));
        assert_eq!(unresolved_value["referenceKind"], json!("references"));
        assert_eq!(unresolved_value["filePath"], json!("src/utils.ts"));
    }

    #[test]
    fn exact_enum_inventories_match_upstream_types_ts() {
        assert_eq!(NODE_KIND_STRINGS, NodeKind::ALL.map(NodeKind::as_str));
        assert_eq!(EDGE_KIND_STRINGS, EdgeKind::ALL.map(EdgeKind::as_str));
        assert_eq!(LANGUAGE_STRINGS, Language::ALL.map(Language::as_str));

        let as_json: Vec<Value> = NodeKind::ALL
            .iter()
            .copied()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()
            .expect("node kinds serialize");
        assert_eq!(as_json[7], json!("function"));
        assert_eq!(as_json[15], json!("type_alias"));
    }

    #[test]
    fn every_node_kind_as_str_display_and_serde_agree() {
        for (kind, expected) in NodeKind::ALL.iter().copied().zip(NODE_KIND_STRINGS) {
            assert_eq!(kind.as_str(), expected);
            assert_eq!(kind.to_string(), expected);
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(expected));
            let back: NodeKind = serde_json::from_value(json!(expected)).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn every_edge_kind_as_str_display_and_serde_agree() {
        for (kind, expected) in EdgeKind::ALL.iter().copied().zip(EDGE_KIND_STRINGS) {
            assert_eq!(kind.as_str(), expected);
            assert_eq!(kind.to_string(), expected);
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(expected));
            let back: EdgeKind = serde_json::from_value(json!(expected)).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn every_language_as_str_display_and_serde_agree() {
        for (language, expected) in Language::ALL.iter().copied().zip(LANGUAGE_STRINGS) {
            assert_eq!(language.as_str(), expected);
            assert_eq!(language.to_string(), expected);
            assert_eq!(serde_json::to_value(language).unwrap(), json!(expected));
            let back: Language = serde_json::from_value(json!(expected)).unwrap();
            assert_eq!(back, language);
        }
    }

    #[test]
    fn is_godot_non_script_file_only_for_scene_resource_project() {
        assert!(Language::GodotScene.is_godot_non_script_file());
        assert!(Language::GodotResource.is_godot_non_script_file());
        assert!(Language::GodotProject.is_godot_non_script_file());
        assert!(!Language::Gdscript.is_godot_non_script_file());
        assert!(!Language::Rust.is_godot_non_script_file());
    }

    #[test]
    fn every_reference_subkind_as_str_display_and_serde_agree() {
        let all = [
            (ReferenceSubkind::ScriptAttach, "script_attach"),
            (ReferenceSubkind::SceneInstance, "scene_instance"),
            (ReferenceSubkind::ExtResource, "ext_resource"),
            (ReferenceSubkind::GroupMember, "group_member"),
            (ReferenceSubkind::SignalMethod, "signal_method"),
            (ReferenceSubkind::GdscriptLoadPath, "gdscript_load_path"),
            (ReferenceSubkind::Autoload, "autoload"),
        ];
        for (subkind, expected) in all {
            assert_eq!(subkind.as_str(), expected);
            assert_eq!(subkind.to_string(), expected);
            assert_eq!(serde_json::to_value(subkind).unwrap(), json!(expected));
            let back: ReferenceSubkind = serde_json::from_value(json!(expected)).unwrap();
            assert_eq!(back, subkind);
        }
    }

    #[test]
    fn unresolved_ref_skips_default_flags_but_round_trips_full() {
        let minimal = UnresolvedRef {
            id: None,
            from_node_id: "function:a".to_string(),
            reference_name: "x".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            col: 2,
            candidates: None,
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        };
        let value = serde_json::to_value(&minimal).unwrap();
        let object = value.as_object().unwrap();
        assert!(!object.contains_key("isFunctionRef"));
        assert!(!object.contains_key("referenceSubkind"));

        let full = UnresolvedRef {
            is_function_ref: true,
            reference_subkind: Some(ReferenceSubkind::ScriptAttach),
            ..minimal
        };
        let full_value = serde_json::to_value(&full).unwrap();
        assert_eq!(full_value["isFunctionRef"], json!(true));
        assert_eq!(full_value["referenceSubkind"], json!("script_attach"));
        let back: UnresolvedRef = serde_json::from_value(full_value).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn extraction_result_round_trips_with_camel_case_keys() {
        let result = ExtractionResult {
            nodes: vec![sample_node()],
            edges: vec![Edge {
                id: None,
                source: "function:a".to_string(),
                target: "function:b".to_string(),
                kind: EdgeKind::Calls,
                metadata: None,
                line: None,
                col: None,
                provenance: None,
            }],
            unresolved_references: Vec::new(),
            errors: vec!["oops".to_string()],
            duration_ms: 12,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert!(
            value
                .as_object()
                .unwrap()
                .contains_key("unresolvedReferences")
        );
        assert_eq!(value["durationMs"], json!(12));
        let back: ExtractionResult = serde_json::from_value(value).unwrap();
        assert_eq!(back, result);
    }
}
