//! How a synthesized hop reads — what bridged the call, and where it was
//! wired. A port of upstream `src/graph/synthesized-hop.ts` (`cd97bb37`,
//! #2428), which holds the wording `b635dd46`'s explore and node trail wrote
//! inline.
//!
//! Static parsing cannot see a handler registered in one place and run in
//! another, so a synthesis pass writes the `calls` edge across it
//! (`provenance: "heuristic"`, `metadata.synthesizedBy`, and usually a
//! `metadata.registeredAt` wiring site). Whatever prints that hop has to say
//! what bridged it, or the hop reads as a direct call, or as the wrong
//! mechanism. `codegraph_explore`'s Flow and the `codegraph_node` trail tag it
//! `dynamic: callback via `subscribe` @src/store.ts:4`; the wording lives here
//! once, and each surface renders it.

use codegraph_core::types::Edge;
use serde_json::Value;

/// What bridged a synthesized hop, and where it was wired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesizedHop {
    /// What bridged the hop: "callback via `subscribe`", "interface → impl",
    /// "redux thunk".
    pub summary: String,
    /// What a call-paths rendering adds after the summary — the queue a job
    /// runs on, a socket message's direction, what an HTTP hop is; empty when
    /// nothing.
    pub detail: String,
    /// Where the hop was wired up (`file:line`), when the synthesizer recorded
    /// it.
    pub registered_at: Option<String>,
}

impl SynthesizedHop {
    /// The tag explore's Flow and the node trail print:
    /// `dynamic: <summary> @<registeredAt>`.
    pub fn tag(&self) -> String {
        match &self.registered_at {
            Some(at) => format!("dynamic: {} @{at}", self.summary),
            None => format!("dynamic: {}", self.summary),
        }
    }
}

/// Describe a synthesized (dynamic-dispatch) edge; `None` for an ordinary
/// static one. A synthesizer not named below still reads as dynamic dispatch,
/// by its own name and with its wiring site — never as a bare call or a
/// borrowed label.
pub fn describe_synthesized_hop(edge: &Edge) -> Option<SynthesizedHop> {
    if edge.provenance.as_deref() != Some("heuristic") {
        return None;
    }
    let metadata = edge.metadata.as_ref()?.as_object()?;
    let by = metadata.get("synthesizedBy")?.as_str()?;
    let field = |key: &str| metadata.get(key).filter(|value| truthy(value));
    let registered_at = metadata
        .get("registeredAt")
        .and_then(Value::as_str)
        .map(str::to_string);
    let hop = |summary: String, detail: String| SynthesizedHop {
        summary,
        detail,
        registered_at: registered_at.clone(),
    };
    let quoted = |key: &str, otherwise: &str| match field(key) {
        Some(value) => format!("`{}`", js_string(value)),
        None => otherwise.to_string(),
    };
    let plain = |key: &str| field(key).map(js_string).unwrap_or_default();
    Some(match by {
        "callback" => hop(
            format!("callback via {}", quoted("via", "a registrar")),
            String::new(),
        ),
        "http-client" => {
            // `${String(m.method ?? 'GET')} ${String(m.href ?? '')}`.trim()
            let nullish = |key: &str| metadata.get(key).filter(|value| !value.is_null());
            let method = nullish("method").map_or_else(|| "GET".to_string(), js_string);
            let href = nullish("href").map(js_string).unwrap_or_default();
            let request = format!("{method} {href}");
            hop(
                format!("HTTP {}", request.trim()),
                " — the client's call onto its own route".to_string(),
            )
        }
        "queue-job" => hop(
            format!("queue job {}", quoted("event", "a job")),
            field("queue")
                .map(|queue| format!(" on `{}`", js_string(queue)))
                .unwrap_or_default(),
        ),
        "event-bus" => {
            let what = if metadata.get("channel").and_then(Value::as_str) == Some("socket") {
                "socket message"
            } else {
                "bus event"
            };
            let direction = match metadata.get("tier").and_then(Value::as_str) {
                Some("client→server") => " → server",
                Some("server→client") => " → client",
                _ => "",
            };
            hop(
                format!("{what} {}", quoted("event", "an event")),
                direction.to_string(),
            )
        }
        "event-emitter" => hop(
            format!("event {}", quoted("event", "an event")),
            String::new(),
        ),
        "react-render" => hop("React re-render via setState".to_string(), String::new()),
        "jsx-render" => hop(
            format!(
                "renders {}",
                field("via").map_or_else(
                    || "a child component".to_string(),
                    |via| format!("<{}>", js_string(via))
                )
            ),
            String::new(),
        ),
        "vue-handler" => hop(
            format!(
                "Vue {} handler",
                field("event").map_or_else(
                    || "a template event".to_string(),
                    |event| format!("@{}", js_string(event))
                )
            ),
            String::new(),
        ),
        // Go: the implementing struct gets the method from a type it embeds.
        "interface-impl" => match metadata.get("promotedInto").and_then(Value::as_str) {
            Some(promoted) => hop(
                format!("interface → method promoted into {promoted}"),
                String::new(),
            ),
            None => hop("interface → impl".to_string(), String::new()),
        },
        "closure-collection" => hop(
            format!("runs {} handlers", quoted("field", "a collection")),
            String::new(),
        ),
        "fn-pointer-dispatch" => hop(format!("fn-pointer {}", plain("via")), String::new()),
        "goframe-route" => hop(format!("GoFrame route {}", plain("route")), String::new()),
        // redux-thunk, cpp-override, gin-middleware-chain, flutter-build, …
        other => hop(other.replace('-', " "), String::new()),
    })
}

/// JavaScript truthiness of a JSON value, as the upstream `m.via ? … : …`
/// tests read it.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `String(value)` for a JSON value.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e21 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::EdgeKind;
    use serde_json::json;

    const AT: &str = "src/wire.ts:7";

    fn edge(metadata: Value) -> Edge {
        let mut metadata = metadata;
        metadata
            .as_object_mut()
            .unwrap()
            .insert("registeredAt".to_string(), json!(AT));
        Edge {
            id: None,
            source: "a".to_string(),
            target: "b".to_string(),
            kind: EdgeKind::Calls,
            metadata: Some(metadata),
            line: None,
            col: None,
            provenance: Some("heuristic".to_string()),
        }
    }

    /// Upstream `__tests__/synthesized-hop.test.ts`, `describeSynthesizedHop`.
    #[test]
    fn each_synthesizer_reads_by_its_own_words() {
        for (metadata, summary, detail) in [
            (
                json!({"synthesizedBy": "interface-impl", "via": "handle"}),
                "interface → impl",
                "",
            ),
            (
                json!({"synthesizedBy": "interface-impl", "via": "Report", "promotedInto": "scrapeLoop"}),
                "interface → method promoted into scrapeLoop",
                "",
            ),
            (
                json!({"synthesizedBy": "closure-collection", "field": "validators"}),
                "runs `validators` handlers",
                "",
            ),
            (
                json!({"synthesizedBy": "cpp-override", "via": "Run"}),
                "cpp override",
                "",
            ),
            (
                json!({"synthesizedBy": "redux-thunk", "via": "fetchUser"}),
                "redux thunk",
                "",
            ),
            (
                json!({"synthesizedBy": "gin-middleware-chain", "via": "Auth"}),
                "gin middleware chain",
                "",
            ),
            (
                json!({"synthesizedBy": "event-bus", "channel": "event", "event": "user.created"}),
                "bus event `user.created`",
                "",
            ),
            (
                json!({"synthesizedBy": "queue-job", "event": "send-email", "queue": "emails"}),
                "queue job `send-email`",
                " on `emails`",
            ),
            (
                json!({"synthesizedBy": "event-bus", "channel": "socket", "event": "chat", "tier": "client→server"}),
                "socket message `chat`",
                " → server",
            ),
            (
                json!({"synthesizedBy": "http-client", "method": "POST", "href": "/api/users"}),
                "HTTP POST /api/users",
                " — the client's call onto its own route",
            ),
        ] {
            assert_eq!(
                describe_synthesized_hop(&edge(metadata.clone())),
                Some(SynthesizedHop {
                    summary: summary.to_string(),
                    detail: detail.to_string(),
                    registered_at: Some(AT.to_string()),
                }),
                "{metadata}"
            );
        }
    }

    /// The `b635dd46` wording for the kinds the upstream table leaves out.
    #[test]
    fn the_other_synthesizers_keep_the_explore_wording() {
        for (metadata, summary) in [
            (
                json!({"synthesizedBy": "callback", "via": "subscribe"}),
                "callback via `subscribe`",
            ),
            (
                json!({"synthesizedBy": "callback"}),
                "callback via a registrar",
            ),
            (json!({"synthesizedBy": "http-client"}), "HTTP GET"),
            (json!({"synthesizedBy": "queue-job"}), "queue job a job"),
            (json!({"synthesizedBy": "event-emitter"}), "event an event"),
            (
                json!({"synthesizedBy": "event-emitter", "event": "ready"}),
                "event `ready`",
            ),
            (
                json!({"synthesizedBy": "react-render"}),
                "React re-render via setState",
            ),
            (
                json!({"synthesizedBy": "jsx-render", "via": "Row"}),
                "renders <Row>",
            ),
            (
                json!({"synthesizedBy": "jsx-render"}),
                "renders a child component",
            ),
            (
                json!({"synthesizedBy": "vue-handler", "event": "click"}),
                "Vue @click handler",
            ),
            (
                json!({"synthesizedBy": "vue-handler"}),
                "Vue a template event handler",
            ),
            (
                json!({"synthesizedBy": "fn-pointer-dispatch", "via": "ops->open"}),
                "fn-pointer ops->open",
            ),
            (
                json!({"synthesizedBy": "fn-pointer-dispatch"}),
                "fn-pointer ",
            ),
            (
                json!({"synthesizedBy": "goframe-route", "route": "/user"}),
                "GoFrame route /user",
            ),
        ] {
            let hop = describe_synthesized_hop(&edge(metadata.clone())).expect("a hop");
            assert_eq!(hop.summary, summary, "{metadata}");
        }
        let hop =
            describe_synthesized_hop(&edge(json!({"synthesizedBy": "callback", "via": "on"})))
                .unwrap();
        assert_eq!(hop.tag(), "dynamic: callback via `on` @src/wire.ts:7");
        let mut unwired = edge(json!({"synthesizedBy": "react-render"}));
        unwired
            .metadata
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("registeredAt");
        assert_eq!(
            describe_synthesized_hop(&unwired).unwrap().tag(),
            "dynamic: React re-render via setState"
        );
    }

    #[test]
    fn a_static_edge_or_one_no_synthesizer_made_is_none() {
        let mut static_edge = edge(json!({"synthesizedBy": "callback", "via": "on"}));
        static_edge.provenance = Some("tree-sitter".to_string());
        assert_eq!(describe_synthesized_hop(&static_edge), None);
        static_edge.provenance = None;
        assert_eq!(describe_synthesized_hop(&static_edge), None);
        assert_eq!(
            describe_synthesized_hop(&edge(json!({"confidence": 0.5}))),
            None
        );
        let mut bare = edge(json!({}));
        bare.metadata = None;
        assert_eq!(describe_synthesized_hop(&bare), None);
    }
}
