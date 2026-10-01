//! Reading a project source file by path, the way every indexing pass does
//! (upstream v1.6.1 `file-limits.ts` and `grammars.ts`, #1910): never more than
//! the size limit plus one byte, with a size stamp standing in for a file over
//! it, and an MPEG transport stream named `.ts` recognised as video rather than
//! TypeScript before anything parses it.

use std::borrow::Cow;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

/// MPEG transport stream: fixed 188-byte packets, each opening with 0x47.
const MPEG_TS_PACKET_SIZE: usize = 188;
const MPEG_TS_SYNC_BYTE: u8 = 0x47;
/// Consecutive packets whose sync byte must line up before a file counts as
/// video — 3 KB of head. A shorter stream is cheap to parse anyway.
const MPEG_TS_MIN_PACKETS: usize = 16;
/// How much of a file's head [`is_mpeg_transport_stream`] looks at.
const MPEG_TS_SNIFF_BYTES: usize = MPEG_TS_PACKET_SIZE * MPEG_TS_MIN_PACKETS;

/// A source file as an indexing pass sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceText {
    /// The file's text, within the limit. Bytes that are not UTF-8 become
    /// U+FFFD, as upstream's `toString('utf8')` decodes them, so a Latin-1 or
    /// binary file is indexed like any other instead of failing the pass.
    Text(String),
    /// A file over the limit, never read: its size in bytes stands in for it.
    Oversize(u64),
    /// An MPEG transport stream named `.ts`: video, not source.
    MpegTransportStream,
}

impl SourceText {
    /// What the index hashes for this file: its text within the limit, its
    /// size stamp over it, and nothing for a file that is not source.
    pub fn hash_input(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Text(text) => Some(Cow::Borrowed(text)),
            Self::Oversize(size) => Some(Cow::Owned(oversize_stamp(*size))),
            Self::MpegTransportStream => None,
        }
    }
}

/// What stands in for the content of a file over the size limit. Such a file
/// is never parsed, so the stamp is a function of its size alone: a same-size
/// rewrite is not a change (nothing is indexed from it), while crossing the
/// limit in either direction is.
pub fn oversize_stamp(size: u64) -> String {
    format!("codegraph:oversize:{size}")
}

/// Read the source file at `path`, named `relative` in the project, without
/// ever holding more than `max_bytes` plus one byte. The size is checked by
/// `stat` before the file is opened, again on the open handle, and once more
/// after a read that itself stops one byte past the limit, so a file growing
/// meanwhile is still stamped rather than decoded. Returns the metadata the
/// file's record is built from. A path that is not a regular file is an error.
pub fn read_source_file(
    path: &Path,
    relative: &str,
    max_bytes: u64,
) -> io::Result<(fs::Metadata, SourceText)> {
    let initial = regular_file_metadata(fs::metadata(path)?)?;
    if initial.len() > max_bytes {
        let size = initial.len();
        return Ok((initial, SourceText::Oversize(size)));
    }
    let mut file = fs::File::open(path)?;
    let opened = regular_file_metadata(file.metadata()?)?;
    if opened.len() > max_bytes {
        let size = opened.len();
        return Ok((opened, SourceText::Oversize(size)));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
    (&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let read = file.metadata()?;
    let read_len = bytes.len() as u64;
    if read_len > max_bytes || read.len() > max_bytes {
        let size = read.len().max(read_len);
        return Ok((read, SourceText::Oversize(size)));
    }
    if has_mpeg_ts_extension(relative) && is_mpeg_transport_stream(&bytes) {
        return Ok((read, SourceText::MpegTransportStream));
    }
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
    Ok((read, SourceText::Text(text)))
}

/// Whether [`read_source_file`] would see source at `path`: everything except
/// an MPEG transport stream named `.ts` within the limit (a file over it is
/// still recorded, by its size stamp). Reads at most the head the check needs,
/// and only for a `.ts` name, so a pending-change scan stays cheap.
pub fn is_source_file(path: &Path, relative: &str, max_bytes: u64) -> io::Result<bool> {
    if !has_mpeg_ts_extension(relative) {
        return Ok(true);
    }
    let metadata = regular_file_metadata(fs::metadata(path)?)?;
    if metadata.len() > max_bytes {
        return Ok(true);
    }
    let mut head = Vec::with_capacity(MPEG_TS_SNIFF_BYTES);
    fs::File::open(path)?
        .take(MPEG_TS_SNIFF_BYTES as u64)
        .read_to_end(&mut head)?;
    Ok(!is_mpeg_transport_stream(&head))
}

fn regular_file_metadata(metadata: fs::Metadata) -> io::Result<fs::Metadata> {
    if metadata.is_file() {
        Ok(metadata)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path is not a regular file",
        ))
    }
}

/// Whether `relative` carries the one extension MPEG-TS shares with a language.
fn has_mpeg_ts_extension(relative: &str) -> bool {
    let bytes = relative.as_bytes();
    bytes.len() > 3 && bytes[bytes.len() - 3..].eq_ignore_ascii_case(b".ts")
}

/// Whether a file starting with `head` is an MPEG transport stream, the other
/// thing a `.ts` file can be: tree-sitter takes tens of seconds on a clip for
/// zero symbols. Both conditions are required:
///   1. the sync byte 0x47 opens each of the first [`MPEG_TS_MIN_PACKETS`]
///      188-byte packets, as it opens every packet of a stream;
///   2. the head is binary: at least 1/64 of it is control bytes other than
///      the whitespace ones, as any compressed payload carries.
///
/// 0x47 is `G`, so source could satisfy (1) by putting one at every stride;
/// no source carries the dozens of control bytes (2) asks for.
fn is_mpeg_transport_stream(head: &[u8]) -> bool {
    let head = &head[..head.len().min(MPEG_TS_SNIFF_BYTES)];
    let last_sync = MPEG_TS_PACKET_SIZE * (MPEG_TS_MIN_PACKETS - 1);
    if head.len() <= last_sync {
        return false;
    }
    if (0..=last_sync)
        .step_by(MPEG_TS_PACKET_SIZE)
        .any(|offset| head[offset] != MPEG_TS_SYNC_BYTE)
    {
        return false;
    }
    let control = head
        .iter()
        .filter(|&&byte| byte < 0x20 && !(0x09..=0x0d).contains(&byte))
        .count();
    control * 64 >= head.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codegraph-source-file-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// `packets` transport-stream packets with a deterministic binary payload.
    fn clip(packets: usize) -> Vec<u8> {
        let mut state = 0x9e37_79b9_u32;
        let mut payload = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        };
        (0..packets)
            .flat_map(|_| {
                std::iter::once(MPEG_TS_SYNC_BYTE)
                    .chain((0..187).map(|_| payload()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn a_transport_stream_needs_sixteen_aligned_packets_and_a_binary_head() {
        assert!(is_mpeg_transport_stream(&clip(16)));
        assert!(is_mpeg_transport_stream(&clip(64)));
        assert!(!is_mpeg_transport_stream(&clip(15)), "too short to judge");
        let mut misaligned = clip(64);
        misaligned[188 * 7] = b'x';
        assert!(!is_mpeg_transport_stream(&misaligned));

        // TypeScript with a `G` at every stride and one raw NUL is still text.
        let mut source = Vec::new();
        for _ in 0..16 {
            let mut line = b"G".to_vec();
            line.extend(std::iter::repeat_n(b'a', 186));
            line.push(b'\n');
            source.extend(line);
        }
        source[100] = 0;
        assert!(!is_mpeg_transport_stream(&source));
    }

    #[test]
    fn the_extension_check_is_the_last_three_characters_case_blind() {
        assert!(has_mpeg_ts_extension("clips/a.ts"));
        assert!(has_mpeg_ts_extension("CLIP.TS"));
        assert!(has_mpeg_ts_extension("types.d.ts"));
        assert!(!has_mpeg_ts_extension(".ts"));
        assert!(!has_mpeg_ts_extension("a.tsx"));
        assert!(!has_mpeg_ts_extension("a.mts"));
    }

    #[test]
    fn reads_text_lossily_stamps_oversize_and_recognises_clips() {
        let dir = temp_dir("read");
        fs::write(dir.join("app.ts"), "export const a = 1;\n").unwrap();
        fs::write(dir.join("latin1.c"), b"/* caf\xe9 */\n").unwrap();
        fs::write(dir.join("clip.ts"), clip(64)).unwrap();
        fs::write(dir.join("clip.bin.c"), clip(64)).unwrap();
        fs::write(dir.join("big.ts"), "a".repeat(65)).unwrap();
        fs::write(dir.join("edge.ts"), "a".repeat(64)).unwrap();
        fs::create_dir(dir.join("folder.ts")).unwrap();

        let read = |name: &str| read_source_file(&dir.join(name), name, 64).map(|(_, text)| text);
        assert_eq!(
            read("app.ts").unwrap(),
            SourceText::Text("export const a = 1;\n".to_string())
        );
        assert_eq!(
            read("latin1.c").unwrap(),
            SourceText::Text("/* caf\u{fffd} */\n".to_string())
        );
        assert_eq!(read("big.ts").unwrap(), SourceText::Oversize(65));
        assert_eq!(read("edge.ts").unwrap(), SourceText::Text("a".repeat(64)));
        assert!(read("folder.ts").is_err());
        assert!(read("missing.ts").is_err());

        let read_large =
            |name: &str| read_source_file(&dir.join(name), name, 1024 * 1024).map(|(_, text)| text);
        assert_eq!(
            read_large("clip.ts").unwrap(),
            SourceText::MpegTransportStream
        );
        // Only a `.ts` name can be a transport stream; other binaries decode.
        assert!(matches!(
            read_large("clip.bin.c").unwrap(),
            SourceText::Text(_)
        ));

        assert_eq!(
            SourceText::Oversize(65).hash_input().as_deref(),
            Some("codegraph:oversize:65")
        );
        assert_eq!(SourceText::MpegTransportStream.hash_input(), None);

        let is_source = |name: &str, max: u64| is_source_file(&dir.join(name), name, max).unwrap();
        assert!(!is_source("clip.ts", 1024 * 1024));
        assert!(
            is_source("clip.ts", 64),
            "an oversize clip is recorded by its stamp"
        );
        assert!(is_source("clip.bin.c", 1024 * 1024));
        assert!(is_source("app.ts", 1024 * 1024));
        fs::remove_dir_all(&dir).ok();
    }
}
