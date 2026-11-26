//! The Screens picture (`GET /api/screens`) against real indexes: the
//! Screens halves of upstream's router tests at `e8ddc4e2`, over their own
//! fixtures (`fixtures/mod.rs`), then this port's own cases — a total order
//! that does not depend on how the index was built, a mention not read from
//! a file that changed, and the two answers that stay `routed: false`.

mod fixtures;
mod support;

use serde_json::Value;
use support::Project;

async fn screens_of(project: &Project) -> Value {
    project.viewer().json("/api/screens").await
}

fn items(value: &Value) -> &Vec<Value> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {value}"))
}

/// The id of the screen at `path`.
fn at<'v>(screens: &'v Value, path: &str) -> &'v str {
    items(&screens["screens"])
        .iter()
        .find(|screen| screen["path"] == path)
        .unwrap_or_else(|| panic!("no screen {path} in {}", screens["screens"]))["id"]
        .as_str()
        .unwrap()
}

/// The path of the screen `id`, or the origin's name.
fn name_of(screens: &Value, id: &str) -> String {
    if let Some(screen) = items(&screens["screens"])
        .iter()
        .find(|screen| screen["id"] == id)
    {
        return screen["path"].as_str().unwrap().to_string();
    }
    items(&screens["origins"])
        .iter()
        .find(|origin| origin["id"] == id)
        .map(|origin| origin["node"]["name"].as_str().unwrap().to_string())
        .unwrap_or_else(|| panic!("no screen or origin {id}"))
}

/// The link from the screen at `from` to the screen at `to`.
fn link<'v>(screens: &'v Value, from: &str, to: &str) -> Option<&'v Value> {
    let (from, to) = (at(screens, from), at(screens, to));
    items(&screens["links"])
        .iter()
        .find(|link| link["from"] == from && link["to"] == to)
}

fn linked(screens: &Value, from: &str, to: &str) -> bool {
    link(screens, from, to).is_some()
}

fn via(link: &Value) -> Vec<&str> {
    items(&link["via"])
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect()
}

fn paths(screens: &Value) -> Vec<String> {
    let mut paths: Vec<String> = items(&screens["screens"])
        .iter()
        .map(|screen| screen["path"].as_str().unwrap().to_string())
        .collect();
    paths.sort();
    paths
}

// ------------------------------------------------------------- Expo Router --

/// Upstream `expo-router.test.ts:432-600`, its Screens assertions: the tap on
/// `ItemCard` is attributed back to the Home screen through the render hop,
/// with the chain and its condition; navigation no screen reaches is an
/// origin, not dropped.
#[tokio::test]
async fn expo_router_attributes_a_tap_to_the_screen_it_is_on_with_the_chain_and_its_condition() {
    let project = Project::indexed(fixtures::EXPO_APP);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    assert_eq!(screens["entry"], at(&screens, "/"));
    let tap = link(&screens, "/", "/object-detail").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(via(tap), ["ItemCard", "handlePress", "openObjectDetail"]);
    assert_eq!(tap["when"], "props.collected");
    assert_eq!(tap["sites"][0]["href"], "/object-detail?detectionItem=${…}");
    let mut from_origins: Vec<String> = items(&screens["links"])
        .iter()
        .filter(|link| link["fromOrigin"] == true)
        .map(|link| name_of(&screens, link["to"].as_str().unwrap()))
        .collect();
    from_origins.sort();
    assert_eq!(from_origins, ["/", "/item/[id]", "/welcome"]);
    let origins: Vec<&str> = items(&screens["origins"])
        .iter()
        .map(|origin| origin["node"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(origins, ["openItem", "resolvePostLoginRoute"]);
    assert_eq!(screens["dropped"], 0);
}

/// Upstream `expo-router.test.ts:636-754`: a push written in a component the
/// graph reaches backwards only through the native bridge belongs to the
/// screen whose file holds it, not to the screen that started the round
/// trip, and no Swift guard comes home with it.
///
/// Upstream's chain runs `MemoizedCaptureComponent > CaptureComponent`.
/// This port's graph takes the `memo(CaptureComponent)` reference from the
/// file node, not from `MemoizedCaptureComponent` (a graph difference this
/// endpoint does not change), so the walk reaches no component and the
/// file's own screen takes the push with an empty `via`.
#[tokio::test]
async fn expo_router_files_a_push_on_the_screen_whose_file_holds_it_not_across_the_bridge() {
    let project = Project::indexed(fixtures::EXPO_BRIDGE);
    let screens = screens_of(&project).await;
    let review = at(&screens, "/capture/review");
    let into: Vec<&Value> = items(&screens["links"])
        .iter()
        .filter(|link| link["to"] == review)
        .collect();
    let from: Vec<String> = into
        .iter()
        .map(|link| name_of(&screens, link["from"].as_str().unwrap()))
        .collect();
    assert_eq!(from, ["/capture"], "{screens}");
    assert!(via(into[0]).is_empty(), "{screens}");
    assert!(
        items(&into[0]["via"])
            .iter()
            .all(|node| node["language"] != "swift")
    );
    assert_eq!(into[0]["when"], "!isRetakeBatchActive");
    assert_eq!(into[0]["sites"][0]["file"], "src/app/capture/index.tsx");
    assert!(!linked(&screens, "/capture/review", "/capture/review"));
}

// ------------------------------------------------------------ React Router --

/// Upstream `react-router.test.ts:299-310`.
#[tokio::test]
async fn react_router_lands_on_the_screens_tab_as_transitions_between_screens() {
    let project = Project::indexed(fixtures::RR_APP);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    let push = link(&screens, "/payment", "/placeorder").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(push["sites"][0]["href"], "/placeorder");
    assert_eq!(push["sites"][0]["method"], "push");
    // The submit handler is the hop between the screen and the push.
    assert_eq!(via(push), ["submitHandler"]);
    assert!(linked(&screens, "/shipping", "/payment"));
    assert!(linked(&screens, "/product/:id", "/cart/:id?"));
}

/// Upstream `react-router.test.ts:474-481`: `<Link to={redirect ? … :
/// '/register'}>` is markup, so its site is a `link`, not a helper's return.
#[tokio::test]
async fn react_router_a_link_written_under_a_condition_reads_as_a_link() {
    let project = Project::indexed(fixtures::RR_SHAPES);
    let screens = screens_of(&project).await;
    let to_register = link(&screens, "/login", "/register").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(to_register["sites"][0]["method"], "link");
}

/// Upstream `react-router.test.ts:483-496`: a component rendered at several
/// addresses gives its navigation to every one of them.
#[tokio::test]
async fn react_router_a_component_at_several_addresses_navigates_from_every_one() {
    let project = Project::indexed(fixtures::RR_SHAPES);
    let screens = screens_of(&project).await;
    for from in ["/", "/search/:keyword", "/page/:pageNumber"] {
        assert!(linked(&screens, from, "/product/:id"), "{from}: {screens}");
    }
    for screen in items(&screens["screens"]) {
        let path = screen["path"].as_str().unwrap();
        if path == "/product/:id" || path == "/register" {
            continue;
        }
        assert!(
            items(&screens["links"])
                .iter()
                .any(|link| link["from"] == screen["id"]),
            "{path} leads nowhere: {screens}"
        );
    }
    assert_eq!(screens["dropped"], 0);
}

/// Upstream `react-router.test.ts:1045-1049`.
#[tokio::test]
async fn react_router_a_route_table_spread_into_route_lands_on_the_screens_tab() {
    let project = Project::indexed(fixtures::RR_ASPNET);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    assert_eq!(
        paths(&screens),
        [
            "/",
            "/admin/users",
            "/authentication/profile",
            "/counter",
            "/fetch-data"
        ]
    );
}

/// Upstream `react-router.test.ts:1132-1146`, its Screens half: a layout's
/// links are drawn on every screen inside it.
#[tokio::test]
async fn react_router_draws_a_layouts_links_on_every_screen_inside_it() {
    let project = Project::indexed(fixtures::RR_USE_ROUTES);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    for from in ["/", "/login"] {
        assert!(
            linked(&screens, from, "/app/dashboard"),
            "{from}: {screens}"
        );
    }
    assert!(linked(&screens, "/login", "/app/account"));
}

/// Upstream `react-router.test.ts:1561-1577`: a layout's links are on every
/// page it wraps and only there; a guard's redirect is on the page it guards.
#[tokio::test]
async fn react_router_draws_a_layouts_links_on_every_screen_inside_it_and_only_there() {
    let project = Project::indexed(fixtures::RR_JSX_LAYOUTS);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    for from in [
        "/",
        "/about",
        "/login",
        "/dashboard",
        "/dashboard/stats",
        "/account",
        "/settings",
    ] {
        assert!(linked(&screens, from, "/about"), "{from}: {screens}");
    }
    assert!(linked(&screens, "/dashboard/stats", "/dashboard"));
    assert!(linked(&screens, "/account", "/login"));
    assert!(!linked(&screens, "/about", "/login"));
    assert!(!linked(&screens, "/admin/users", "/about"));
    assert!(linked(&screens, "/admin", "/admin/users"));
}

/// Upstream `react-router.test.ts:1676-1684`: the header's link is drawn from
/// every screen, a guard's redirect from the screens it guards.
#[tokio::test]
async fn react_router_draws_the_headers_link_from_every_screen_and_a_guards_redirect_from_its_screens()
 {
    let project = Project::indexed(fixtures::RR_PROSHOP_V2);
    let screens = screens_of(&project).await;
    for from in ["/", "/login", "/shipping", "/admin/orderlist"] {
        assert!(linked(&screens, from, "/cart"), "{from}: {screens}");
    }
    assert!(linked(&screens, "/shipping", "/login"));
    assert!(linked(&screens, "/admin/orderlist", "/login"));
    assert!(!linked(&screens, "/cart", "/login"));
}

// ----------------------------------------------------------------- Next.js --

/// Upstream `nextjs.test.ts:255-277`: the entry page, its links, the form's
/// push attributed back to its page with the condition, and the middleware's
/// redirect as an origin.
#[tokio::test]
async fn nextjs_lands_on_the_screens_tab_with_the_entry_page_its_links_and_the_forms_push() {
    let project = Project::indexed(fixtures::NEXT_APP);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    let home = at(&screens, "/");
    assert_eq!(screens["entry"], home);
    let home_screen = items(&screens["screens"])
        .iter()
        .find(|screen| screen["id"] == home)
        .unwrap();
    assert_eq!(home_screen["component"]["name"], "Home");
    let to_users = link(&screens, "/", "/users").unwrap_or_else(|| panic!("{screens}"));
    assert!(via(to_users).is_empty());
    assert_eq!(to_users["synthesized"], true);
    // Markup, not a return value: the site keeps its own verb.
    assert_eq!(to_users["sites"][0]["href"], "/users");
    assert_eq!(to_users["sites"][0]["method"], "link");
    let push = link(&screens, "/users", "/users/:id").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(via(push), ["NewUserForm", "handleSubmit"]);
    assert_eq!(push["when"], "user.ok");
    assert_eq!(push["sites"][0]["href"], "/users/${…}");
    assert_eq!(push["sites"][0]["method"], "push");
    assert!(
        items(&screens["origins"])
            .iter()
            .any(|origin| origin["node"]["name"] == "middleware"),
        "{screens}"
    );
    assert_eq!(screens["dropped"], 0);
}

/// Upstream `nextjs.test.ts:279-285`: an endpoint is not a screen.
#[tokio::test]
async fn nextjs_an_endpoint_is_not_a_screen() {
    let project = Project::indexed(fixtures::NEXT_APP);
    let screens = screens_of(&project).await;
    assert_eq!(paths(&screens), ["/", "/login", "/users", "/users/:id"]);
}

// ----------------------------------------------------------------- Angular --

/// Upstream `angular-router.test.ts:380-397`: a child component's navigation
/// is on its screen, a layout's on each screen inside it.
#[tokio::test]
async fn angular_draws_a_child_components_navigation_on_its_screen_and_a_layouts_on_each_screen_inside_it()
 {
    let files = fixtures::angular_app();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let project = Project::indexed(&refs);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    let mut links: Vec<String> = items(&screens["links"])
        .iter()
        .map(|link| {
            let names = via(link);
            let through = if names.is_empty() {
                String::new()
            } else {
                format!(" via {}", names.join(">"))
            };
            format!(
                "{} -> {}{through}",
                name_of(&screens, link["from"].as_str().unwrap()),
                name_of(&screens, link["to"].as_str().unwrap())
            )
        })
        .collect();
    links.sort();
    assert_eq!(
        links,
        [
            "/account -> /account/access/:id",
            "/account -> /login",
            "/account/access/:id -> /account/access/:id",
            "/account/access/:id -> /login",
            "/home -> /about",
            "/home -> /account/access/:id",
            "/home -> /account/access/:id via CardComponent>save",
            "/home -> /login",
            "/home -> /login via open",
            "/login -> /home",
            "/login -> /home via done",
        ]
    );
}

// --------------------------------------------- SvelteKit, TanStack, Vue Router --

/// Upstream `sveltekit-router.test.ts:207-219`.
#[tokio::test]
async fn sveltekit_lands_on_the_screens_tab_as_transitions_between_screens() {
    let project = Project::indexed(fixtures::SVELTEKIT_APP);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    // One screen per address — a layout does not double them.
    assert_eq!(
        items(&screens["screens"])
            .iter()
            .filter(|screen| screen["path"] == "/")
            .count(),
        1
    );
    assert!(linked(&screens, "/settings", "/login"), "{screens}");
    let publish =
        link(&screens, "/editor", "/article/:slug").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(publish["sites"][0]["href"], "/article/${…}");
    assert_eq!(publish["sites"][0]["method"], "goto");
    assert!(linked(&screens, "/article/:slug", "/editor"));
    assert_eq!(screens["dropped"], 0);
}

/// Upstream `tanstack-router.test.ts:341-354`.
#[tokio::test]
async fn tanstack_lands_on_the_screens_tab_as_transitions_between_screens() {
    let project = Project::indexed(fixtures::TANSTACK_APP);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    let posts = items(&screens["screens"])
        .iter()
        .find(|screen| screen["path"] == "/posts")
        .unwrap();
    assert_eq!(posts["component"]["name"], "PostsIndexComponent");
    let to_post = link(&screens, "/", "/posts/:postId").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(to_post["sites"][0]["href"], "/posts/:postId");
    let sign_in = link(&screens, "/login", "/dashboard").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(via(sign_in), ["submit"]);
    assert_eq!(sign_in["when"], "ok");
    assert_eq!(screens["dropped"], 0);
}

/// Upstream `vue-router.test.ts:291-303`.
#[tokio::test]
async fn vue_router_lands_on_the_screens_tab_as_transitions_between_screens() {
    let project = Project::indexed(fixtures::VUE_APP);
    let screens = screens_of(&project).await;
    assert_eq!(screens["routed"], true, "{screens}");
    assert_eq!(
        paths(&screens),
        [
            "/",
            "/login",
            "/profile/:username",
            "/register",
            "/settings"
        ]
    );
    let login = items(&screens["screens"])
        .iter()
        .find(|screen| screen["path"] == "/login")
        .unwrap();
    assert_eq!(login["component"]["name"], "Login");
    let to_profile =
        link(&screens, "/settings", "/profile/:username").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(via(to_profile), ["save"]);
    assert_eq!(to_profile["sites"][0]["href"], "profile");
    assert_eq!(to_profile["sites"][0]["method"], "push");
    assert!(linked(&screens, "/login", "/register"));
    assert_eq!(screens["dropped"], 0);
}

// ----------------------------------------------------------- This port's own --

/// A screen whose component renders two children that each lead to the same
/// navigation: two paths of one length, and which one the picture draws
/// must not depend on the order the index returns its rows in.
const TWO_PATHS: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\",\"expo-router\":\"4\",\"react\":\"18\"}}",
    ),
    (
        "src/app/_layout.tsx",
        "export default function Layout() { return null }\n",
    ),
    (
        "src/app/index.tsx",
        "import { Card } from '../components/card'\nimport { Panel } from '../components/panel'\nexport default function Home() {\n  return <><Card /><Panel /></>\n}\n",
    ),
    (
        "src/app/detail.tsx",
        "export default function Detail() { return null }\n",
    ),
    (
        "src/components/card.tsx",
        "import { openDetail } from '../services/nav'\nexport function Card(props: { ready: boolean }) {\n  if (props.ready) openDetail()\n  return null\n}\n",
    ),
    (
        "src/components/panel.tsx",
        "import { openDetail } from '../services/nav'\nexport function Panel(props: { open: boolean }) {\n  if (props.open) openDetail()\n  return null\n}\n",
    ),
    (
        "src/services/nav.ts",
        "import { router } from 'expo-router'\nexport function openDetail() {\n  router.push('/detail')\n}\nexport function openHome() {\n  router.push('/')\n}\n",
    ),
];

/// The payload without what changes from one request to the next.
fn stable(mut screens: Value) -> String {
    screens.as_object_mut().unwrap().remove("timing");
    screens["index"]
        .as_object_mut()
        .unwrap()
        .remove("lastIndexedAt");
    serde_json::to_string_pretty(&screens).unwrap()
}

/// The same files indexed afresh, and indexed then edited and put back,
/// draw the same picture byte for byte: every list the payload holds, and
/// every choice between two equal paths, follows a total order, never the
/// order the store returns its rows in.
#[tokio::test]
async fn the_picture_does_not_depend_on_how_the_index_was_built() {
    let fresh = Project::indexed(TWO_PATHS);
    let synced = Project::indexed(TWO_PATHS);
    let card = TWO_PATHS
        .iter()
        .find(|(path, _)| *path == "src/components/card.tsx")
        .unwrap()
        .1;
    synced.write(
        "src/components/card.tsx",
        &card.replace("return null", "return 0"),
    );
    synced.index();
    synced.write("src/components/card.tsx", card);
    synced.index();
    let fresh = screens_of(&fresh).await;
    let synced = screens_of(&synced).await;
    assert_eq!(fresh["routed"], true, "{fresh}");
    assert!(linked(&fresh, "/", "/detail"), "{fresh}");
    assert_eq!(stable(fresh), stable(synced));
}

/// A navigation inside a value nothing calls — the arrow a wrapper is handed
/// at a file's top level — starts from the screens whose components mention
/// the value in a file that imports it. A mention is read from that file's
/// source, and not when the file changed since it was indexed.
const MENTIONED: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\",\"expo-router\":\"4\",\"react\":\"18\"}}",
    ),
    (
        "src/app/_layout.tsx",
        "export default function Layout() { return null }\n",
    ),
    (
        "src/app/index.tsx",
        "import { useAction } from '../services/form'\nimport { signIn } from '../services/actions'\nexport default function Home() {\n  const submit = useAction(signIn)\n  return submit\n}\n",
    ),
    (
        "src/app/dashboard.tsx",
        "export default function Dashboard() { return null }\n",
    ),
    (
        "src/services/form.ts",
        "export function useAction(action: () => void) { return action }\nexport function wrap(run: () => void) { return run }\n",
    ),
    (
        "src/services/actions.ts",
        "import { router } from 'expo-router'\nimport { wrap } from './form'\nexport const signIn = wrap(() => {\n  router.push('/dashboard')\n})\n",
    ),
];

#[tokio::test]
async fn a_mention_starts_a_navigation_and_is_not_read_from_a_file_that_changed() {
    let project = Project::indexed(MENTIONED);
    let screens = screens_of(&project).await;
    let sign_in = link(&screens, "/", "/dashboard").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(via(sign_in), ["signIn"]);
    // The importing file moved under the index: its lines are not read for
    // a mention, and the navigation starts from no screen.
    let home = MENTIONED
        .iter()
        .find(|(path, _)| *path == "src/app/index.tsx")
        .unwrap()
        .1;
    project.write("src/app/index.tsx", &format!("// moved\n{home}"));
    let screens = screens_of(&project).await;
    assert!(!linked(&screens, "/", "/dashboard"), "{screens}");
}

/// One component at two addresses whose conditional call leads to a
/// conditional push: two screens, one navigation.
const TWO_ADDRESSES: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"react\":\"^18\",\"react-router-dom\":\"^5\"}}",
    ),
    (
        "src/App.jsx",
        "import { BrowserRouter as Router, Route } from 'react-router-dom'\nimport HomeScreen from './screens/HomeScreen'\nimport DetailScreen from './screens/DetailScreen'\nconst App = () => (\n  <Router>\n    <Route path='/' component={HomeScreen} exact />\n    <Route path='/home' component={HomeScreen} exact />\n    <Route path='/detail' component={DetailScreen} />\n  </Router>\n)\nexport default App\n",
    ),
    (
        "src/screens/HomeScreen.jsx",
        "import React from 'react'\nimport { openDetail } from '../nav'\nconst HomeScreen = ({ history, ready, done }) => {\n  if (ready) openDetail(history, done)\n  return <div>Home</div>\n}\nexport default HomeScreen\n",
    ),
    (
        "src/screens/DetailScreen.jsx",
        "import React from 'react'\nconst DetailScreen = () => <div>Detail</div>\nexport default DetailScreen\n",
    ),
    (
        "src/nav.js",
        "export function openDetail(history, done) {\n  if (done) history.push('/detail')\n}\n",
    ),
];

/// Each screen a navigation starts from reads the chain's conditions and the
/// site's own once, on its link and on its site. Upstream reuses one site
/// object for all of a navigation's screens and rewrites its `when` for
/// each, so the second screen read `ready && ready && done` and every link
/// showed the last value.
#[tokio::test]
async fn each_screen_of_one_navigation_reads_its_own_site_conditions() {
    let project = Project::indexed(TWO_ADDRESSES);
    let screens = screens_of(&project).await;
    for from in ["/", "/home"] {
        let link = link(&screens, from, "/detail").unwrap_or_else(|| panic!("{from}: {screens}"));
        assert_eq!(link["when"], "ready && done", "{from}: {screens}");
        assert_eq!(
            link["sites"][0]["when"], "ready && done",
            "{from}: {screens}"
        );
    }
}

const EXPO_PACKAGE: (&str, &str) = (
    "package.json",
    "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\",\"expo-router\":\"4\",\"react\":\"18\"}}",
);

/// A component three screens render navigates once, from itself: the top
/// bar's link is one fact about the top bar, marked with how many screens
/// render it, not three facts about three screens.
#[tokio::test]
async fn a_component_three_screens_render_navigates_as_shared_chrome() {
    let screen = |name: &str| {
        format!(
            "import {{ TopBar }} from '../components/top-bar'\nexport default function {name}() {{\n  return <TopBar />\n}}\n"
        )
    };
    let (home, feed, profile) = (screen("Home"), screen("Feed"), screen("Profile"));
    let project = Project::indexed(&[
        EXPO_PACKAGE,
        (
            "src/app/_layout.tsx",
            "export default function Layout() { return null }\n",
        ),
        ("src/app/index.tsx", &home),
        ("src/app/feed.tsx", &feed),
        ("src/app/profile.tsx", &profile),
        (
            "src/app/settings.tsx",
            "export default function Settings() { return null }\n",
        ),
        (
            "src/components/top-bar.tsx",
            "import { router } from 'expo-router'\nexport function TopBar() {\n  router.push('/settings')\n  return null\n}\n",
        ),
    ]);
    let screens = screens_of(&project).await;
    let top_bar = items(&screens["origins"])
        .iter()
        .find(|origin| origin["node"]["name"] == "TopBar")
        .unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(top_bar["sharedBy"], 3, "{screens}");
    let settings = at(&screens, "/settings");
    assert!(
        items(&screens["links"])
            .iter()
            .any(|link| link["from"] == top_bar["id"]
                && link["to"] == settings
                && link["fromOrigin"] == true),
        "{screens}"
    );
    for from in ["/", "/feed", "/profile"] {
        assert!(!linked(&screens, from, "/settings"), "{from}: {screens}");
    }
}

/// `if (x) push(A) else push(A)`: the two arms of one condition together
/// are "always", so the link carries no condition.
#[tokio::test]
async fn both_arms_of_one_condition_leading_to_one_screen_are_unconditional() {
    let project = Project::indexed(&[
        EXPO_PACKAGE,
        (
            "src/app/index.tsx",
            "import { router } from 'expo-router'\nexport default function Home(props: { ok: boolean }) {\n  if (props.ok) router.push('/next')\n  else router.push('/next')\n  return null\n}\n",
        ),
        (
            "src/app/next.tsx",
            "export default function Next() { return null }\n",
        ),
    ]);
    let screens = screens_of(&project).await;
    let next = link(&screens, "/", "/next").unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(next["when"], "", "{screens}");
    let mut whens: Vec<&str> = items(&next["sites"])
        .iter()
        .map(|site| site["when"].as_str().unwrap())
        .collect();
    whens.sort();
    assert_eq!(whens, ["!props.ok", "props.ok"]);
}

/// An Angular template names its destination where it is written, in a
/// file of its own beside the component: its link keeps the template's
/// verb, never reading as a helper's return.
#[tokio::test]
async fn an_angular_template_link_keeps_its_own_verb() {
    let files = fixtures::angular_app();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let project = Project::indexed(&refs);
    let screens = screens_of(&project).await;
    let sign_in = items(&screens["links"])
        .iter()
        .find(|link| {
            link["from"] == at(&screens, "/home")
                && link["to"] == at(&screens, "/login")
                && via(link).is_empty()
        })
        .unwrap_or_else(|| panic!("{screens}"));
    assert_eq!(sign_in["sites"][0]["method"], "routerLink", "{sign_in}");
}

/// No screen route, or screen routes nothing navigates to: the graph holds
/// no screen navigation, and the payload says so.
#[tokio::test]
async fn without_a_screen_route_or_a_navigation_into_one_the_app_is_not_routed() {
    let none = Project::indexed(&[("src/a.ts", "export function a() { return 1 }\n")]);
    let screens = screens_of(&none).await;
    assert_eq!(screens["routed"], false);
    assert!(items(&screens["screens"]).is_empty());
    assert!(items(&screens["links"]).is_empty());

    let unlinked = Project::indexed(&[
        (
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\",\"expo-router\":\"4\",\"react\":\"18\"}}",
        ),
        (
            "src/app/index.tsx",
            "export default function Home() { return null }\n",
        ),
        (
            "src/app/about.tsx",
            "export default function About() { return null }\n",
        ),
    ]);
    let screens = screens_of(&unlinked).await;
    assert_eq!(screens["routed"], false, "{screens}");
    assert_eq!(screens["entry"], Value::Null);
    assert_eq!(screens["dropped"], 0);
}
