//! Vue store collections and RTK Query endpoints and hooks are function nodes
//! (W7-16b, upstream `extractVariable`'s store shapes).
//!
//! `defineStore({ actions: {…} })`, `createStore`, `new Vuex.Store` and a
//! Vuex module's `export default { mutations: {…} }` give each collection
//! member a node; `createApi({ endpoints })` gives each endpoint one, and
//! `export const { useGetPostsQuery } = api` each generated hook. The rest of
//! a store's initializer stays the store's (#693), as in
//! `js_store_initializers.rs`.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind};
use codegraph_extract::extract_source;
use codegraph_extract::walker::RTK_GENERATED_HOOK_SIGNATURE;

fn extract(path: &str, source: &str) -> ExtractionResult {
    let language = if path.ends_with(".js") {
        Language::JavaScript
    } else {
        Language::TypeScript
    };
    let result = extract_source(path, source, Some(language));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    result
}

/// The one node qualified as `qualified`.
#[track_caller]
fn node<'r>(result: &'r ExtractionResult, qualified: &str) -> &'r Node {
    let nodes = result
        .nodes
        .iter()
        .filter(|node| node.qualified_name == qualified)
        .collect::<Vec<_>>();
    assert_eq!(nodes.len(), 1, "{qualified}: {:#?}", result.nodes);
    nodes[0]
}

/// Sorted qualified names of the function nodes.
fn functions(result: &ExtractionResult) -> Vec<String> {
    let mut names = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Function)
        .map(|node| node.qualified_name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// Sorted names of the `calls` refs made by the node with id `id`.
fn calls_of(result: &ExtractionResult, id: &str) -> Vec<String> {
    let mut names = result
        .unresolved_references
        .iter()
        .filter(|reference| {
            reference.reference_kind == EdgeKind::Calls && reference.from_node_id == id
        })
        .map(|reference| reference.reference_name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// Sorted names of the `calls` refs made by the node qualified as `qualified`.
#[track_caller]
fn calls_from(result: &ExtractionResult, qualified: &str) -> Vec<String> {
    let id = node(result, qualified).id.clone();
    calls_of(result, &id)
}

#[test]
fn rtk_endpoints_are_functions_of_the_api() {
    let result = extract(
        "fiatRampApi.ts",
        "import { createApi, fetchBaseQuery } from '@reduxjs/toolkit/query/react';
import { fetchRamps } from './ramps';

export const fiatRampApi = createApi({
  reducerPath: 'fiatRampApi',
  baseQuery: fetchBaseQuery({ baseUrl: '/' }),
  endpoints: build => ({
    getFiatRamps: build.query({
      queryFn: async () => {
        const data = await fetchRamps();
        return { data };
      },
    }),
    placeOrder: build.mutation({
      query: body => ({ url: 'order', method: 'POST', body: encode(body) }),
    }),
    listPages: build.infiniteQuery({
      query: ({ pageParam }) => pageOf(pageParam),
    }),
    checkConnection: build.mutation({
      queryFn: makeCheckFn('/check'),
    }),
  }),
});
",
    );
    assert_eq!(
        functions(&result),
        [
            "fiatRampApi::checkConnection",
            "fiatRampApi::getFiatRamps",
            "fiatRampApi::listPages",
            "fiatRampApi::placeOrder",
        ]
    );
    // An endpoint with a handler spans it; its calls are its own.
    let get = node(&result, "fiatRampApi::getFiatRamps");
    assert_eq!((get.start_line, get.end_line), (9, 12));
    assert_eq!(
        calls_from(&result, "fiatRampApi::getFiatRamps"),
        ["fetchRamps"]
    );
    assert_eq!(calls_from(&result, "fiatRampApi::placeOrder"), ["encode"]);
    assert_eq!(calls_from(&result, "fiatRampApi::listPages"), ["pageOf"]);
    // One without spans its builder call, which is walked under it.
    let check = node(&result, "fiatRampApi::checkConnection");
    assert_eq!((check.start_line, check.end_line), (20, 22));
    assert_eq!(
        check.signature.as_deref(),
        Some("build.mutation({\n      queryFn: makeCheckFn('/check'),\n    })")
    );
    assert_eq!(
        calls_from(&result, "fiatRampApi::checkConnection"),
        ["build.mutation", "makeCheckFn"]
    );
    // The rest of the initializer stays the api's, the endpoints' calls not.
    assert_eq!(
        calls_from(&result, "fiatRampApi"),
        [
            "build.infiniteQuery",
            "build.mutation",
            "build.query",
            "createApi",
            "fetchBaseQuery"
        ]
    );
}

#[test]
fn rtk_endpoints_through_a_method_and_injected() {
    let result = extract(
        "dashApi.ts",
        "import { baseApi } from './baseApi';

export const dashApi = createApi({
  endpoints(builder) {
    return {
      getDashboards: builder.query({
        query() {
          return route('/dashboards');
        },
      }),
    };
  },
});

export const extendedApi = baseApi.injectEndpoints({
  endpoints: (build) => {
    return {
      getPosts: build.query({ query: () => postsRoute() }),
    };
  },
  overrideExisting: false,
});
",
    );
    assert_eq!(
        functions(&result),
        ["dashApi::getDashboards", "extendedApi::getPosts"]
    );
    let dashboards = node(&result, "dashApi::getDashboards");
    assert_eq!((dashboards.start_line, dashboards.end_line), (7, 9));
    assert_eq!(calls_from(&result, "dashApi::getDashboards"), ["route"]);
    assert_eq!(calls_from(&result, "extendedApi::getPosts"), ["postsRoute"]);
    assert_eq!(
        calls_from(&result, "extendedApi"),
        ["baseApi.injectEndpoints", "build.query"]
    );
}

#[test]
fn an_rtk_endpoint_takes_its_query_fn_then_its_query_then_its_first_function() {
    let result = extract(
        "api.ts",
        "export const api = createApi({
  endpoints: (build) => ({
    both: build.query({
      transformResponse: (r) => shape(r),
      query: () => viaQuery(),
      queryFn: () => viaQueryFn(),
    }),
    queryOnly: build.query({
      transformResponse: (r) => shape(r),
      query: () => viaQuery(),
    }),
    neither: build.query({
      transformResponse: (r) => shape(r),
      onQueryStarted: () => started(),
    }),
  }),
});
",
    );
    assert_eq!(calls_from(&result, "api::both"), ["viaQueryFn"]);
    assert_eq!(calls_from(&result, "api::queryOnly"), ["viaQuery"]);
    assert_eq!(calls_from(&result, "api::neither"), ["shape"]);
    // The other members of an endpoint's config run as the api's.
    assert_eq!(
        calls_from(&result, "api"),
        [
            "build.query",
            "build.query",
            "build.query",
            "createApi",
            "shape",
            "shape",
            "started",
            "viaQuery",
        ]
    );
}

#[test]
fn only_builder_calls_under_a_plain_endpoints_key_are_endpoints() {
    let result = extract(
        "api.ts",
        "export const quoted = createApi({
  'endpoints': (build) => ({ getA: build.query({ query: () => a() }) }),
});
export const notBuilder = createApi({
  endpoints: (build) => ({
    getB: makeEndpoint({ query: () => b() }),
    getC: build.other({ query: () => c() }),
    'getD': build.query({ query: () => d() }),
    [dynamicName]: build.query({ query: () => e() }),
  }),
});
export const notRtk = createStuff({
  endpoints: (build) => ({ getF: build.query({ query: () => f() }) }),
});
",
    );
    assert_eq!(functions(&result), ["notBuilder::getD"]);
}

#[test]
fn a_bare_endpoint_signature_counts_utf16_code_units() {
    // The call text's first 80 UTF-16 code units end with `汉字`, one unit
    // each; the emoji after them, two units, is cut off whole.
    let prefix = "build.query({ queryFn: make('";
    let filler = "x".repeat(78 - prefix.encode_utf16().count());
    let result = extract(
        "api.ts",
        &format!(
            "export const api = createApi({{
  endpoints: (build) => ({{
    plain: build.query({{ queryFn: make('{filler}汉字😀tail') }}),
  }}),
}});
"
        ),
    );
    let signature = node(&result, "api::plain").signature.clone().unwrap();
    assert_eq!(signature, format!("{prefix}{filler}汉字"));
    assert_eq!(signature.encode_utf16().count(), 80);
}

#[test]
fn a_bare_endpoint_signature_marks_a_split_surrogate_pair() {
    // 79 units before the emoji: its pair would be cut after its first unit.
    let prefix = "build.query({ queryFn: make('";
    let filler = "y".repeat(79 - prefix.encode_utf16().count());
    let result = extract(
        "api.ts",
        &format!(
            "export const api = createApi({{
  endpoints: (build) => ({{
    split: build.query({{ queryFn: make('{filler}😀') }}),
  }}),
}});
"
        ),
    );
    let signature = node(&result, "api::split").signature.clone().unwrap();
    assert_eq!(signature, format!("{prefix}{filler}\u{FFFD}"));
}

#[test]
fn rtk_hooks_are_the_generated_bindings_of_a_destructuring() {
    let result = extract(
        "api.ts",
        "export const {
  useGetPostsQuery,
  useLazyGetPostsQuery,
  useAddPostMutation,
  usePrefetch: usePrefetchPosts,
  useGetDraftsQuery: useDrafts,
  useRefetchQuery = fallback,
  useQuery,
  useMutation,
  useGetquery,
  notAHook,
} = api;
const { useGetUserQuery } = api;
const { useGetMoreQuery } = makeApi();
function local() {
  const { useGetLocalQuery } = api;
}
",
    );
    let hooks = result
        .nodes
        .iter()
        .filter(|node| node.signature.as_deref() == Some(RTK_GENERATED_HOOK_SIGNATURE))
        .map(|node| {
            (
                node.name.as_str(),
                node.kind,
                node.is_exported,
                node.start_line,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        hooks,
        [
            ("useGetPostsQuery", NodeKind::Function, true, 2),
            ("useLazyGetPostsQuery", NodeKind::Function, true, 3),
            ("useAddPostMutation", NodeKind::Function, true, 4),
            ("useGetUserQuery", NodeKind::Function, false, 13),
        ]
    );
    assert_eq!(
        node(&result, "useGetPostsQuery").qualified_name,
        "useGetPostsQuery"
    );
}

#[test]
fn pinia_options_collections_are_functions_of_the_store() {
    let result = extract(
        "authStore.ts",
        "import { defineStore } from 'pinia';
export const useAuthStore = defineStore({
  id: 'auth',
  state: () => ({ token: readToken() }),
  getters: {
    upperName: state => state.name.toUpperCase(),
  },
  actions: {
    async fetchMenu() { return loadMenu(); },
    setName(n: string) { this.name = n; },
  },
  hooks: { onInit() { warmUp(); } },
});
export const machine = createMachine({
  actions: { notify() { ping(); } },
});
export const useCartStore = defineStore('cart', {
  actions: {
    add: function (item: string) { this.items.push(item); },
  },
});
",
    );
    assert_eq!(
        functions(&result),
        [
            "useAuthStore::fetchMenu",
            "useAuthStore::setName",
            "useAuthStore::upperName",
            "useCartStore::add",
        ]
    );
    assert_eq!(calls_from(&result, "useAuthStore::fetchMenu"), ["loadMenu"]);
    assert_eq!(
        calls_from(&result, "useAuthStore::upperName"),
        ["state.name.toUpperCase"]
    );
    assert_eq!(
        calls_from(&result, "useCartStore::add"),
        ["this.items.push"]
    );
    // The store call and its state stay the store's, the members' calls not.
    assert_eq!(
        calls_from(&result, "useAuthStore"),
        ["defineStore", "readToken", "warmUp"]
    );
    // Only a store's call holds collections.
    assert_eq!(calls_from(&result, "machine"), ["createMachine", "ping"]);
    assert_eq!(calls_from(&result, "useCartStore"), ["defineStore"]);
}

#[test]
fn vuex_stores_built_by_a_call_or_a_construction() {
    let result = extract(
        "store.js",
        "import Vuex, { createStore, Store } from 'vuex';
export const store = createStore({
  mutations: { SET(state, v) { state.v = v; } },
  actions: { save({ commit }) { commit('SET', persist()); } },
  'getters': { quoted: (state) => state.v },
  modules: { other: { actions: { nested() {} } } },
});
const legacy = new Vuex.Store({
  actions: { load() { return fetchAll(); } },
});
const plain = new Store({
  getters: { [computedKey]: (state) => state.k, named: (state) => state.n },
});
",
    );
    assert_eq!(
        functions(&result),
        ["legacy::load", "plain::named", "store::SET", "store::save"]
    );
    assert_eq!(calls_from(&result, "store::save"), ["commit", "persist"]);
    assert_eq!(calls_from(&result, "legacy::load"), ["fetchAll"]);
    assert_eq!(calls_from(&result, "store"), ["createStore"]);
}

#[test]
fn a_store_files_collection_declaration_holds_its_functions() {
    let wrapped = "const actions = wrap(() => ({
  refresh() { return reload(); },
}));
export default { namespaced: true, actions };
";
    let result = extract("module.js", wrapped);
    assert_eq!(functions(&result), ["actions::refresh"]);
    assert_eq!(calls_from(&result, "actions::refresh"), ["reload"]);
    assert_eq!(calls_from(&result, "actions"), ["wrap"]);
    // Without a second store word the file is no store.
    let result = extract("module.js", &wrapped.replace("namespaced: true, ", ""));
    assert_eq!(functions(&result), Vec::<String>::new());
    assert_eq!(calls_from(&result, "actions"), ["reload", "wrap"]);
}

#[test]
fn a_store_file_reads_two_different_store_words() {
    let module = |words: &str| {
        format!(
            "// {words}
export default {{
  mutations: {{ SET(state) {{ state.v = 1; }} }},
}};
"
        )
    };
    // `mutations` is one word; a second must differ from it.
    assert_eq!(
        functions(&extract("m.js", &module(""))),
        Vec::<String>::new()
    );
    assert_eq!(
        functions(&extract("m.js", &module("mutations again"))),
        Vec::<String>::new()
    );
    assert_eq!(
        functions(&extract("m.js", &module("getterss mutationsX Vuex_"))),
        Vec::<String>::new()
    );
    assert_eq!(functions(&extract("m.js", &module("Vuex"))), ["SET"]);
    assert_eq!(
        functions(&extract("m.js", &module("$namespaced$"))),
        ["SET"]
    );
}

#[test]
fn a_vuex_modules_default_export_holds_its_collections_functions() {
    let source = "import { login as apiLogin } from '../../api/user';
export default {
  namespaced: true,
  state: () => ({ token: initialToken() }),
  mutations: {
    SET_TOKEN(state, t) { state.token = t; },
  },
  actions: {
    login({ commit }, info) {
      apiLogin(info);
      commit('SET_TOKEN', info.token);
    },
  },
  methods: { helper() { return assist(); } },
};
";
    let result = extract("store/modules/user.js", source);
    assert_eq!(functions(&result), ["SET_TOKEN", "login"]);
    let login = node(&result, "login");
    assert_eq!((login.start_line, login.end_line), (9, 12));
    assert_eq!(calls_from(&result, "login"), ["apiLogin", "commit"]);
    // The rest of the statement is walked as before: its calls are the file's.
    assert_eq!(
        calls_of(&result, "file:store/modules/user.js"),
        ["assist", "initialToken"]
    );
    // A file that reads like no store keeps the statement as it was.
    let result = extract(
        "config.js",
        &source
            .replace("namespaced: true,\n", "")
            .replace("mutations", "mutators"),
    );
    assert_eq!(functions(&result), Vec::<String>::new());
    assert_eq!(
        calls_of(&result, "file:config.js"),
        ["apiLogin", "assist", "commit", "initialToken"]
    );
}
