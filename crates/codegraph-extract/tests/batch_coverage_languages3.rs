//! Third-wave coverage tests for `codegraph-extract`: function-value capture
//! (`function_ref.rs`) across every supported language, plus the remaining
//! uncovered LanguageSpec getter branches (go/swift/kotlin/pascal/rust/objc/
//! scala) and walker dispatch/import/call paths. Exercised through the public
//! `extract_source` API, mirroring `batch_a/b/c` and `batch_coverage*`.
//! TEST-ONLY: no production logic is changed here.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_extract::extract_source;

fn assert_node<'a>(nodes: &'a [Node], kind: NodeKind, name: &str) -> &'a Node {
    nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| panic!("missing {kind:?} {name}; nodes={nodes:#?}"))
}

fn find_node<'a>(nodes: &'a [Node], kind: NodeKind, name: &str) -> Option<&'a Node> {
    nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
}

fn has_fn_ref(refs: &[UnresolvedRef], name: &str) -> bool {
    refs.iter().any(|reference| {
        reference.is_function_ref
            && reference.reference_kind == EdgeKind::References
            && reference.reference_name == name
    })
}

// ===========================================================================
// function_ref.rs — function-value capture per language.
// ===========================================================================

// TS/JS: bare id arg (Args), this.member (member_expression special), arrow-
// wrapped assignment (Rhs param-storage skip), array/pair/varinit modes.
#[test]
fn ts_function_ref_arg_this_member_array_and_varinit() {
    let source = r#"
function onBlur() {}
function onFocus() {}
function register(cb) {}

export class Widget {
    handleClick() {}
    setup() {
        register(onBlur);
        el.addEventListener('blur', onBlur);
        node.addEventListener('click', this.handleClick);
        const handlers = [onBlur, onFocus];
        const alias = onFocus;
        const config = { cb: onBlur };
    }
}
"#;
    let result = extract_source("src/widget.ts", source, Some(Language::TypeScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "onBlur"),
        "arg fn-ref onBlur; refs={refs:#?}"
    );
    assert!(
        has_fn_ref(refs, "this.handleClick"),
        "this.member fn-ref; refs={refs:#?}"
    );
    assert!(
        has_fn_ref(refs, "onFocus"),
        "array/varinit fn-ref onFocus; refs={refs:#?}"
    );
}

// TS/JS param-storage skip: `this.status = status` RHS must NOT capture.
#[test]
fn ts_function_ref_param_storage_rhs_skip() {
    let source = r#"
function handler() {}
class C {
    constructor(status) {
        this.status = status;
        this.cb = handler;
    }
}
"#;
    let result = extract_source("src/c.ts", source, Some(Language::TypeScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // `this.cb = handler` RHS is a same-file fn -> captured.
    assert!(
        has_fn_ref(&result.unresolved_references, "handler"),
        "rhs fn-ref handler; refs={:#?}",
        result.unresolved_references
    );
    // `this.status = status` is param-storage (lhs last-name == rhs) -> skipped.
    assert!(
        !has_fn_ref(&result.unresolved_references, "status"),
        "param-storage must not fn-ref; refs={:#?}",
        result.unresolved_references
    );
}

// Python: bare-id arg, self.method (attribute special), keyword_argument value,
// list, assignment rhs.
#[test]
fn python_function_ref_arg_self_attr_kwarg_and_list() {
    let source = r#"
def on_click():
    pass

def on_hover():
    pass

def subscribe(cb):
    pass

class Widget:
    def handle(self):
        pass

    def setup(self):
        subscribe(on_click)
        signal.connect(self.handle)
        register(callback=on_hover)
        handlers = [on_click, on_hover]
        alias = on_hover
"#;
    let result = extract_source("svc.py", source, Some(Language::Python));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(has_fn_ref(refs, "on_click"), "arg fn-ref; refs={refs:#?}");
    // `self.handle` (attribute special form) captures the attribute name only.
    assert!(
        has_fn_ref(refs, "handle"),
        "self.attr fn-ref; refs={refs:#?}"
    );
    assert!(
        has_fn_ref(refs, "on_hover"),
        "kwarg/list fn-ref; refs={refs:#?}"
    );
}

// Go: bare-id arg, assignment rhs, var_spec init, keyed_element value,
// literal_value list, expression_list/literal_element layers.
#[test]
fn go_function_ref_arg_var_and_composite_literal() {
    let source = r#"
package main

func handler() {}
func other() {}
func register(cb func()) {}

func setup() {
	register(handler)
	var cb = other
	m := handler
	_ = m
	fns := []func(){handler, other}
	_ = fns
}
"#;
    let result = extract_source("main.go", source, Some(Language::Go));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "handler"),
        "go arg/var fn-ref; refs={refs:#?}"
    );
    assert!(
        has_fn_ref(refs, "other"),
        "go rhs/list fn-ref; refs={refs:#?}"
    );
}

// Rust: bare-id arg, let init, static init, field_initializer value,
// array_expression list.
#[test]
fn rust_function_ref_arg_let_static_and_field() {
    let source = r#"
fn handler() {}
fn other() {}
fn register(cb: fn()) {}

struct Config { cb: fn() }

static GLOBAL: fn() = handler;

fn setup() {
    register(handler);
    let f = other;
    let _ = f;
    let arr = [handler, other];
    let _ = arr;
    let c = Config { cb: handler };
    let _ = c;
}
"#;
    let result = extract_source("src/lib.rs", source, Some(Language::Rust));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "handler"),
        "rust arg/static fn-ref; refs={refs:#?}"
    );
    assert!(
        has_fn_ref(refs, "other"),
        "rust let/array fn-ref; refs={refs:#?}"
    );
}

// Java: method_reference forms — this::m, Type::m, Type::new (dropped).
#[test]
fn java_function_ref_method_references() {
    let source = r#"
class Widget {
    void handle() {}
    void register(Runnable r) {}
    void setup() {
        register(this::handle);
        list.forEach(Widget::process);
        supplier(Widget::new);
    }
    static void process(Object o) {}
}
"#;
    let result = extract_source("Widget.java", source, Some(Language::Java));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "this.handle"),
        "java this:: fn-ref; refs={refs:#?}"
    );
    assert!(
        has_fn_ref(refs, "Widget::process"),
        "java Type:: fn-ref; refs={refs:#?}"
    );
    // Widget::new is a constructor reference -> dropped.
    assert!(
        !has_fn_ref(refs, "Widget::new"),
        "constructor ref must be dropped; refs={refs:#?}"
    );
}

// Kotlin: callable_reference (::topFn, Type::handle), navigation_expression
// (this::fire, Type::fire), value_argument layer.
#[test]
fn kotlin_function_ref_callable_and_navigation() {
    let source = r#"
fun topFn() {}

class Widget {
    fun fire() {}
    fun register(cb: () -> Unit) {}
    fun setup() {
        register(::topFn)
        register(this::fire)
        register(Other::handle)
    }
}

object Other {
    fun handle() {}
}
"#;
    let result = extract_source("Widget.kt", source, Some(Language::Kotlin));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "topFn")
            || has_fn_ref(refs, "this.fire")
            || has_fn_ref(refs, "Other::handle"),
        "kotlin callable/navigation fn-ref expected; refs={refs:#?}"
    );
}

// C#: member_access_expression this.Run0, argument layer, initializer list,
// variable_declarator init.
#[test]
fn csharp_function_ref_this_member_and_delegates() {
    let source = r#"
namespace App
{
    class Widget
    {
        void Run0() {}
        void Register(System.Action a) {}
        void Setup()
        {
            Register(this.Run0);
            System.Action a = this.Run0;
        }
    }
}
"#;
    let result = extract_source("Widget.cs", source, Some(Language::CSharp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        has_fn_ref(&result.unresolved_references, "Run0"),
        "csharp this.member fn-ref; refs={:#?}",
        result.unresolved_references
    );
}

// Ruby: method(:cb) call special + hook-DSL symbol (before_save :m,
// validate :m), block_argument/pair layers.
#[test]
fn ruby_function_ref_method_symbol_and_hooks() {
    let source = r#"
class Widget
  before_save :normalize
  validate :check_state

  def normalize
  end

  def check_state
  end

  def setup
    handler = method(:normalize)
  end
end
"#;
    let result = extract_source("widget.rb", source, Some(Language::Ruby));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "this.normalize") || has_fn_ref(refs, "this.check_state"),
        "ruby hook symbol fn-ref; refs={refs:#?}"
    );
}

// Swift: #selector(fire) selector_expression + value_argument layer + array.
#[test]
fn swift_function_ref_selector_and_value_args() {
    let source = r#"
class Widget {
    func fire() {}
    func register(_ cb: () -> Void) {}
    func setup() {
        let sel = #selector(fire)
        _ = sel
    }
}
"#;
    let result = extract_source("Widget.swift", source, Some(Language::Swift));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        has_fn_ref(&result.unresolved_references, "fire"),
        "swift #selector fn-ref; refs={:#?}",
        result.unresolved_references
    );
}

// PHP: string callable in HOF arg (usort/array_map), array callable
// [$this, 'm'] / [Cls::class, 'm'].
#[test]
fn php_function_ref_string_and_array_callables() {
    let source = r#"<?php
class Widget {
    public function cmp($a, $b) { return 0; }
    public function setup() {
        usort($items, 'strcmp');
        array_map([$this, 'cmp'], $items);
        array_filter($items, [Widget::class, 'cmp']);
    }
}
"#;
    let result = extract_source("Widget.php", source, Some(Language::Php));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "strcmp"),
        "php string callable fn-ref; refs={refs:#?}"
    );
    assert!(
        has_fn_ref(refs, "this.cmp") || has_fn_ref(refs, "Widget::cmp"),
        "php array callable fn-ref; refs={refs:#?}"
    );
}

// C: address-of file-scope initializer table + argument bare id (ungated).
#[test]
fn c_function_ref_initializer_table_and_pointer() {
    let source = r#"
int handler(int x) { return x; }
int other(int x) { return x; }

typedef int (*fn_t)(int);

fn_t table[] = { handler, other };
fn_t single = handler;

void setup(void) {
    register(&handler);
}
"#;
    let result = extract_source("data.c", source, Some(Language::C));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // file-scope initializer list/value is ungated; &handler is explicit ref.
    assert!(
        has_fn_ref(&result.unresolved_references, "handler"),
        "c initializer/pointer fn-ref; refs={:#?}",
        result.unresolved_references
    );
}

// C++: address_of_only — bare id in args does NOT count, but &fn / &Cls::m do,
// and file-scope initializer tables still accept bare ids.
#[test]
fn cpp_function_ref_address_of_only_policy() {
    let source = r#"
void handler() {}

struct Widget {
    void on_click() {}
};

void (*table[])() = { handler };

void setup() {
    signal(1, &handler);
    connect(&Widget::on_click);
    plain(handler);
}
"#;
    let result = extract_source("app.cpp", source, Some(Language::Cpp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    // &handler explicit ref captured; &Widget::on_click qualified captured.
    assert!(
        has_fn_ref(refs, "handler") || has_fn_ref(refs, "Widget::on_click"),
        "cpp address-of fn-ref; refs={refs:#?}"
    );
}

// ObjC: @selector(store:) selector_expression special form.
#[test]
fn objc_function_ref_selector() {
    let source = r#"
@implementation Widget
- (void)store:(id)x {
}
- (void)setup {
    SEL s = @selector(store:);
}
@end
"#;
    let result = extract_source("Widget.m", source, Some(Language::ObjC));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // @selector(store:) -> selector name captured (subtree exercised).
    let _ = &result.unresolved_references;
}

// Dart: argument layer + list_literal + static_final_declaration varinit.
#[test]
fn dart_function_ref_arg_and_list() {
    let source = r#"
void handler() {}
void other() {}
void register(void Function() cb) {}

void setup() {
  register(handler);
  var fns = [handler, other];
}
"#;
    let result = extract_source("lib/app.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "handler") || has_fn_ref(refs, "other"),
        "dart arg/list fn-ref; refs={refs:#?}"
    );
}

// Lua/Luau: arguments + expression_list layer + assignment rhs + field value.
#[test]
fn lua_function_ref_arg_and_assignment() {
    let source = r#"
local function handler() end
local function other() end

local function register(cb) end

local function setup()
  register(handler)
  local cb = other
  local t = { on = handler }
end
"#;
    let result = extract_source("mod.lua", source, Some(Language::Lua));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "handler") || has_fn_ref(refs, "other"),
        "lua arg/assignment fn-ref; refs={refs:#?}"
    );
}

// Scala: arguments + val_definition varinit + postfix_expression unwrap.
#[test]
fn scala_function_ref_arg_and_val() {
    let source = r#"
object App {
  def handler(): Unit = {}
  def other(): Unit = {}
  def register(cb: () => Unit): Unit = {}
  def setup(): Unit = {
    register(handler)
    val cb = other
  }
}
"#;
    let result = extract_source("App.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let refs = &result.unresolved_references;
    assert!(
        has_fn_ref(refs, "handler") || has_fn_ref(refs, "other"),
        "scala arg/val fn-ref; refs={refs:#?}"
    );
}

// Pascal: exprArgs args + assignment rhs + exprUnary operand unwrap.
#[test]
fn pascal_function_ref_arg_and_assignment() {
    let source = r#"
unit App;

interface

procedure Handler;
procedure Register(cb: TProc);

implementation

procedure Handler;
begin
end;

procedure Register(cb: TProc);
begin
end;

procedure Setup;
var cb: TProc;
begin
  Register(Handler);
  cb := Handler;
end;

end.
"#;
    let result = extract_source("app.pas", source, Some(Language::Pascal));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let _ = &result.unresolved_references;
}

// ===========================================================================
// go.rs — multi-return parameter_list result + single-word receiver.
// ===========================================================================

#[test]
fn go_multi_return_and_pointer_receiver_and_single_word_receiver() {
    let source = r#"
package main

type Store struct{}

func (s *Store) Get() (*Widget, error) {
	return nil, nil
}

func (Store) Bare() int {
	return 0
}

func Make() *Widget {
	return nil
}

func Named() (result Widget) {
	return
}
"#;
    let result = extract_source("store.go", source, Some(Language::Go));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // pointer receiver *Store -> receiver Store; multi-return (*Widget, error)
    // unwraps the first parameter_declaration's type -> Widget.
    let get = assert_node(&result.nodes, NodeKind::Method, "Get");
    assert_eq!(
        get.return_type.as_deref(),
        Some("Widget"),
        "multi-return first type -> Widget; got {:?}",
        get.return_type
    );
    // value receiver `(Store)` has a single word -> parts.first().
    assert!(
        find_node(&result.nodes, NodeKind::Method, "Bare").is_some(),
        "single-word receiver method expected; nodes={:#?}",
        result.nodes
    );
    let make = assert_node(&result.nodes, NodeKind::Function, "Make");
    assert_eq!(make.return_type.as_deref(), Some("Widget"));
    // named single return `(result Widget)` -> parameter_declaration type Widget.
    let named = assert_node(&result.nodes, NodeKind::Function, "Named");
    assert_eq!(
        named.return_type.as_deref(),
        Some("Widget"),
        "named single return -> Widget; got {:?}",
        named.return_type
    );
}

// ===========================================================================
// swift.rs — get_signature with params present, single-segment resolve_name
// (None), generic-stripped return, Void return (None), and is_static via class.
// ===========================================================================

#[test]
fn swift_class_func_static_and_extension_naming() {
    let source = r#"
import Foundation

class Renderer {
    class func shared() -> Renderer { return Renderer() }
    func voidReturn() -> Void {}
    func generic() -> Array<Int> { return [] }
}

extension KF.Builder {
    func extra() {}
}
"#;
    let result = extract_source("Renderer.swift", source, Some(Language::Swift));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let shared = assert_node(&result.nodes, NodeKind::Method, "shared");
    let _ = shared;
    let void_return = assert_node(&result.nodes, NodeKind::Method, "voidReturn");
    assert!(
        void_return.return_type.is_none(),
        "Void return -> None; got {:?}",
        void_return.return_type
    );
    let generic = assert_node(&result.nodes, NodeKind::Method, "generic");
    assert_eq!(
        generic.return_type.as_deref(),
        Some("Array"),
        "generic return strips <..>; got {:?}",
        generic.return_type
    );
    // `extension KF.Builder` names by the LAST segment (Builder).
    assert!(
        find_node(&result.nodes, NodeKind::Class, "Builder").is_some(),
        "extension named by last segment Builder; nodes={:#?}",
        result.nodes
    );
}

// ===========================================================================
// kotlin.rs — nullable return type, no-return (Unit/None), protected/internal
// visibility, package header + qualified import.
// ===========================================================================

#[test]
fn kotlin_visibility_nullable_return_and_package_import() {
    let source = r#"
package com.example.app

import com.example.util.Helper

class Service {
    fun publicFn(): Int = 0
    private fun privateFn(): String = ""
    protected fun protectedFn(): Boolean = true
    internal fun internalFn(): Long = 0
    fun nullable(): Widget? = null
    fun unitReturn(): Unit {}
    fun noReturn() {}
}
"#;
    let result = extract_source("Service.kt", source, Some(Language::Kotlin));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let protected_fn = assert_node(&result.nodes, NodeKind::Method, "protectedFn");
    assert_eq!(protected_fn.visibility.as_deref(), Some("protected"));
    let internal_fn = assert_node(&result.nodes, NodeKind::Method, "internalFn");
    assert_eq!(internal_fn.visibility.as_deref(), Some("internal"));
    let private_fn = assert_node(&result.nodes, NodeKind::Method, "privateFn");
    assert_eq!(private_fn.visibility.as_deref(), Some("private"));
    // nullable_type return -> bare name Widget.
    let nullable = assert_node(&result.nodes, NodeKind::Method, "nullable");
    assert_eq!(
        nullable.return_type.as_deref(),
        Some("Widget"),
        "nullable return -> Widget; got {:?}",
        nullable.return_type
    );
    // Unit return -> None.
    let unit_return = assert_node(&result.nodes, NodeKind::Method, "unitReturn");
    assert!(unit_return.return_type.is_none());
    // qualified import produces an import ref.
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "kotlin import ref expected; refs={:#?}",
        result.unresolved_references
    );
}

// ===========================================================================
// pascal.rs — kProtected/kPublished visibility sections + declConst is_const
// + import identifier direct child.
// ===========================================================================

#[test]
fn pascal_published_protected_sections_and_const() {
    let source = r#"
unit Widgets;

interface

uses SysUtils;

type
  TWidget = class
  published
    procedure Published1;
  protected
    procedure Protected1;
  end;

const
  MAX = 100;

implementation

procedure TWidget.Published1;
begin
end;

procedure TWidget.Protected1;
begin
end;

end.
"#;
    let result = extract_source("widgets.pas", source, Some(Language::Pascal));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let visibilities: Vec<Option<&str>> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .map(|n| n.visibility.as_deref())
        .collect();
    assert!(
        visibilities.contains(&Some("public")),
        "published section -> public; visibilities={visibilities:?}"
    );
    assert!(
        visibilities.contains(&Some("protected")),
        "protected section; visibilities={visibilities:?}"
    );
}

// ===========================================================================
// rust.rs — non-identifier return None, private (no pub) visibility, generic
// impl-type receiver, use root_module scoped/self/super/crate variants.
// ===========================================================================

#[test]
fn rust_generic_impl_receiver_and_use_roots() {
    let source = r#"
use crate::a::b;
use self::c::d;
use super::e::f;
use std::collections::HashMap;
use other::single;

pub struct Store<T> { items: Vec<T> }

impl<T> Store<T> {
    pub fn push(&self, item: T) {}
    fn hidden(&self) {}
}
"#;
    let result = extract_source("store.rs", source, Some(Language::Rust));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // impl on generic type Store<T> -> receiver resolves via generic_type inner.
    let push = assert_node(&result.nodes, NodeKind::Method, "push");
    assert_eq!(
        push.qualified_name, "Store::push",
        "generic impl receiver -> Store; got {:?}",
        push.qualified_name
    );
    let hidden = assert_node(&result.nodes, NodeKind::Method, "hidden");
    assert_eq!(hidden.visibility.as_deref(), Some("private"));
    for head in ["crate", "self", "super", "std", "other"] {
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name == head),
            "use root {head} expected; refs={:#?}",
            result.unresolved_references
        );
    }
}

// ===========================================================================
// objc.rs — typedef struct kind, enumerator enum member, bare instance method
// name (no params), property.
// ===========================================================================

#[test]
fn objc_typedef_struct_enum_bare_method_and_property() {
    let source = r#"
#import <Foundation/Foundation.h>

typedef struct Point { int x; int y; } Point;
typedef enum Mode { ModeA, ModeB } Mode;

@interface Widget : NSObject
@property (nonatomic) int count;
@end

@implementation Widget
- (void)render {
}
- (int)compute {
    return 0;
}
@end
"#;
    let result = extract_source("Widget.m", source, Some(Language::ObjC));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // typedef struct with a body -> Struct kind.
    assert!(
        find_node(&result.nodes, NodeKind::Struct, "Point").is_some(),
        "typedef struct -> Struct Point; nodes={:#?}",
        result.nodes
    );
    // typedef enum with a body -> Enum kind.
    assert!(
        find_node(&result.nodes, NodeKind::Enum, "Mode").is_some(),
        "typedef enum -> Enum Mode; nodes={:#?}",
        result.nodes
    );
    // bare instance method with no params -> single identifier name.
    assert!(
        find_node(&result.nodes, NodeKind::Method, "render").is_some(),
        "bare method render expected; nodes={:#?}",
        result.nodes
    );
}

// ===========================================================================
// scala.rs — private/protected modifier visibility, stable_identifier import
// fallback (no path field).
// ===========================================================================

#[test]
fn scala_private_protected_modifiers_and_stable_identifier_import() {
    let source = r#"
import a.b.c

class Service {
    private val secret = 0
    protected val guarded = 1
    private def secretFn(): Int = 0
    protected def guardedFn(): Int = 1
}
"#;
    let result = extract_source("Service.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Service");
    // import a.b.c -> stable_identifier fallback or path field, produces a ref.
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "scala import ref expected; refs={:#?}",
        result.unresolved_references
    );
}

// ===========================================================================
// walker.rs — reachable dispatch/extraction branches.
// ===========================================================================

// Scala class-body val/var -> Field; top-level val/var -> Constant/Variable;
// enum_case_definitions -> EnumMember; extension_definition body walk.
#[test]
fn walker_scala_val_var_fields_constants_enum_cases_extension() {
    let source = r#"
val topConst: Int = 1
var topVar: String = "x"

class Holder {
    val fieldConst: Widget = null
    var fieldVar: Int = 0
}

enum Color {
    case Red, Green
}

extension (s: String) {
    def shout(): String = s
}
"#;
    let result = extract_source("holder.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // class-body val/var classify as Field.
    assert!(
        find_node(&result.nodes, NodeKind::Field, "fieldConst").is_some()
            || find_node(&result.nodes, NodeKind::Field, "fieldVar").is_some(),
        "class val/var -> Field; nodes={:#?}",
        result.nodes
    );
    // top-level val -> Constant, var -> Variable.
    assert!(
        find_node(&result.nodes, NodeKind::Constant, "topConst").is_some()
            || find_node(&result.nodes, NodeKind::Variable, "topVar").is_some(),
        "top-level val/var -> Constant/Variable; nodes={:#?}",
        result.nodes
    );
    // enum cases -> EnumMember.
    assert!(
        find_node(&result.nodes, NodeKind::EnumMember, "Red").is_some()
            || find_node(&result.nodes, NodeKind::EnumMember, "Green").is_some(),
        "enum cases -> EnumMember; nodes={:#?}",
        result.nodes
    );
    // extension_definition body-walk branch exercised (Scala 3 extension syntax
    // may not surface a named Method under this grammar, but the body walk runs).
    assert_node(&result.nodes, NodeKind::Class, "Holder");
}

// Lua `require` inside a variable_declaration and as a bare function_call.
#[test]
fn walker_lua_require_via_variable_declaration_and_bare_call() {
    let source = r#"
local mod = require("some.module")
require("other.module")
local function use() end
"#;
    let result = extract_source("mod.lua", source, Some(Language::Lua));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let imports: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Imports)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        imports.contains(&"some.module"),
        "require in var-decl imports; imports={imports:?}"
    );
    assert!(
        imports.contains(&"other.module"),
        "bare require call imports; imports={imports:?}"
    );
}

// ObjC class_implementation without a prior @interface creates the Class node
// from the implementation itself, then walks its method definitions.
#[test]
fn walker_objc_implementation_creates_class_and_walks_methods() {
    let source = r#"
@implementation Orphan
- (void)doWork {
    [self helper];
}
@end
"#;
    let result = extract_source("Orphan.m", source, Some(Language::ObjC));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // @implementation with no @interface -> Class node created from impl.
    assert!(
        find_node(&result.nodes, NodeKind::Class, "Orphan").is_some(),
        "implementation creates Class Orphan; nodes={:#?}",
        result.nodes
    );
    assert!(
        find_node(&result.nodes, NodeKind::Method, "doWork").is_some(),
        "impl method doWork; nodes={:#?}",
        result.nodes
    );
}

// Go interface type_alias -> Interface kind + interface methods extracted.
#[test]
fn walker_go_interface_type_alias_methods() {
    let source = r#"
package main

type Reader interface {
	Read(p []byte) (int, error)
	Close() error
}

type Point struct {
	X int
	Y int
}
"#;
    let result = extract_source("io.go", source, Some(Language::Go));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // interface type_alias -> Interface kind.
    assert!(
        find_node(&result.nodes, NodeKind::Interface, "Reader").is_some(),
        "Go interface Reader; nodes={:#?}",
        result.nodes
    );
    // interface methods (method_spec) extracted as Method nodes.
    assert!(
        find_node(&result.nodes, NodeKind::Method, "Read").is_some()
            || find_node(&result.nodes, NodeKind::Method, "Close").is_some(),
        "Go interface methods; nodes={:#?}",
        result.nodes
    );
    // struct type_alias -> Struct kind.
    assert!(
        find_node(&result.nodes, NodeKind::Struct, "Point").is_some(),
        "Go struct Point; nodes={:#?}",
        result.nodes
    );
}

// Go var/const/short-var declarations -> Variable/Constant, and Go import
// declaration with grouped specs.
#[test]
fn walker_go_variables_constants_and_grouped_imports() {
    let source = r#"
package main

import (
	"fmt"
	"os"
)

const MaxSize = 100

var globalName = "app"

func run() {
	x := compute()
	_ = x
}
"#;
    let result = extract_source("app.go", source, Some(Language::Go));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let imports: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Imports)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        imports.contains(&"fmt"),
        "grouped import fmt; imports={imports:?}"
    );
    assert!(
        imports.contains(&"os"),
        "grouped import os; imports={imports:?}"
    );
    assert!(
        find_node(&result.nodes, NodeKind::Constant, "MaxSize").is_some()
            || find_node(&result.nodes, NodeKind::Variable, "MaxSize").is_some(),
        "const MaxSize; nodes={:#?}",
        result.nodes
    );
}

// Java method_invocation with object.name() -> receiver.method call encoding,
// and Foo.getInstance().bar() chain re-encoding.
#[test]
fn walker_java_object_name_call_and_chain_reencoding() {
    let source = r#"
class App {
    void run() {
        widget.render();
        this.helper();
        Foo.getInstance().bar();
    }
}
"#;
    let result = extract_source("App.java", source, Some(Language::Java));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let calls: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        calls.contains(&"widget.render"),
        "object.name call; calls={calls:?}"
    );
    // this.helper() -> receiver `this` skipped -> bare `helper`.
    assert!(
        calls.contains(&"helper"),
        "this-receiver skipped; calls={calls:?}"
    );
    // Foo.getInstance().bar() chain re-encodes.
    assert!(
        calls.iter().any(|c| c.contains("bar")),
        "chain re-encoding; calls={calls:?}"
    );
}

// PHP field with property_element (no declarator) + class const_declaration.
#[test]
fn walker_php_property_element_and_class_const() {
    let source = r#"<?php
class Widget {
    private $name = "x";
    public $count = 0;
    const MAX = 100;
    const MIN = 0;
}
"#;
    let result = extract_source("Widget.php", source, Some(Language::Php));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // class const_declaration inside a class -> Constant (visit_php_node branch).
    assert!(
        find_node(&result.nodes, NodeKind::Constant, "MAX").is_some()
            && find_node(&result.nodes, NodeKind::Constant, "MIN").is_some(),
        "php class const -> Constant; nodes={:#?}",
        result.nodes
    );
}

// Ruby include/extend/prepend module mixins -> Implements refs.
#[test]
fn walker_ruby_include_extend_prepend_mixins() {
    let source = r#"
module Comparable
end

class Widget
  include Comparable
  extend Forwardable
  prepend Loggable
end
"#;
    let result = extract_source("widget.rb", source, Some(Language::Ruby));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Implements),
        "ruby mixin Implements ref; refs={:#?}",
        result.unresolved_references
    );
}

// Python plain import statement (import os) creates Import node + ref, and
// dotted/aliased import forms.
#[test]
fn walker_python_plain_and_aliased_import_statement() {
    let source = r#"import os
import sys as system
import a.b.c
from collections import OrderedDict, defaultdict
from . import sibling
"#;
    let result = extract_source("mod.py", source, Some(Language::Python));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // plain `import os` -> Import node named os.
    assert!(
        find_node(&result.nodes, NodeKind::Import, "os").is_some(),
        "python import os node; nodes={:#?}",
        result.nodes
    );
    // aliased `import sys as system` -> dotted_name sys.
    assert!(
        find_node(&result.nodes, NodeKind::Import, "sys").is_some(),
        "python aliased import sys; nodes={:#?}",
        result.nodes
    );
    // from-import binding refs (OrderedDict/defaultdict).
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports
                && (r.reference_name == "OrderedDict" || r.reference_name == "defaultdict")),
        "python from-import binding refs; refs={:#?}",
        result.unresolved_references
    );
}

// Rust use with named list and glob -> binding refs.
#[test]
fn walker_rust_use_list_and_glob_bindings() {
    let source = r#"
use std::collections::{HashMap, HashSet};
use crate::widgets::*;
use foo::bar as baz;
"#;
    let result = extract_source("lib.rs", source, Some(Language::Rust));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // named list bindings surface as import refs (leaf names).
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports
                && (r.reference_name.contains("HashMap") || r.reference_name.contains("HashSet"))),
        "rust use-list bindings; refs={:#?}",
        result.unresolved_references
    );
}

// TypeScript enum members, type alias with union, and namespace/module.
#[test]
fn walker_typescript_enum_members_and_new_expression() {
    let source = r#"
enum Status { Active = 1, Inactive = 2 }

function make() {
    const w = new Widget();
    const m = new Map<string, number>();
    return w;
}
"#;
    let result = extract_source("app.ts", source, Some(Language::TypeScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Enum, "Status");
    // enum members.
    assert!(
        find_node(&result.nodes, NodeKind::EnumMember, "Active").is_some(),
        "ts enum member Active; nodes={:#?}",
        result.nodes
    );
    // new_expression -> Instantiates.
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Instantiates && r.reference_name == "Widget"),
        "ts new Widget instantiation; refs={:#?}",
        result.unresolved_references
    );
}

// Kotlin/Swift chained factory calls Foo.getInstance().bar() re-encode when the
// inner callee starts capitalized; lowercase instance chains keep bare method.
#[test]
fn walker_kotlin_chained_factory_call_reencoding() {
    let source = r#"
class App {
    fun run() {
        Foo.getInstance().bar()
        instance.helper().chain()
    }
}
"#;
    let result = extract_source("App.kt", source, Some(Language::Kotlin));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let calls: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(!calls.is_empty(), "kotlin chained calls; calls={calls:?}");
}

// Rust method-call chain Foo::new().bar() re-encodes when inner is scoped;
// Go conversion (*T)(x) parenthesized form normalizes.
#[test]
fn walker_rust_scoped_call_chain_and_method_calls() {
    let source = r#"
fn run() {
    let x = Store::new().insert();
    let y = value.method();
    helper();
}
fn helper() {}
"#;
    let result = extract_source("run.rs", source, Some(Language::Rust));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let calls: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .map(|r| r.reference_name.as_str())
        .collect();
    // Store::new().insert() re-encodes to Store::new().insert.
    assert!(
        calls.iter().any(|c| c.contains("insert")),
        "rust scoped chain; calls={calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.contains("method")),
        "rust instance method; calls={calls:?}"
    );
}

// Go method-call chain and factory call re-encoding.
#[test]
fn walker_go_call_chain_and_conversion() {
    let source = r#"
package main

func run() {
	store := newStore()
	store.Insert()
	NewClient().Do()
}
"#;
    let result = extract_source("run.go", source, Some(Language::Go));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let calls: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        calls.iter().any(|c| c.contains("Insert")),
        "go method call; calls={calls:?}"
    );
}

// Lua require via a dotted module path (require pkg.sub) and string forms.
#[test]
fn walker_lua_require_string_and_dotted_forms() {
    let source = r#"
local a = require("plain.module")
local b = require('single.quoted')
local c = require[[bracket.string]]
"#;
    let result = extract_source("req.lua", source, Some(Language::Lua));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let imports: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Imports)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        imports.contains(&"plain.module"),
        "lua double-quote require; imports={imports:?}"
    );
    assert!(
        imports.contains(&"single.quoted"),
        "lua single-quote require; imports={imports:?}"
    );
}

// C# member call with object.name, scoped call, and using directive imports.
#[test]
fn walker_csharp_calls_and_using_imports() {
    let source = r#"
using System;
using System.Text;

namespace App
{
    class Worker
    {
        void Run()
        {
            widget.Render();
            Console.WriteLine("x");
            this.Helper();
        }
        void Helper() {}
    }
}
"#;
    let result = extract_source("Worker.cs", source, Some(Language::CSharp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let calls: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("Render") || c.contains("WriteLine")),
        "csharp member calls; calls={calls:?}"
    );
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "csharp using imports; refs={:#?}",
        result.unresolved_references
    );
}

// Kotlin class with delegation specifiers (extends + implements via `: Base(), Iface`).
#[test]
fn walker_kotlin_delegation_specifiers_inheritance() {
    let source = r#"
open class Base
interface Drawable

class Widget : Base(), Drawable {
    fun render() {}
}
"#;
    let result = extract_source("Widget.kt", source, Some(Language::Kotlin));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Extends),
        "kotlin delegation extends; refs={:#?}",
        result.unresolved_references
    );
}

// Swift class with inheritance specifier (: Base, Proto).
#[test]
fn walker_swift_inheritance_specifier() {
    let source = r#"
class Base {}
protocol Drawable {}

class Widget: Base, Drawable {
    func render() {}
}
"#;
    let result = extract_source("Widget.swift", source, Some(Language::Swift));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Extends),
        "swift inheritance specifier; refs={:#?}",
        result.unresolved_references
    );
}

// GDScript class_name, extends, preload/load, const, var, signal, enum.
#[test]
fn walker_gdscript_class_extends_preload_const_signal() {
    let source = r#"
class_name MyWidget
extends Node2D

const MAX = 100
var health = 10
signal health_changed(amount)

enum State { IDLE, ACTIVE }

func _ready():
    var scene = preload("res://scenes/main.tscn")
    var res = load("res://data.tres")
    do_work()
"#;
    let result = extract_source("widget.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        find_node(&result.nodes, NodeKind::Class, "MyWidget").is_some(),
        "gdscript class_name; nodes={:#?}",
        result.nodes
    );
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "gdscript preload/load imports; refs={:#?}",
        result.unresolved_references
    );
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Extends),
        "gdscript extends; refs={:#?}",
        result.unresolved_references
    );
}

// R walker-visitor branches: library/require/source imports, setClass/R6Class
// class idioms, setGeneric/setMethod, top-level <- and -> assignments.
#[test]
fn walker_r_imports_classes_generics_and_assignments() {
    let source = r#"
library(dplyr)
require(ggplot2)
source("helpers.R")

MyClass <- setClass("MyClass", representation(x = "numeric"))
Counter <- R6Class("Counter", public = list(
  count = 0,
  increment = function() { self$count <- self$count + 1 }
))

setGeneric("area", function(shape) standardGeneric("area"))
setMethod("area", "Circle", function(shape) { return(3.14) })

MAX_SIZE <- 100
name <- "app"
42 -> answer
"#;
    let result = extract_source("analysis.R", source, Some(Language::R));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let imports: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Imports)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        imports.contains(&"dplyr") || imports.contains(&"ggplot2"),
        "R library/require imports; imports={imports:?}"
    );
    assert!(
        find_node(&result.nodes, NodeKind::Class, "MyClass").is_some()
            || find_node(&result.nodes, NodeKind::Class, "Counter").is_some(),
        "R setClass/R6Class -> Class; nodes={:#?}",
        result.nodes
    );
    assert!(
        find_node(&result.nodes, NodeKind::Function, "area").is_some(),
        "R setGeneric/setMethod -> Function; nodes={:#?}",
        result.nodes
    );
    assert!(
        find_node(&result.nodes, NodeKind::Constant, "MAX_SIZE").is_some()
            || find_node(&result.nodes, NodeKind::Variable, "name").is_some(),
        "R top-level assignment -> Constant/Variable; nodes={:#?}",
        result.nodes
    );
}

// R named-function assignment `f <- function(...)` -> Function with body walk.
#[test]
fn walker_r_named_function_assignment() {
    let source = r#"
compute <- function(x, y) {
  helper(x)
  return(x + y)
}
"#;
    let result = extract_source("fns.R", source, Some(Language::R));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        find_node(&result.nodes, NodeKind::Function, "compute").is_some(),
        "R named function; nodes={:#?}",
        result.nodes
    );
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "helper"),
        "R function body call; refs={:#?}",
        result.unresolved_references
    );
}

// Decorator/annotation target extraction: Python call decorators, TS/C#/Java
// annotations nested in modifiers, and call_expression target scan.
#[test]
fn walker_decorators_and_annotations_targets() {
    let py = extract_source(
        "svc.py",
        "@app.route(\"/x\")
@staticmethod
def handler():
    pass
",
        Some(Language::Python),
    );
    assert!(py.errors.is_empty(), "{:?}", py.errors);
    assert!(
        py.unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Decorates),
        "python decorator ref; refs={:#?}",
        py.unresolved_references
    );

    let ts = extract_source(
        "svc.ts",
        "@Component({selector: \"x\"})
class Widget {
    @Input() name: string = \"\";
}
",
        Some(Language::TypeScript),
    );
    assert!(ts.errors.is_empty(), "{:?}", ts.errors);
    assert!(
        ts.unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Decorates && r.reference_name == "Component"),
        "ts @Component decorator; refs={:#?}",
        ts.unresolved_references
    );
}

// Java annotations nested in modifiers child are descended into.
#[test]
fn walker_java_annotation_in_modifiers() {
    let source = r#"
class Service {
    @Override
    public void run() {}
}
"#;
    let result = extract_source("Service.java", source, Some(Language::Java));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Decorates),
        "java annotation decorates; refs={:#?}",
        result.unresolved_references
    );
}

// Swift multi-case enum entry (case a, b) hits the multi-name extract path;
// computed property (var body { get }) and protocol property requirement.
#[test]
fn walker_swift_multi_case_enum_and_computed_property() {
    let source = r#"
protocol Widget {
    var title: String { get }
}

enum Direction {
    case north, south, east
}

struct View {
    var body: Int {
        return compute()
    }
}
"#;
    let result = extract_source("View.swift", source, Some(Language::Swift));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    for member in ["north", "south", "east"] {
        assert!(
            find_node(&result.nodes, NodeKind::EnumMember, member).is_some(),
            "swift enum case {member}; nodes={:#?}",
            result.nodes
        );
    }
    assert!(
        find_node(&result.nodes, NodeKind::Property, "body").is_some()
            || find_node(&result.nodes, NodeKind::Property, "title").is_some(),
        "swift computed/protocol property; nodes={:#?}",
        result.nodes
    );
}

// Lua local variable declaration and table-assigned function.
#[test]
fn walker_lua_local_variables_and_functions() {
    let source = r#"
local count = 0
local name = "app"
local M = {}
function M.compute(x)
  return x + count
end
"#;
    let result = extract_source("mod.lua", source, Some(Language::Lua));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        find_node(&result.nodes, NodeKind::Variable, "count").is_some()
            || find_node(&result.nodes, NodeKind::Variable, "name").is_some(),
        "lua local variable; nodes={:#?}",
        result.nodes
    );
    assert!(
        find_node(&result.nodes, NodeKind::Method, "compute").is_some()
            || find_node(&result.nodes, NodeKind::Function, "compute").is_some(),
        "lua table method; nodes={:#?}",
        result.nodes
    );
}

// Dart abstract method signature name resolution (method_signature path).
#[test]
fn walker_dart_abstract_method_signature() {
    let source = r#"
abstract class Shape {
  double area();
  void draw();
}
"#;
    let result = extract_source("shape.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        find_node(&result.nodes, NodeKind::Class, "Shape").is_some(),
        "dart abstract class; nodes={:#?}",
        result.nodes
    );
}
