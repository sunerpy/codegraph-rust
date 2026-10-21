//! The marker under which a sync re-resolves an unchanged dependent file from
//! its stored references instead of re-extracting it (W5-09e).
//!
//! Resolution moves each reference it takes out of `unresolved_refs` into
//! `resolved_refs`, so the two tables together hold every reference of a
//! file's last extraction. That is what re-extracting the unchanged file gives
//! only while nothing else its extraction reads has moved, so the marker
//! records a digest of those inputs, a full index writes it, a sync reads it
//! before its first change, withdraws it at that change and puts it back only
//! when it was valid at the start and the inputs did not move.

use anyhow::Result;
use codegraph_core::node_id::hash_content;
use codegraph_extract::engine::ExtractOptions;
use codegraph_resolve::framework::FrameworkExtractionContext;
use codegraph_store::Store;
use codegraph_store::index_state::CURRENT_EXTRACTION_VERSION;

/// `project_metadata` key of the marker.
pub const VERSION_KEY: &str = "resolved_refs_version";

/// The marker for the extraction inputs `options`, `framework` and `store`'s
/// detected frameworks hold now: `1:` and a digest of everything that decides
/// what an unchanged file's extraction yields.
pub fn marker_value(
    store: &Store,
    options: &ExtractOptions,
    framework: &FrameworkExtractionContext,
) -> Result<String> {
    let frameworks = store.get_project_metadata(crate::control_inputs::FRAMEWORKS_KEY)?;
    let identity = serde_json::json!({
        "extensions": options
            .extensions
            .entries()
            .map(|(extension, language)| (extension.to_string(), format!("{language:?}")))
            .collect::<Vec<_>>(),
        "max_file_size": options.max_file_size,
        "godot_dsl": format!("{:?}", framework.godot_dsl()),
        "extraction_version": CURRENT_EXTRACTION_VERSION,
        "frameworks": frameworks,
    });
    Ok(format!("1:{}", hash_content(&identity.to_string())))
}

/// Record that every file's stored references are its last extraction's
/// under the current inputs: a full index, once its resolution is persisted.
pub fn write_marker(
    store: &Store,
    options: &ExtractOptions,
    framework: &FrameworkExtractionContext,
) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, &marker_value(store, options, framework)?)?;
    Ok(())
}

/// The marker's value when it holds for the current inputs, read before a
/// sync's first change; `None` otherwise.
pub(crate) fn valid_marker(
    store: &Store,
    options: &ExtractOptions,
    framework: &FrameworkExtractionContext,
) -> Result<Option<String>> {
    let expected = marker_value(store, options, framework)?;
    Ok(
        (store.get_project_metadata(VERSION_KEY)?.as_deref() == Some(expected.as_str()))
            .then_some(expected),
    )
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

/// Put the marker back after a fully persisted sync that found it valid
/// (`at_start`), only when the inputs it digests are what they were.
pub(crate) fn restore_marker(
    store: &Store,
    options: &ExtractOptions,
    framework: &FrameworkExtractionContext,
    at_start: Option<&str>,
) -> Result<()> {
    let Some(at_start) = at_start else {
        return Ok(());
    };
    if marker_value(store, options, framework)? == at_start {
        store.set_project_metadata(VERSION_KEY, at_start)?;
    }
    Ok(())
}
