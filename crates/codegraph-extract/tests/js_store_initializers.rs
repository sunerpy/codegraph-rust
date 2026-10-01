//! A store initializer's own calls belong to the store (#693, #1862).
//!
//! `export const useStore = create(persist((set) => ({ reset: () => set({}) }),
//! opts))` makes each inline action a function node of its own. The rest of
//! the initializer, the `create`/`persist` wrappers and their options, runs
//! when the store is built, so its calls stay attributed to the store, as they
//! were before the actions became nodes. An action's calls are never counted
//! for the store as well.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

fn extract(source: &str) -> ExtractionResult {
    let result = extract_source("store.ts", source, Some(Language::TypeScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    result
}

/// Sorted names of the `calls` refs made by the node named by `qualified`.
fn calls_from(result: &ExtractionResult, qualified: &str) -> Vec<String> {
    let ids = result
        .nodes
        .iter()
        .filter(|node| node.qualified_name == qualified)
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 1, "{qualified}: {:#?}", result.nodes);
    let mut names = result
        .unresolved_references
        .iter()
        .filter(|reference| {
            reference.reference_kind == EdgeKind::Calls && reference.from_node_id == ids[0]
        })
        .map(|reference| reference.reference_name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn wrapper_and_option_calls_stay_with_the_store() {
    let result = extract(
        "import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';
export const useStore = create(
  persist(
    (set, get) => ({
      count: initialCount(),
      reset: () => set({ count: 0 }),
      bump() { get().reset(); },
    }),
    { name: 'app', storage: createJSONStorage(() => localStorage) },
  ),
);
",
    );
    assert_eq!(
        calls_from(&result, "useStore"),
        vec!["create", "createJSONStorage", "initialCount", "persist"]
    );
    assert_eq!(calls_from(&result, "useStore::reset"), vec!["set"]);
    assert_eq!(
        calls_from(&result, "useStore::bump"),
        vec!["get", "get().reset"]
    );
}

#[test]
fn a_curried_store_mints_no_function_for_its_factory() {
    let result = extract(
        "import { create } from 'zustand';
interface S { bump(): void }
export const useCurried = create<S>()((set) => {
  const seed = loadSeed();
  return { bump: () => set({}) };
});
",
    );
    assert!(
        !result
            .nodes
            .iter()
            .any(|node| node.name == "useCurried" && node.kind == NodeKind::Function),
        "{:#?}",
        result.nodes
    );
    // The outer application is named by its callee text, as any curried
    // call is.
    assert_eq!(
        calls_from(&result, "useCurried"),
        vec!["create", "create<S>()", "loadSeed"]
    );
    assert_eq!(calls_from(&result, "useCurried::bump"), vec!["set"]);
}

#[test]
fn initializer_callees_follow_the_path_to_the_action_function() {
    use codegraph_extract::walker::js_store_initializer_callees;
    let callees = |source: &str| js_store_initializer_callees(source, Language::TypeScript, "s");
    let path = |names: &[&str]| Some(names.iter().map(|n| n.to_string()).collect::<Vec<_>>());
    assert_eq!(
        callees("export const s = createSelectors(create<S>()(persist((set) => ({ a() {} }))));"),
        path(&["createSelectors", "create", "persist"])
    );
    assert_eq!(
        callees("export const s = zs.create(() => ({ a() {} }));"),
        path(&["zs.create"])
    );
    // Only the parser decides what is a call, and only the path counts.
    for decoy in [
        "export const s = otherFactory(/* create( */ () => ({ a() {} }));",
        "export const s = otherFactory(\"create(\", () => ({ a() {} }));",
        "export const s = otherFactory(create(1), () => ({ a() {} }));",
        "export const s = otherFactory<ReturnType<typeof create>>(() => ({ a() {} }));",
    ] {
        assert_eq!(callees(decoy), path(&["otherFactory"]), "{decoy}");
    }
    assert_eq!(callees("export const s = plain(1);"), None);
    assert_eq!(
        callees("export const other = create(() => ({ a() {} }));"),
        None
    );
}
