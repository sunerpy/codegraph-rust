//! `GET/POST/DELETE /api/trails` — saved trails, the reader's own tours through
//! the graph, and the only thing `codegraph ui` ever writes. Upstream
//! `src/ui-server/api/trails.ts` + `trail-store.ts`.
//!
//! Trails live in `<index root>/ui/trails/<slug>.json` — inside the index
//! directory `IndexPaths` resolved, so a `CODEGRAPH_DIR` override moves them
//! with the index — and every path goes through the same containment check a
//! source read does. A write is a temp file beside the target, then a rename.
//!
//! **A trail must survive a re-index.** A node id carries its start line, so a
//! hop is stored as what it IS (qualified name, kind, file) with the id only as
//! a fast path, and every hop is re-resolved on the way out: `ok`, `moved`,
//! `ambiguous` (best guess, labelled as one) or `missing`. Nothing is silently
//! dropped and nothing is silently guessed.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_core::types::Node;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::wire::{locale_compare, to_node_ref};
use crate::respond::{ApiError, ApiErrorCode, ApiResult};
use crate::security::resolve_project_file;

/// The only `version` this build writes.
pub const TRAIL_FORMAT_VERSION: i64 = 1;
/// Trail files read from the directory before the list stops looking.
pub const MAX_TRAILS: usize = 200;
/// Hops one trail may carry. Past this it is a history, not a tour.
pub const MAX_TRAIL_HOPS: usize = 64;
pub const MAX_TRAIL_NAME: usize = 120;
pub const MAX_TRAIL_NOTE: usize = 600;
/// Bytes a trail file may be before it is skipped as not-ours.
pub const MAX_TRAIL_FILE_BYTES: u64 = 64 * 1024;
const MAX_SLUG: usize = 60;
const MAX_AUTHOR: usize = 120;

/// `encodeURIComponent`'s unreserved set.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// One hop, described by what it is rather than by the id it had.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoredHop {
    pub dir: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    /// Project-relative, forward slashes.
    pub file: String,
    pub line: i64,
    /// The node id at save time. A fast path, never the identity.
    pub id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoredTrail {
    pub version: serde_json::Number,
    /// Slug, and the file's basename.
    pub id: String,
    pub name: String,
    pub note: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    pub hops: Vec<StoredHop>,
}

/* ------------------------------------------------------------------ text -- */

/// JavaScript's `.length` for a string.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `text.slice(0, max)` in UTF-16 units, never splitting a character.
fn slice_utf16(text: &str, max: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in text.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        out.push(c);
    }
    out
}

/// `new Date().toISOString()`: `2026-10-02T09:15:42.123Z`.
pub fn iso_timestamp(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let millis = since.subsec_millis();
    let secs = since.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/* ------------------------------------------------------------------ paths -- */

/// Whether a string is a trail id we would have written: a lowercase slug, so
/// `<id>.json` is a filename rather than a path expression.
pub fn is_trail_id(value: &str) -> bool {
    let b = value.as_bytes();
    let ok = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty() && b.len() <= 80 && ok(b[0]) && b[1..].iter().all(|&c| ok(c) || c == b'-')
}

/// `Read a file with these lines` → `read-a-file-with-these-lines`; a name with
/// no ASCII letters or digits at all slugs to `trail`.
pub fn slugify(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut slug = String::new();
    let mut gap = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(c);
        } else {
            gap = true;
        }
    }
    let cut: String = slug.chars().take(MAX_SLUG).collect();
    let cut = cut.trim_end_matches('-');
    if cut.is_empty() {
        "trail".to_string()
    } else {
        cut.to_string()
    }
}

/// The trails directory relative to the project, forward slashes: the index
/// root's own name plus `ui/trails`.
pub fn trails_relative_dir(ctx: &Ctx<'_>) -> String {
    let index_root = ctx.state.paths.current_root();
    let relative = index_root
        .strip_prefix(ctx.project_root())
        .ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .filter(|rel| !rel.is_empty())
        .unwrap_or_else(|| ".codegraph".to_string());
    format!("{relative}/ui/trails")
}

fn refused(reason: crate::security::PathRefusal) -> ApiError {
    ApiError::refused(reason.0)
}

fn trails_directory(ctx: &Ctx<'_>) -> ApiResult<PathBuf> {
    resolve_project_file(ctx.project_root(), &trails_relative_dir(ctx)).map_err(refused)
}

fn trail_path(ctx: &Ctx<'_>, id: &str) -> ApiResult<PathBuf> {
    if !is_trail_id(id) {
        return Err(ApiError::bad_request(format!("\"{id}\" is not a saved trail id."))
            .with_hint("Trail ids are the lowercase slug in the file name, e.g. \"how-a-request-is-served\"."));
    }
    resolve_project_file(
        ctx.project_root(),
        &format!("{}/{id}.json", trails_relative_dir(ctx)),
    )
    .map_err(refused)
}

/* ------------------------------------------------------------------- read -- */

/// Parse a file into a trail, or `None` if it is not one. Everything is
/// re-validated: a hand-edited or dropped-in file that half-parsed would draw a
/// row with holes in it.
pub fn parse_trail(id: &str, text: &str) -> Option<StoredTrail> {
    let raw: Value = serde_json::from_str(text).ok()?;
    let value = raw.as_object()?;
    let name = value.get("name")?.as_str()?;
    if name.trim().is_empty() {
        return None;
    }
    let entries = value.get("hops")?.as_array()?;
    if entries.is_empty() {
        return None;
    }
    let text_field = |hop: &serde_json::Map<String, Value>, key: &str| -> String {
        hop.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut hops = Vec::new();
    for entry in entries.iter().take(MAX_TRAIL_HOPS) {
        let hop = entry.as_object()?;
        let qualified_name = text_field(hop, "qualifiedName");
        let name = text_field(hop, "name");
        if qualified_name.is_empty() && name.is_empty() {
            return None;
        }
        let dir = match hop.get("dir").and_then(Value::as_str) {
            Some("up") => "up",
            Some("down") => "down",
            _ => "start",
        };
        let line = hop
            .get("line")
            .and_then(Value::as_f64)
            .filter(|l| *l > 0.0)
            .map(|l| l.floor() as i64)
            .unwrap_or(0);
        hops.push(StoredHop {
            dir: dir.to_string(),
            name: if name.is_empty() {
                qualified_name.clone()
            } else {
                name.clone()
            },
            qualified_name: if qualified_name.is_empty() {
                name
            } else {
                qualified_name
            },
            kind: text_field(hop, "kind"),
            file: text_field(hop, "file"),
            line,
            id: text_field(hop, "id"),
        });
    }
    let created = value
        .get("createdAt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(StoredTrail {
        version: value
            .get("version")
            .and_then(Value::as_number)
            .cloned()
            .unwrap_or_else(|| TRAIL_FORMAT_VERSION.into()),
        // The FILE's name wins over any `id` inside it: the basename is what the
        // delete route addresses.
        id: id.to_string(),
        name: slice_utf16(name, MAX_TRAIL_NAME),
        note: value
            .get("note")
            .and_then(Value::as_str)
            .map(|n| slice_utf16(n, MAX_TRAIL_NOTE))
            .unwrap_or_default(),
        author: value
            .get("author")
            .and_then(Value::as_str)
            .map(|a| slice_utf16(a, MAX_AUTHOR))
            .unwrap_or_default(),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| created.clone()),
        created_at: created,
        hops,
    })
}

fn read_trail_file(path: &Path, id: &str) -> Option<StoredTrail> {
    let meta = std::fs::metadata(path).ok()?;
    // Too big to be a trail: skipped, never read.
    if !meta.is_file() || meta.len() > MAX_TRAIL_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    parse_trail(id, &String::from_utf8_lossy(&bytes))
}

/// Every trail in the project, newest save first, plus how many files in the
/// directory were not readable trails. A missing directory is the ordinary
/// state of a project nobody saved a trail in.
pub fn list_stored_trails(ctx: &Ctx<'_>) -> ApiResult<(Vec<StoredTrail>, usize)> {
    let dir = trails_directory(ctx)?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok((Vec::new(), 0));
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut trails = Vec::new();
    let mut skipped = 0;
    for name in names {
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        if trails.len() >= MAX_TRAILS {
            break;
        }
        if !is_trail_id(id) {
            skipped += 1;
            continue;
        }
        match read_trail_file(&dir.join(&name), id) {
            Some(trail) => trails.push(trail),
            None => skipped += 1,
        }
    }
    trails.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| locale_compare(&a.name, &b.name))
    });
    Ok((trails, skipped))
}

/* ------------------------------------------------------------------ write -- */

fn write_failure(err: &std::io::Error) -> ApiError {
    let detail = err.to_string();
    if matches!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    ) {
        return ApiError::refused(format!("Saved trails could not be written: {detail}"));
    }
    ApiError::internal(format!("Saved trails could not be written: {detail}"))
}

fn write_failure_with_hint(ctx: &Ctx<'_>, err: &std::io::Error) -> ApiError {
    let failure = write_failure(err);
    if failure.code == ApiErrorCode::Refused {
        return failure.with_hint(format!(
            "The viewer writes only to {} inside this project. Check that it is writable.",
            trails_relative_dir(ctx)
        ));
    }
    failure
}

/// Write a trail atomically: a temp file beside the target (named with the pid,
/// so two viewers on one project cannot share one), then a rename.
pub fn write_stored_trail(ctx: &Ctx<'_>, trail: &StoredTrail) -> ApiResult<()> {
    let dir = trails_directory(ctx)?;
    std::fs::create_dir_all(&dir).map_err(|e| write_failure_with_hint(ctx, &e))?;
    let target = trail_path(ctx, &trail.id)?;
    let mut temp = target.clone().into_os_string();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(trail).map_err(|e| ApiError::internal(e.to_string()))?
    );
    let written = std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, &target));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(write_failure_with_hint(ctx, &err));
    }
    Ok(())
}

/// Remove a trail. `false` when there was nothing there.
pub fn delete_stored_trail(ctx: &Ctx<'_>, id: &str) -> ApiResult<bool> {
    match std::fs::remove_file(trail_path(ctx, id)?) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(write_failure_with_hint(ctx, &err)),
    }
}

/// An id nothing in `taken` uses, preferring the plain slug. A save under an
/// existing NAME replaces that trail, so `taken` holds only the ids of trails
/// carrying a different name.
pub fn unique_trail_id(base: &str, taken: &HashSet<String>) -> ApiResult<String> {
    if !taken.contains(base) {
        return Ok(base.to_string());
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err(ApiError::bad_request(format!(
        "Too many saved trails are already named like \"{base}\"."
    )))
}

/* -------------------------------------------------------------- resolution -- */

fn hop_matches(node: &Node, hop: &StoredHop) -> bool {
    if !hop.kind.is_empty() && node.kind.as_str() != hop.kind {
        return false;
    }
    node.qualified_name == hop.qualified_name || node.name == hop.name
}

/// Re-resolve one saved hop against the index as it is now: the recorded id
/// first (verified against the name — a recycled id must not put a stranger
/// in the middle of an explanation), then the qualified name.
pub fn resolve_hop(ctx: &Ctx<'_>, hop: &StoredHop) -> ApiResult<Value> {
    let mut out = json!({
        "dir": hop.dir,
        "name": hop.name,
        "qualifiedName": hop.qualified_name,
        "kind": hop.kind,
        "savedFile": hop.file,
        "savedLine": hop.line,
    });
    let mut settle = |status: &str, node: Option<&Node>, note: Option<String>| {
        out["status"] = json!(status);
        out["id"] = json!(node.map(|n| n.id.clone()));
        out["file"] = json!(node.map(|n| n.file_path.clone()));
        out["line"] = json!(node.map(|n| n.start_line));
        out["note"] = json!(note);
    };
    if !hop.id.is_empty()
        && let Some(node) = ctx.store.node_by_id(&hop.id)?
        && hop_matches(&node, hop)
    {
        settle("ok", Some(&node), None);
        return Ok(out);
    }
    let mut candidates: Vec<Node> = ctx
        .store
        .nodes_by_qualified_name(&hop.qualified_name)?
        .into_iter()
        .filter(|n| hop.kind.is_empty() || n.kind.as_str() == hop.kind)
        .collect();
    candidates.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.id.cmp(&b.id))
    });
    if candidates.is_empty() {
        settle(
            "missing",
            None,
            Some(
                "no longer in the index — moved or renamed since this trail was saved".to_string(),
            ),
        );
        return Ok(out);
    }
    let same_file: Vec<&Node> = candidates
        .iter()
        .filter(|n| n.file_path == hop.file)
        .collect();
    if same_file.len() == 1 {
        settle("ok", Some(same_file[0]), None);
        return Ok(out);
    }
    if candidates.len() == 1 {
        let node = &candidates[0];
        let from = if hop.file.is_empty() {
            "an unrecorded file"
        } else {
            hop.file.as_str()
        };
        settle(
            "moved",
            Some(node),
            Some(format!("moved from {from} to {}", node.file_path)),
        );
        return Ok(out);
    }
    // Several symbols carry the name and none is where it used to be: the best
    // guess is offered, labelled as one.
    let pick = same_file.first().copied().unwrap_or(&candidates[0]);
    settle(
        "ambiguous",
        Some(pick),
        Some(format!(
            "{} symbols now carry this name — showing the one in {}",
            candidates.len(),
            pick.file_path
        )),
    );
    Ok(out)
}

/// The longest CONSECUTIVE run of resolved hops, as the viewer's `t` param.
/// Never stitched across a hole; the run's first hop is always `start`.
pub fn encode_resolved_run(hops: &[Value]) -> Value {
    let resolved = |hop: &Value| hop.get("id").and_then(Value::as_str).is_some();
    let (mut best_start, mut best_len) = (0, 0);
    let mut start: Option<usize> = None;
    for i in 0..=hops.len() {
        if i < hops.len() && resolved(&hops[i]) {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take()
            && i - s > best_len
        {
            best_start = s;
            best_len = i - s;
        }
    }
    if best_len == 0 {
        return json!({ "encoded": Value::Null, "openFrom": 0, "openCount": 0, "openId": Value::Null });
    }
    let run = &hops[best_start..best_start + best_len];
    let encoded: Vec<String> = run
        .iter()
        .enumerate()
        .map(|(index, hop)| {
            let dir = if index == 0 {
                "s"
            } else {
                match hop.get("dir").and_then(Value::as_str) {
                    Some("up") => "u",
                    Some("down") => "d",
                    _ => "s",
                }
            };
            let id = hop.get("id").and_then(Value::as_str).unwrap_or("");
            format!("{dir}{}", utf8_percent_encode(id, URI_COMPONENT))
        })
        .collect();
    json!({
        "encoded": encoded.join(","),
        "openFrom": best_start + 1,
        "openCount": best_len,
        "openId": run.last().and_then(|h| h.get("id")).cloned().unwrap_or(Value::Null),
    })
}

fn resolve_trail(ctx: &Ctx<'_>, stored: &StoredTrail) -> ApiResult<Value> {
    let hops: Vec<Value> = stored
        .hops
        .iter()
        .map(|hop| resolve_hop(ctx, hop))
        .collect::<ApiResult<_>>()?;
    let run = encode_resolved_run(&hops);
    let resolved = hops
        .iter()
        .filter(|h| h.get("id").and_then(Value::as_str).is_some())
        .count();
    let intact = hops
        .iter()
        .all(|h| h.get("status").and_then(Value::as_str) == Some("ok"));
    let mut out = json!({
        "id": stored.id,
        "name": stored.name,
        "note": stored.note,
        "author": stored.author,
        "createdAt": stored.created_at,
        "updatedAt": stored.updated_at,
        "hops": hops,
        "resolved": resolved,
        "intact": intact,
    });
    if let (Some(target), Some(fields)) = (out.as_object_mut(), run.as_object()) {
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(out)
}

/// `GET /api/trails`: every saved trail, re-resolved.
pub fn build_list(ctx: &Ctx<'_>) -> ApiResult<Value> {
    let (trails, skipped) = list_stored_trails(ctx)?;
    let bounded = trails.len() >= MAX_TRAILS;
    let resolved: Vec<Value> = trails
        .iter()
        .map(|stored| resolve_trail(ctx, stored))
        .collect::<ApiResult<_>>()?;
    Ok(json!({
        "trails": resolved,
        "readOnly": ctx.state.read_only,
        "readOnlyReason": ctx.state.read_only_reason,
        "directory": trails_relative_dir(ctx),
        "skipped": skipped,
        "bounded": bounded,
    }))
}

fn read_only_refusal(ctx: &Ctx<'_>) -> ApiError {
    ApiError::refused(ctx.state.read_only_reason.clone().unwrap_or_else(|| {
        "This viewer is running read-only, so trails cannot be saved.".to_string()
    }))
    .with_hint(format!(
        "Restart without --read-only to let the viewer write trails into {}.",
        trails_relative_dir(ctx)
    ))
}

/// A POST body as JSON, with upstream's refusals for an empty or malformed one.
pub fn parse_json_body(body: &[u8]) -> ApiResult<Value> {
    if body.is_empty() {
        return Err(ApiError::bad_request("That request needs a JSON body."));
    }
    serde_json::from_str(&String::from_utf8_lossy(body))
        .map_err(|_| ApiError::bad_request("That request body is not valid JSON."))
}

struct SaveRequest {
    name: String,
    note: String,
    hops: Vec<(String, Option<String>)>,
}

fn parse_save_request(body: &Value) -> ApiResult<SaveRequest> {
    let Some(value) = body.as_object() else {
        return Err(ApiError::bad_request(
            "A trail is saved from a JSON object: { name, hops }.",
        ));
    };
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    if name.is_empty() {
        return Err(ApiError::bad_request("A saved trail needs a name."));
    }
    if utf16_len(&name) > MAX_TRAIL_NAME {
        return Err(ApiError::bad_request(format!(
            "That name is too long (max {MAX_TRAIL_NAME} characters)."
        )));
    }
    let note = value
        .get("note")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if utf16_len(&note) > MAX_TRAIL_NOTE {
        return Err(ApiError::bad_request(format!(
            "That note is too long (max {MAX_TRAIL_NOTE} characters)."
        )));
    }
    let entries = match value.get("hops").and_then(Value::as_array) {
        Some(entries) if !entries.is_empty() => entries,
        _ => {
            return Err(ApiError::bad_request(
                "A saved trail needs at least one hop.",
            ));
        }
    };
    if entries.len() > MAX_TRAIL_HOPS {
        return Err(ApiError::bad_request(format!(
            "A saved trail can hold at most {MAX_TRAIL_HOPS} hops."
        )));
    }
    let mut hops = Vec::new();
    for entry in entries {
        let Some(hop) = entry.as_object() else {
            return Err(ApiError::bad_request("Each hop is { dir, id }."));
        };
        let id = match hop.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return Err(ApiError::bad_request("Each hop needs an id.")),
        };
        hops.push((
            id,
            hop.get("dir").and_then(Value::as_str).map(str::to_string),
        ));
    }
    Ok(SaveRequest { name, note, hops })
}

/// `POST /api/trails`. The client sends ids and directions only; every hop's
/// name, kind, file and line is read from the index here, so a trail is always
/// a claim the index can re-check. A save under an existing name replaces it,
/// keeping its `createdAt`.
pub fn save(ctx: &Ctx<'_>, body: &[u8]) -> ApiResult<Value> {
    let value = parse_json_body(body)?;
    if ctx.state.read_only {
        return Err(read_only_refusal(ctx));
    }
    let request = parse_save_request(&value)?;
    let mut hops: Vec<StoredHop> = Vec::new();
    for (index, (id, dir)) in request.hops.iter().enumerate() {
        let Some(node) = ctx.store.node_by_id(id)? else {
            return Err(ApiError::bad_request(format!("Hop {} is not in the index: {id}", index + 1)).with_hint(
                "Trails are saved from symbols the index holds. Reload the page and walk the trail again.",
            ));
        };
        let wire = to_node_ref(&node);
        hops.push(StoredHop {
            dir: match dir.as_deref() {
                Some("up") => "up",
                Some("down") => "down",
                _ => "start",
            }
            .to_string(),
            name: wire.name,
            qualified_name: wire.qualified_name,
            kind: wire.kind.to_string(),
            file: wire.file,
            line: wire.line,
            id: wire.id,
        });
    }
    // The first hop is where the walk began, whatever the client called it.
    if let Some(first) = hops.first_mut() {
        first.dir = "start".to_string();
    }

    let (existing, _) = list_stored_trails(ctx)?;
    let same_name = existing.iter().find(|t| t.name == request.name);
    let taken: HashSet<String> = existing
        .iter()
        .filter(|t| t.name != request.name)
        .map(|t| t.id.clone())
        .collect();
    let id = match same_name {
        Some(trail) => trail.id.clone(),
        None => unique_trail_id(&slugify(&request.name), &taken)?,
    };
    let now = iso_timestamp(SystemTime::now());
    let created_at = same_name
        .map(|t| t.created_at.clone())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| now.clone());
    write_stored_trail(
        ctx,
        &StoredTrail {
            version: TRAIL_FORMAT_VERSION.into(),
            id: id.clone(),
            name: request.name,
            note: request.note,
            author: trail_author(ctx),
            created_at,
            updated_at: now,
            hops,
        },
    )?;
    let mut out = build_list(ctx)?;
    out["saved"] = json!(id);
    out["replaced"] = json!(same_name.is_some());
    Ok(out)
}

/// `DELETE /api/trails/<id>`.
pub fn delete(ctx: &Ctx<'_>, id: &str) -> ApiResult<Value> {
    if ctx.state.read_only {
        return Err(read_only_refusal(ctx));
    }
    if !delete_stored_trail(ctx, id)? {
        return Err(ApiError::not_found(
            format!("There is no saved trail called \"{id}\"."),
            None,
        ));
    }
    let mut out = build_list(ctx)?;
    out["deleted"] = json!(id);
    Ok(out)
}

/* ---------------------------------------------------------------- author -- */

/// Who to record as the author: git's `user.name` in the project, else the OS
/// user. Read once per server — `git config` is a subprocess — and never sent
/// anywhere: it goes into a file inside the user's own index directory.
fn trail_author(ctx: &Ctx<'_>) -> String {
    ctx.state
        .trail_author
        .get_or_init(|| {
            git_user_name(ctx.project_root())
                .or_else(os_user_name)
                .unwrap_or_default()
        })
        .clone()
}

/// The variables that point git at a repository other than the one `cwd` is
/// in (a hook exports them); the project's own configuration should answer.
const GIT_LOCATION_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
];

fn git_user_name(project_root: &Path) -> Option<String> {
    let mut command = Command::new("git");
    command
        .args(["config", "user.name"])
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for var in GIT_LOCATION_VARS {
        command.env_remove(var);
    }
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let name = out.trim();
    (!name.is_empty()).then(|| slice_utf16(name, MAX_AUTHOR))
}

fn os_user_name() -> Option<String> {
    ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .map(|name| slice_utf16(&name, MAX_AUTHOR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trail_ids_are_slugs() {
        assert!(is_trail_id("how-a-request-is-served"));
        assert!(is_trail_id("a"));
        assert!(!is_trail_id("-a"));
        assert!(!is_trail_id("A"));
        assert!(!is_trail_id("a.json"));
        assert!(!is_trail_id("../x"));
        assert!(!is_trail_id(""));
        assert!(!is_trail_id(&"a".repeat(81)));
        assert!(is_trail_id(&"a".repeat(80)));
    }

    #[test]
    fn names_slugify_like_upstream() {
        assert_eq!(
            slugify("Read a file with these lines"),
            "read-a-file-with-these-lines"
        );
        assert_eq!(slugify("  --Hello, World!--  "), "hello-world");
        assert_eq!(slugify("请求如何被处理"), "trail");
        assert_eq!(slugify(&format!("{}-tail", "a".repeat(59))), "a".repeat(59));
        assert_eq!(slugify("x1 2y"), "x1-2y");
    }

    #[test]
    fn a_trail_file_is_revalidated_on_read() {
        let text = r#"{"version":1,"id":"other","name":"Tour","hops":[{"dir":"down","name":"run","line":12.7},{"qualifiedName":"a::b"}],"createdAt":"2026-01-01T00:00:00.000Z"}"#;
        let trail = parse_trail("tour", text).expect("a trail");
        assert_eq!(trail.id, "tour");
        assert_eq!(trail.updated_at, "2026-01-01T00:00:00.000Z");
        assert_eq!(trail.hops[0].dir, "down");
        assert_eq!(trail.hops[0].qualified_name, "run");
        assert_eq!(trail.hops[0].line, 12);
        assert_eq!(trail.hops[1].dir, "start");
        assert_eq!(trail.hops[1].name, "a::b");
        assert!(parse_trail("x", r#"{"name":"","hops":[{"name":"a"}]}"#).is_none());
        assert!(parse_trail("x", r#"{"name":"n","hops":[]}"#).is_none());
        assert!(parse_trail("x", r#"{"name":"n","hops":[{"kind":"function"}]}"#).is_none());
        assert!(parse_trail("x", "not json").is_none());
    }

    #[test]
    fn unique_ids_step_aside_for_other_names_only() {
        let taken: HashSet<String> = ["tour".to_string(), "tour-2".to_string()]
            .into_iter()
            .collect();
        assert_eq!(unique_trail_id("tour", &taken).unwrap(), "tour-3");
        assert_eq!(unique_trail_id("fresh", &taken).unwrap(), "fresh");
    }

    #[test]
    fn the_run_is_consecutive_and_starts_at_start() {
        let hop = |id: Option<&str>, dir: &str| json!({ "id": id, "dir": dir });
        let hops = vec![
            hop(Some("a"), "start"),
            hop(None, "down"),
            hop(Some("b c"), "up"),
            hop(Some("d/e"), "down"),
        ];
        let run = encode_resolved_run(&hops);
        assert_eq!(run["encoded"], "sb%20c,dd%2Fe");
        assert_eq!(run["openFrom"], 3);
        assert_eq!(run["openCount"], 2);
        assert_eq!(run["openId"], "d/e");
        let none = encode_resolved_run(&[hop(None, "start")]);
        assert_eq!(none["encoded"], Value::Null);
    }

    #[test]
    fn timestamps_match_to_iso_string() {
        let at = UNIX_EPOCH + Duration::from_millis(1_790_000_000_123);
        assert_eq!(iso_timestamp(at), "2026-09-21T14:13:20.123Z");
        assert_eq!(iso_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(iso_timestamp(leap), "2000-02-29T00:00:00.000Z");
    }
}
