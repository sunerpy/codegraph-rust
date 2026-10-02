//! `GET /api/source` — verbatim source for an INDEXED file, and the drift
//! verdict every other endpoint flags. Upstream `src/ui-server/api/source.ts`.
//!
//! The viewer only reads files the index knows about, through the one read
//! chokepoint ([`crate::security::resolve_project_file`]); the refusal runs
//! before the index lookup. A file that drifted since indexing is not sliced at
//! stored line numbers: by default it is omitted, and `ondrift=current` serves
//! its current bytes with a reason saying nothing the graph holds lines up.

use std::path::{Path, PathBuf};

use codegraph_core::types::FileRecord;
use serde_json::{Value, json};

use super::Ctx;
use crate::highlight::highlight_lines;
use crate::respond::{ApiError, ApiResult, Query};
use crate::security::resolve_project_file;

/// The largest file served as source (8 MB).
pub const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
/// Lines one request may return.
pub const MAX_SOURCE_LINES: usize = 4000;

/// Backslashes to forward slashes, a leading `./` dropped.
pub fn to_request_path(requested: &str) -> String {
    let posix = requested.replace('\\', "/");
    posix
        .strip_prefix("./")
        .map(str::to_string)
        .unwrap_or(posix)
}

/// The file record for a requested path, and the path the index stores it under
/// (a legacy native-separator record is found too).
pub fn find_indexed_file(
    ctx: &Ctx<'_>,
    requested: &str,
) -> ApiResult<Option<(FileRecord, String)>> {
    let posix = to_request_path(requested);
    if let Some(record) = ctx.store.file_by_path(&posix)? {
        return Ok(Some((record, posix)));
    }
    let native = posix.replace('/', std::path::MAIN_SEPARATOR_STR);
    if native != posix
        && let Some(record) = ctx.store.file_by_path(&native)?
    {
        return Ok(Some((record, native)));
    }
    Ok(None)
}

pub fn not_indexed_error(file: &str) -> ApiError {
    ApiError::not_found(
        format!("{file} is not in this CodeGraph index."),
        Some(
            "The viewer only reads files the index knows about. If the file is new, \
             it appears after the next sync; if it is excluded (gitignored, generated, \
             or too large to parse), it will not appear at all.",
        ),
    )
}

/// Refusal first, index lookup second.
pub fn resolve_requested_file(
    ctx: &Ctx<'_>,
    requested: &str,
) -> ApiResult<(FileRecord, String, PathBuf)> {
    let posix = to_request_path(requested);
    let absolute =
        resolve_project_file(ctx.project_root(), &posix).map_err(|r| ApiError::refused(r.0))?;
    let Some((record, stored)) = find_indexed_file(ctx, &posix)? else {
        return Err(not_indexed_error(&posix));
    };
    Ok((record, stored, absolute))
}

/// Split source the way the index counted it: `\n` rows, a trailing `\r` dropped
/// per row, and the empty string after a final newline not counted as a line.
pub fn split_lines(content: &str) -> Vec<String> {
    let mut lines: Vec<String> = content
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// The project's indexing size limit — what the index hashed a file against.
pub fn max_file_size(ctx: &Ctx<'_>) -> u64 {
    codegraph_core::config::Config::load_for_paths(None, &ctx.state.paths)
        .map(|c| c.indexing.max_file_size)
        .unwrap_or(codegraph_core::config::DEFAULT_MAX_FILE_SIZE)
}

/// sha256 of what the index hashes for `bytes`: the text within the limit, the
/// size stamp over it (#1910).
pub fn indexed_hash(size: u64, text: &str, max_bytes: u64) -> String {
    if size > max_bytes {
        codegraph_core::node_id::hash_content(&codegraph_core::source_file::oversize_stamp(size))
    } else {
        codegraph_core::node_id::hash_content(text)
    }
}

fn mtime_ms(meta: &std::fs::Metadata) -> Option<i64> {
    let modified = meta.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(since.as_millis() as i64)
}

fn read_lossy(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8(bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// Whether an indexed file changed on disk since it was indexed. Size plus
/// floored mtime first (the sync fast path); only a stat mismatch pays for a
/// hash. Any failure answers `false`: a wrong "stale" flag would put a warning
/// over correct source.
pub fn has_drifted_on_disk(
    ctx: &Ctx<'_>,
    stored_path: &str,
    record: &FileRecord,
    max_bytes: u64,
) -> bool {
    let Ok(absolute) = resolve_project_file(ctx.project_root(), stored_path) else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(&absolute) else {
        return false;
    };
    if meta.len() as i64 == record.size && mtime_ms(&meta) == Some(record.modified_at) {
        return false;
    }
    if meta.len() > max_bytes {
        return indexed_hash(meta.len(), "", max_bytes) != record.content_hash;
    }
    match read_lossy(&absolute) {
        Ok(text) => indexed_hash(meta.len(), &text, max_bytes) != record.content_hash,
        Err(_) => false,
    }
}

/// The drift verdict and the line count from one read (the whole-file view
/// needs both before it draws).
pub struct FileShape {
    pub drift: bool,
    pub total_lines: Option<usize>,
    pub reason: Option<String>,
}

pub fn read_file_shape(
    ctx: &Ctx<'_>,
    stored_path: &str,
    record: &FileRecord,
    max_bytes: u64,
) -> FileShape {
    let Ok(absolute) = resolve_project_file(ctx.project_root(), stored_path) else {
        return FileShape {
            drift: false,
            total_lines: None,
            reason: None,
        };
    };
    let result = (|| -> std::io::Result<FileShape> {
        let meta = std::fs::metadata(&absolute)?;
        if meta.len() > MAX_SOURCE_BYTES {
            return Ok(FileShape {
                drift: false,
                total_lines: None,
                reason: Some("The file is too large to read here.".to_string()),
            });
        }
        let text = read_lossy(&absolute)?;
        let drift = indexed_hash(meta.len(), &text, max_bytes) != record.content_hash;
        Ok(FileShape {
            drift,
            total_lines: Some(split_lines(&text).len()),
            reason: drift.then(|| {
                "This file changed on disk after the last index sync, so the line \
                 numbers the graph holds no longer match it."
                    .to_string()
            }),
        })
    })();
    result.unwrap_or(FileShape {
        drift: true,
        total_lines: None,
        reason: Some("The file is in the index but could not be read from disk.".to_string()),
    })
}

/// The whole text of an indexed file, or `None` for anything unreadable
/// (drift deliberately not checked: the current bytes answer "does anything in
/// this file write this name" better than the indexed ones).
pub fn read_indexed_file_text(ctx: &Ctx<'_>, requested: &str, max_bytes: u64) -> Option<String> {
    let (_, stored) = find_indexed_file(ctx, requested).ok()??;
    let absolute = resolve_project_file(ctx.project_root(), &stored).ok()?;
    let meta = std::fs::metadata(&absolute).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    read_lossy(&absolute).ok()
}

const DRIFT_CURRENT_REASON: &str = "This file changed on disk after the last index sync. These are its current \
     lines; the indexed line ranges — symbol bodies, call sites, ports — no longer \
     match them. The next sync picks it up.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDrift {
    Omit,
    Current,
}

pub fn parse_on_drift(query: &Query) -> ApiResult<OnDrift> {
    match query.get("ondrift") {
        None | Some("") | Some("omit") => Ok(OnDrift::Omit),
        Some("current") => Ok(OnDrift::Current),
        Some(raw) => Err(ApiError::bad_request(format!(
            "Parameter \"ondrift\" must be \"omit\" or \"current\" (got \"{raw}\")."
        ))
        .with_hint("Omit it to leave a drifted file unsliced; \"current\" serves the bytes on disk instead.")),
    }
}

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let requested = query.text("file")?;
    let (record, stored, absolute) = resolve_requested_file(ctx, requested)?;
    let from = query.int("from", 1, 5_000_000, Some(1))? as usize;
    let to = query.int("to", 1, 5_000_000, Some(0))? as usize;
    if to != 0 && to < from {
        return Err(ApiError::bad_request(format!(
            "Parameter \"to\" ({to}) must not be before \"from\" ({from})."
        )));
    }
    let on_drift = parse_on_drift(query)?;
    let file = stored.replace('\\', "/");
    let mut base = json!({
        "file": file,
        "language": record.language.as_str(),
        "drift": false,
        "showing": "indexed",
        "contentHash": record.content_hash,
        "indexedAt": record.indexed_at,
        "generated": record.generated,
        "totalLines": Value::Null,
    });

    let Ok(meta) = std::fs::metadata(&absolute) else {
        base["drift"] = json!(true);
        base["showing"] = json!("none");
        base["reason"] = json!("The file is in the index but no longer on disk.");
        return Ok(base);
    };
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(ApiError::bad_request(format!(
            "{file} is {} MB — too large to serve as source.",
            (meta.len() as f64 / 1024.0 / 1024.0).round()
        )));
    }
    let content = read_lossy(&absolute)
        .map_err(|e| ApiError::internal(format!("Could not read {file}: {e}")))?;
    let hash = indexed_hash(meta.len(), &content, max_file_size(ctx));
    let drift = hash != record.content_hash;
    if drift && on_drift == OnDrift::Omit {
        base["drift"] = json!(true);
        base["showing"] = json!("none");
        base["reason"] = json!(
            "This file changed on disk after the last index sync, so the indexed line \
             ranges no longer reliably match. Source is omitted rather than risk showing \
             a different symbol's code; it returns after the next sync."
        );
        return Ok(base);
    }

    let all = split_lines(&content);
    if from > all.len() {
        if !drift {
            return Err(ApiError::bad_request(format!(
                "Parameter \"from\" ({from}) is past the end of {file}, which has {} lines.",
                all.len()
            )));
        }
        base["drift"] = json!(true);
        base["showing"] = json!("current");
        base["totalLines"] = json!(all.len());
        base["from"] = json!(from);
        base["to"] = json!(from - 1);
        base["lines"] = json!([]);
        base["truncated"] = json!(false);
        base["reason"] = json!(DRIFT_CURRENT_REASON);
        return Ok(base);
    }
    let start = from;
    let requested_end = if to == 0 {
        all.len()
    } else {
        to.min(all.len())
    };
    let end = requested_end.min(start + MAX_SOURCE_LINES - 1);
    let slice: Vec<String> = all[start - 1..end].to_vec();
    let highlight = highlight_lines(
        &ctx.state.caches.highlight,
        &slice,
        Some(record.language),
        Some(&format!("{hash}:{start}:{end}")),
    );
    base["drift"] = json!(drift);
    base["showing"] = json!(if drift { "current" } else { "indexed" });
    if drift {
        base["reason"] = json!(DRIFT_CURRENT_REASON);
    }
    base["totalLines"] = json!(all.len());
    base["from"] = json!(start);
    base["to"] = json!(end);
    base["lines"] = json!(slice);
    base["truncated"] = json!(end < requested_end);
    base["highlight"] = serde_json::to_value(&highlight).unwrap_or(Value::Null);
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_like_the_index_counts_them() {
        assert_eq!(split_lines("a\r\nb\n"), vec!["a", "b"]);
        assert_eq!(split_lines("a\n\n"), vec!["a", ""]);
        assert_eq!(split_lines(""), vec![""]);
        assert_eq!(split_lines("a"), vec!["a"]);
    }

    #[test]
    fn request_paths_are_posix_without_a_dot_prefix() {
        assert_eq!(to_request_path(".\\src\\a.rs"), "src/a.rs");
        assert_eq!(to_request_path("./src/a.rs"), "src/a.rs");
    }
}
