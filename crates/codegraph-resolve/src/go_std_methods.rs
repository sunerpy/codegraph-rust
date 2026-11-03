//! The methods of the standard-library types Go code embeds (upstream
//! `src/resolution/go-stdlib-methods.ts` at `e8ddc4e2`, #584, #2759). An
//! embedded type from outside the index (`io.Closer`, `http.ResponseWriter`,
//! `*os.File`) resolves to nothing, so the method set of a type embedding one
//! comes from here.
//!
//! Each row is an import path, a type that package declares, and the type's
//! methods, space-separated, as `name:params/results`: the counts
//! [`go_signature_arity`] reads off a stored signature, so a project method
//! answers a table method only with the same counts. An interface lists every
//! method, embedded ones included; a struct or other named type lists those
//! its embedded fields promote too, and an alias its target's. Exported names
//! only, and an unexported one where a listed interface requires it
//! (`testing.TB`'s `private`).
//!
//! The rows between the markers are generated from Go 1.27.3's source
//! (`go1.27.3.src.tar.gz`, sha256
//! `6e7749035cf360f06b2575d162ea79df9fda8ef671889e5c375f8e50777f26bb`):
//! `bench --gen-go-std-methods --go-src <dir> --write` rewrites them for the
//! types they list, and `--check` compares. Add or drop a type by editing a
//! row, then regenerate.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::frameworks::js_scan::js_trim;

/// `(import path, type, methods)`, in upstream's order.
#[rustfmt::skip]
pub const GO_STD_METHOD_ROWS: &[(&str, &str, &str)] = &[
    // BEGIN GENERATED (bench --gen-go-std-methods)
    ("io", "Reader", "Read:1/2"),
    ("io", "Writer", "Write:1/2"),
    ("io", "Closer", "Close:0/1"),
    ("io", "Seeker", "Seek:2/2"),
    ("io", "ReadWriter", "Read:1/2 Write:1/2"),
    ("io", "ReadCloser", "Close:0/1 Read:1/2"),
    ("io", "WriteCloser", "Close:0/1 Write:1/2"),
    ("io", "ReadWriteCloser", "Close:0/1 Read:1/2 Write:1/2"),
    ("io", "ReadSeeker", "Read:1/2 Seek:2/2"),
    ("io", "ReadSeekCloser", "Close:0/1 Read:1/2 Seek:2/2"),
    ("io", "WriteSeeker", "Seek:2/2 Write:1/2"),
    ("io", "ReadWriteSeeker", "Read:1/2 Seek:2/2 Write:1/2"),
    ("io", "ReaderFrom", "ReadFrom:1/2"),
    ("io", "WriterTo", "WriteTo:1/2"),
    ("io", "ReaderAt", "ReadAt:2/2"),
    ("io", "WriterAt", "WriteAt:2/2"),
    ("io", "ByteReader", "ReadByte:0/2"),
    ("io", "ByteScanner", "ReadByte:0/2 UnreadByte:0/1"),
    ("io", "ByteWriter", "WriteByte:1/1"),
    ("io", "RuneReader", "ReadRune:0/3"),
    ("io", "RuneScanner", "ReadRune:0/3 UnreadRune:0/1"),
    ("io", "StringWriter", "WriteString:1/2"),
    ("io", "PipeReader", "Close:0/1 CloseWithError:1/1 Read:1/2"),
    ("io", "PipeWriter", "Close:0/1 CloseWithError:1/1 Write:1/2"),
    ("io", "SectionReader", "Outer:0/3 Read:1/2 ReadAt:2/2 Seek:2/2 Size:0/1"),
    ("io/fs", "FS", "Open:1/2"),
    ("io/fs", "File", "Close:0/1 Read:1/2 Stat:0/2"),
    ("io/fs", "DirEntry", "Info:0/2 IsDir:0/1 Name:0/1 Type:0/1"),
    ("io/fs", "FileInfo", "IsDir:0/1 ModTime:0/1 Mode:0/1 Name:0/1 Size:0/1 Sys:0/1"),
    ("io/fs", "ReadDirFile", "Close:0/1 Read:1/2 ReadDir:1/2 Stat:0/2"),
    ("io/fs", "ReadDirFS", "Open:1/2 ReadDir:1/2"),
    ("io/fs", "ReadFileFS", "Open:1/2 ReadFile:1/2"),
    ("io/fs", "StatFS", "Open:1/2 Stat:1/2"),
    ("io/fs", "SubFS", "Open:1/2 Sub:1/2"),
    ("io/fs", "GlobFS", "Glob:1/2 Open:1/2"),
    ("os", "File", "Chdir:0/1 Chmod:1/1 Chown:2/1 Close:0/1 Fd:0/1 Name:0/1 PollFD:0/1 Read:1/2 ReadAt:2/2 ReadDir:1/2 ReadFrom:1/2 Readdir:1/2 Readdirnames:1/2 Seek:2/2 SetDeadline:1/1 SetReadDeadline:1/1 SetWriteDeadline:1/1 Stat:0/2 Sync:0/1 SyscallConn:0/2 Truncate:1/1 Write:1/2 WriteAt:2/2 WriteString:1/2 WriteTo:1/2"),
    ("os", "FileInfo", "IsDir:0/1 ModTime:0/1 Mode:0/1 Name:0/1 Size:0/1 Sys:0/1"),
    ("os", "DirEntry", "Info:0/2 IsDir:0/1 Name:0/1 Type:0/1"),
    ("os", "Signal", "Signal:0/0 String:0/1"),
    ("fmt", "Stringer", "String:0/1"),
    ("fmt", "GoStringer", "GoString:0/1"),
    ("fmt", "Formatter", "Format:2/0"),
    ("fmt", "State", "Flag:1/1 Precision:0/2 Width:0/2 Write:1/2"),
    ("fmt", "Scanner", "Scan:2/1"),
    ("fmt", "ScanState", "Read:1/2 ReadRune:0/3 SkipSpace:0/0 Token:2/2 UnreadRune:0/1 Width:0/2"),
    ("sort", "Interface", "Len:0/1 Less:2/1 Swap:2/0"),
    ("container/heap", "Interface", "Len:0/1 Less:2/1 Pop:0/1 Push:1/0 Swap:2/0"),
    ("context", "Context", "Deadline:0/2 Done:0/1 Err:0/1 Value:1/1"),
    ("sync", "Locker", "Lock:0/0 Unlock:0/0"),
    ("sync", "Mutex", "Lock:0/0 TryLock:0/1 Unlock:0/0"),
    ("sync", "RWMutex", "Lock:0/0 RLock:0/0 RLocker:0/1 RUnlock:0/0 TryLock:0/1 TryRLock:0/1 Unlock:0/0"),
    ("net", "Conn", "Close:0/1 LocalAddr:0/1 Read:1/2 RemoteAddr:0/1 SetDeadline:1/1 SetReadDeadline:1/1 SetWriteDeadline:1/1 Write:1/2"),
    ("net", "PacketConn", "Close:0/1 LocalAddr:0/1 ReadFrom:1/3 SetDeadline:1/1 SetReadDeadline:1/1 SetWriteDeadline:1/1 WriteTo:2/2"),
    ("net", "Listener", "Accept:0/2 Addr:0/1 Close:0/1"),
    ("net", "Addr", "Network:0/1 String:0/1"),
    ("net", "Error", "Error:0/1 Temporary:0/1 Timeout:0/1"),
    ("net", "TCPConn", "Close:0/1 CloseRead:0/1 CloseWrite:0/1 File:0/2 LocalAddr:0/1 MultipathTCP:0/2 Read:1/2 ReadFrom:1/2 RemoteAddr:0/1 SetDeadline:1/1 SetKeepAlive:1/1 SetKeepAliveConfig:1/1 SetKeepAlivePeriod:1/1 SetLinger:1/1 SetNoDelay:1/1 SetReadBuffer:1/1 SetReadDeadline:1/1 SetWriteBuffer:1/1 SetWriteDeadline:1/1 SyscallConn:0/2 Write:1/2 WriteTo:1/2"),
    ("net", "UDPConn", "Close:0/1 File:0/2 LocalAddr:0/1 Read:1/2 ReadFrom:1/3 ReadFromUDP:1/3 ReadFromUDPAddrPort:1/3 ReadMsgUDP:2/5 ReadMsgUDPAddrPort:2/5 RemoteAddr:0/1 SetDeadline:1/1 SetReadBuffer:1/1 SetReadDeadline:1/1 SetWriteBuffer:1/1 SetWriteDeadline:1/1 SyscallConn:0/2 Write:1/2 WriteMsgUDP:3/3 WriteMsgUDPAddrPort:3/3 WriteTo:2/2 WriteToUDP:2/2 WriteToUDPAddrPort:2/2"),
    ("net", "UnixConn", "Close:0/1 CloseRead:0/1 CloseWrite:0/1 File:0/2 LocalAddr:0/1 Read:1/2 ReadFrom:1/3 ReadFromUnix:1/3 ReadMsgUnix:2/5 RemoteAddr:0/1 SetDeadline:1/1 SetReadBuffer:1/1 SetReadDeadline:1/1 SetWriteBuffer:1/1 SetWriteDeadline:1/1 SyscallConn:0/2 Write:1/2 WriteMsgUnix:3/3 WriteTo:2/2 WriteToUnix:2/2"),
    ("net", "TCPListener", "Accept:0/2 AcceptTCP:0/2 Addr:0/1 Close:0/1 File:0/2 SetDeadline:1/1 SyscallConn:0/2"),
    ("net", "UnixListener", "Accept:0/2 AcceptUnix:0/2 Addr:0/1 Close:0/1 File:0/2 SetDeadline:1/1 SetUnlinkOnClose:1/0 SyscallConn:0/2"),
    ("net/http", "ResponseWriter", "Header:0/1 Write:1/2 WriteHeader:1/0"),
    ("net/http", "Hijacker", "Hijack:0/3"),
    ("net/http", "Flusher", "Flush:0/0"),
    ("net/http", "CloseNotifier", "CloseNotify:0/1"),
    ("net/http", "Pusher", "Push:2/1"),
    ("net/http", "Handler", "ServeHTTP:2/0"),
    ("net/http", "RoundTripper", "RoundTrip:1/2"),
    ("net/http", "File", "Close:0/1 Read:1/2 Readdir:1/2 Seek:2/2 Stat:0/2"),
    ("net/http", "FileSystem", "Open:1/2"),
    ("net/http", "CookieJar", "Cookies:1/1 SetCookies:2/0"),
    ("net/http", "ServeMux", "Handle:2/0 HandleFunc:2/0 Handler:1/2 ServeHTTP:2/0"),
    ("net/http", "Transport", "CancelRequest:1/0 Clone:0/1 CloseIdleConnections:0/0 NewClientConn:3/2 RegisterProtocol:2/0 RoundTrip:1/2"),
    ("net/http/httptest", "ResponseRecorder", "Flush:0/0 Header:0/1 Result:0/1 Write:1/2 WriteHeader:1/0 WriteString:1/2"),
    ("bufio", "Reader", "Buffered:0/1 Discard:1/2 Peek:1/2 Read:1/2 ReadByte:0/2 ReadBytes:1/2 ReadLine:0/3 ReadRune:0/3 ReadSlice:1/2 ReadString:1/2 Reset:1/0 Size:0/1 UnreadByte:0/1 UnreadRune:0/1 WriteTo:1/2"),
    ("bufio", "Writer", "Available:0/1 AvailableBuffer:0/1 Buffered:0/1 Flush:0/1 ReadFrom:1/2 Reset:1/0 Size:0/1 Write:1/2 WriteByte:1/1 WriteRune:1/2 WriteString:1/2"),
    ("bufio", "ReadWriter", "Available:0/1 AvailableBuffer:0/1 Buffered:0/1 Discard:1/2 Flush:0/1 Peek:1/2 Read:1/2 ReadByte:0/2 ReadBytes:1/2 ReadFrom:1/2 ReadLine:0/3 ReadRune:0/3 ReadSlice:1/2 ReadString:1/2 Reset:1/0 Size:0/1 UnreadByte:0/1 UnreadRune:0/1 Write:1/2 WriteByte:1/1 WriteRune:1/2 WriteString:1/2 WriteTo:1/2"),
    ("bytes", "Buffer", "Available:0/1 AvailableBuffer:0/1 Bytes:0/1 Cap:0/1 Grow:1/0 Len:0/1 Next:1/1 Peek:1/2 Read:1/2 ReadByte:0/2 ReadBytes:1/2 ReadFrom:1/2 ReadRune:0/3 ReadString:1/2 Reset:0/0 String:0/1 Truncate:1/0 UnreadByte:0/1 UnreadRune:0/1 Write:1/2 WriteByte:1/1 WriteRune:1/2 WriteString:1/2 WriteTo:1/2"),
    ("bytes", "Reader", "Len:0/1 Read:1/2 ReadAt:2/2 ReadByte:0/2 ReadRune:0/3 Reset:1/0 Seek:2/2 Size:0/1 UnreadByte:0/1 UnreadRune:0/1 WriteTo:1/2"),
    ("strings", "Reader", "Len:0/1 Read:1/2 ReadAt:2/2 ReadByte:0/2 ReadRune:0/3 Reset:1/0 Seek:2/2 Size:0/1 UnreadByte:0/1 UnreadRune:0/1 WriteTo:1/2"),
    ("strings", "Builder", "Cap:0/1 Grow:1/0 Len:0/1 Reset:0/0 String:0/1 Write:1/2 WriteByte:1/1 WriteRune:1/2 WriteString:1/2"),
    ("testing", "TB", "ArtifactDir:0/1 Attr:2/0 Chdir:1/0 Cleanup:1/0 Context:0/1 Error:1/0 Errorf:2/0 Fail:0/0 FailNow:0/0 Failed:0/1 Fatal:1/0 Fatalf:2/0 Helper:0/0 Log:1/0 Logf:2/0 Name:0/1 Output:0/1 Setenv:2/0 Skip:1/0 SkipNow:0/0 Skipf:2/0 Skipped:0/1 TempDir:0/1 private:0/0"),
    ("testing", "T", "ArtifactDir:0/1 Attr:2/0 Chdir:1/0 Cleanup:1/0 Context:0/1 Deadline:0/2 Error:1/0 Errorf:2/0 Fail:0/0 FailNow:0/0 Failed:0/1 Fatal:1/0 Fatalf:2/0 Helper:0/0 Log:1/0 Logf:2/0 Name:0/1 Output:0/1 Parallel:0/0 Run:2/1 Setenv:2/0 Skip:1/0 SkipNow:0/0 Skipf:2/0 Skipped:0/1 TempDir:0/1 private:0/0"),
    ("testing", "B", "ArtifactDir:0/1 Attr:2/0 Chdir:1/0 Cleanup:1/0 Context:0/1 Elapsed:0/1 Error:1/0 Errorf:2/0 Fail:0/0 FailNow:0/0 Failed:0/1 Fatal:1/0 Fatalf:2/0 Helper:0/0 Log:1/0 Logf:2/0 Loop:0/1 Name:0/1 Output:0/1 ReportAllocs:0/0 ReportMetric:2/0 ResetTimer:0/0 Run:2/1 RunParallel:1/0 SetBytes:1/0 SetParallelism:1/0 Setenv:2/0 Skip:1/0 SkipNow:0/0 Skipf:2/0 Skipped:0/1 StartTimer:0/0 StopTimer:0/0 TempDir:0/1 private:0/0"),
    ("testing", "F", "Add:1/0 ArtifactDir:0/1 Attr:2/0 Chdir:1/0 Cleanup:1/0 Context:0/1 Error:1/0 Errorf:2/0 Fail:0/0 FailNow:0/0 Failed:0/1 Fatal:1/0 Fatalf:2/0 Fuzz:1/0 Helper:0/0 Log:1/0 Logf:2/0 Name:0/1 Output:0/1 Setenv:2/0 Skip:1/0 SkipNow:0/0 Skipf:2/0 Skipped:0/1 TempDir:0/1 private:0/0"),
    ("log/slog", "Handler", "Enabled:2/1 Handle:2/1 WithAttrs:1/1 WithGroup:1/1"),
    ("log/slog", "JSONHandler", "Enabled:2/1 Handle:2/1 WithAttrs:1/1 WithGroup:1/1"),
    ("log/slog", "TextHandler", "Enabled:2/1 Handle:2/1 WithAttrs:1/1 WithGroup:1/1"),
    ("encoding", "BinaryMarshaler", "MarshalBinary:0/2"),
    ("encoding", "BinaryUnmarshaler", "UnmarshalBinary:1/1"),
    ("encoding", "TextMarshaler", "MarshalText:0/2"),
    ("encoding", "TextUnmarshaler", "UnmarshalText:1/1"),
    ("encoding/json", "Marshaler", "MarshalJSON:0/2"),
    ("encoding/json", "Unmarshaler", "UnmarshalJSON:1/1"),
    ("encoding/json", "RawMessage", "MarshalJSON:0/2 UnmarshalJSON:1/1"),
    ("encoding/xml", "Marshaler", "MarshalXML:2/1"),
    ("encoding/xml", "Unmarshaler", "UnmarshalXML:2/1"),
    ("encoding/gob", "GobEncoder", "GobEncode:0/2"),
    ("encoding/gob", "GobDecoder", "GobDecode:1/1"),
    ("expvar", "Var", "String:0/1"),
    ("hash", "Hash", "BlockSize:0/1 Reset:0/0 Size:0/1 Sum:1/1 Write:1/2"),
    ("hash", "Hash32", "BlockSize:0/1 Reset:0/0 Size:0/1 Sum:1/1 Sum32:0/1 Write:1/2"),
    ("hash", "Hash64", "BlockSize:0/1 Reset:0/0 Size:0/1 Sum:1/1 Sum64:0/1 Write:1/2"),
    ("crypto", "Signer", "Public:0/1 Sign:3/2"),
    ("crypto", "Decrypter", "Decrypt:3/2 Public:0/1"),
    ("crypto/elliptic", "Curve", "Add:4/2 Double:2/2 IsOnCurve:2/1 Params:0/1 ScalarBaseMult:1/2 ScalarMult:3/2"),
    ("crypto/cipher", "Block", "BlockSize:0/1 Decrypt:2/0 Encrypt:2/0"),
    ("crypto/cipher", "Stream", "XORKeyStream:2/0"),
    ("crypto/cipher", "AEAD", "NonceSize:0/1 Open:4/2 Overhead:0/1 Seal:4/1"),
    ("crypto/cipher", "BlockMode", "BlockSize:0/1 CryptBlocks:2/0"),
    ("crypto/tls", "Conn", "Close:0/1 CloseWrite:0/1 ConnectionState:0/1 Handshake:0/1 HandshakeContext:1/1 LocalAddr:0/1 NetConn:0/1 OCSPResponse:0/1 Read:1/2 RemoteAddr:0/1 SetDeadline:1/1 SetReadDeadline:1/1 SetWriteDeadline:1/1 VerifyHostname:1/1 Write:1/2"),
    ("image", "Image", "At:2/1 Bounds:0/1 ColorModel:0/1"),
    ("image", "PalettedImage", "At:2/1 Bounds:0/1 ColorIndexAt:2/1 ColorModel:0/1"),
    ("image/color", "Color", "RGBA:0/4"),
    ("image/color", "Model", "Convert:1/1"),
    ("database/sql", "Scanner", "Scan:1/1"),
    ("database/sql/driver", "Valuer", "Value:0/2"),
    ("database/sql/driver", "Driver", "Open:1/2"),
    ("database/sql/driver", "Conn", "Begin:0/2 Close:0/1 Prepare:1/2"),
    ("database/sql/driver", "Stmt", "Close:0/1 Exec:1/2 NumInput:0/1 Query:1/2"),
    ("database/sql/driver", "Tx", "Commit:0/1 Rollback:0/1"),
    ("database/sql/driver", "Rows", "Close:0/1 Columns:0/1 Next:1/1"),
    ("database/sql/driver", "Result", "LastInsertId:0/2 RowsAffected:0/2"),
    ("flag", "Value", "Set:1/1 String:0/1"),
    ("flag", "Getter", "Get:0/1 Set:1/1 String:0/1"),
    ("runtime", "Error", "Error:0/1 RuntimeError:0/0"),
    ("reflect", "Type", "Align:0/1 AssignableTo:1/1 Bits:0/1 CanSeq:0/1 CanSeq2:0/1 ChanDir:0/1 Comparable:0/1 ConvertibleTo:1/1 Elem:0/1 Field:1/1 FieldAlign:0/1 FieldByIndex:1/1 FieldByName:1/2 FieldByNameFunc:1/2 Fields:0/1 Implements:1/1 In:1/1 Ins:0/1 IsVariadic:0/1 Key:0/1 Kind:0/1 Len:0/1 Method:1/1 MethodByName:1/2 Methods:0/1 Name:0/1 NumField:0/1 NumIn:0/1 NumMethod:0/1 NumOut:0/1 Out:1/1 Outs:0/1 OverflowComplex:1/1 OverflowFloat:1/1 OverflowInt:1/1 OverflowUint:1/1 PkgPath:0/1 Size:0/1 String:0/1 common:0/1 uncommon:0/1"),
    ("time", "Time", "Add:1/1 AddDate:3/1 After:1/1 AppendBinary:1/2 AppendFormat:2/1 AppendText:1/2 Before:1/1 Clock:0/3 Compare:1/1 Date:0/3 Day:0/1 Equal:1/1 Format:1/1 GoString:0/1 GobDecode:1/1 GobEncode:0/2 Hour:0/1 ISOWeek:0/2 In:1/1 IsDST:0/1 IsZero:0/1 Local:0/1 Location:0/1 MarshalBinary:0/2 MarshalJSON:0/2 MarshalText:0/2 Minute:0/1 Month:0/1 Nanosecond:0/1 Round:1/1 Second:0/1 String:0/1 Sub:1/1 Truncate:1/1 UTC:0/1 Unix:0/1 UnixMicro:0/1 UnixMilli:0/1 UnixNano:0/1 UnmarshalBinary:1/1 UnmarshalJSON:1/1 UnmarshalText:1/1 Weekday:0/1 Year:0/1 YearDay:0/1 Zone:0/2 ZoneBounds:0/2"),
    ("compress/gzip", "Reader", "Close:0/1 Multistream:1/0 Read:1/2 Reset:1/1"),
    ("compress/gzip", "Writer", "Close:0/1 Flush:0/1 Reset:1/0 Write:1/2"),
    // END GENERATED
];

/// The rows by import path and type.
static BY_TYPE: LazyLock<HashMap<(&'static str, &'static str), &'static str>> =
    LazyLock::new(|| {
        GO_STD_METHOD_ROWS
            .iter()
            .map(|&(path, name, methods)| ((path, name), methods))
            .collect()
    });

/// The methods of the standard-library type `type_name` that the package at
/// `import_path` declares, each name with its `params/results`, or those of
/// the predeclared `error` when `import_path` is `None` (upstream
/// `goStdlibMethods`). `None` for a type the table does not list.
pub fn go_std_methods(
    import_path: Option<&str>,
    type_name: &str,
) -> Option<Vec<(&'static str, &'static str)>> {
    let Some(import_path) = import_path else {
        return (type_name == "error").then(|| vec![("Error", "0/1")]);
    };
    let methods = BY_TYPE.get(&(import_path, type_name))?;
    Some(
        methods
            .split(' ')
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.split_once(':').unwrap_or((entry, "")))
            .collect(),
    )
}

/// How many parameters and results a Go signature has, as `params/results`
/// (upstream `goSignatureArity`): `(ctx context.Context, keys ...string)
/// (int, error)` is `2/2`, and a grouped `a, b int` is two parameters.
/// `None` when the signature does not read.
pub fn go_signature_arity(signature: Option<&str>) -> Option<String> {
    let (params, results) = go_signature_parts(signature)?;
    Some(format!("{}/{}", params.len(), results.len()))
}

/// A Go signature as extraction stores it (the parameter list's text, then
/// the result's) split into its parameters and results (upstream
/// `goSignatureParts`); `None` when the text has another shape.
pub fn go_signature_parts(signature: Option<&str>) -> Option<(Vec<String>, Vec<String>)> {
    let signature = js_trim(signature?);
    if !signature.starts_with('(') {
        return None;
    }
    let (params, end) = go_list_items(signature, 0)?;
    let rest = js_trim(&signature[end + 1..]);
    if !rest.starts_with('(') {
        let results = if rest.is_empty() {
            Vec::new()
        } else {
            vec![rest.to_string()]
        };
        return Some((params, results));
    }
    let (results, end) = go_list_items(rest, 0)?;
    js_trim(&rest[end + 1..])
        .is_empty()
        .then_some((params, results))
}

/// The top-level items of the Go bracketed list opening at `text[open]` and
/// the offset of its closing bracket (upstream `goListItems`): the items of
/// `(ctx context.Context, a, b int)` are `ctx context.Context`, `a` and
/// `b int`. Nested brackets, string literals and comments are skipped whole;
/// `None` when the list never closes.
pub fn go_list_items(text: &str, open: usize) -> Option<(Vec<String>, usize)> {
    let bytes = text.as_bytes();
    let mut items = Vec::new();
    let mut depth = 0i64;
    let mut item = String::new();
    let mut at = open;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'/' && matches!(bytes.get(at + 1), Some(b'/' | b'*')) {
            let end = if bytes[at + 1] == b'/' {
                text[at..].find('\n').map(|offset| at + offset)?
            } else {
                text[at + 2..]
                    .find("*/")
                    .map(|offset| at + 2 + offset + 1)?
            };
            at = end + 1;
            item.push(' ');
            continue;
        }
        if matches!(byte, b'"' | b'`' | b'\'') {
            let mut close = at + 1;
            while close < bytes.len() && bytes[close] != byte {
                close += if byte != b'`' && bytes[close] == b'\\' {
                    2
                } else {
                    1
                };
            }
            if close >= bytes.len() {
                return None;
            }
            item.push_str(&text[at..=close]);
            at = close + 1;
            continue;
        }
        match byte {
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth == 1 {
                    at += 1;
                    continue;
                }
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    let trimmed = js_trim(&item);
                    if !trimmed.is_empty() {
                        items.push(trimmed.to_string());
                    }
                    return Some((items, at));
                }
            }
            b',' if depth == 1 => {
                let trimmed = js_trim(&item);
                if !trimmed.is_empty() {
                    items.push(trimmed.to_string());
                }
                item.clear();
                at += 1;
                continue;
            }
            _ => {}
        }
        let width = text[at..].chars().next().map_or(1, char::len_utf8);
        item.push_str(&text[at..at + width]);
        at += width;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arity(signature: &str) -> Option<String> {
        go_signature_arity(Some(signature))
    }

    #[test]
    fn arity_counts_parameters_and_results() {
        assert_eq!(
            arity("(ctx context.Context, keys ...string) (int, error)").as_deref(),
            Some("2/2")
        );
        assert_eq!(arity("(a, b int)").as_deref(), Some("2/0"));
        assert_eq!(arity("() error").as_deref(), Some("0/1"));
        assert_eq!(
            arity("(p []byte) (n int, err error)").as_deref(),
            Some("1/2")
        );
        assert_eq!(
            arity("(f func(a, b int) (c int), m map[string]int)").as_deref(),
            Some("2/0")
        );
        assert_eq!(
            arity("(s string /* a, b */, t string)").as_deref(),
            Some("2/0")
        );
        assert_eq!(
            arity("(sep string, x rune) // trailing, comment\n").as_deref(),
            Some("2/1")
        );
        assert_eq!(arity("(tag string `json:\"a,b\"`)").as_deref(), Some("1/0"));
        assert_eq!(arity("()").as_deref(), Some("0/0"));
    }

    #[test]
    fn a_signature_of_another_shape_does_not_read() {
        assert_eq!(arity("(a int"), None);
        assert_eq!(arity("int"), None);
        assert_eq!(arity("(a int) (b int) c"), None);
        assert_eq!(arity("(s string \"open)"), None);
        assert_eq!(arity("(a int /* never closed)"), None);
        assert_eq!(go_signature_arity(None), None);
    }

    #[test]
    fn the_table_answers_listed_types_and_error() {
        assert_eq!(go_std_methods(None, "error"), Some(vec![("Error", "0/1")]));
        assert_eq!(go_std_methods(None, "Reader"), None);
        assert_eq!(
            go_std_methods(Some("io"), "Reader"),
            Some(vec![("Read", "1/2")])
        );
        assert_eq!(go_std_methods(Some("io"), "Nope"), None);
        let rows: Vec<_> = GO_STD_METHOD_ROWS
            .iter()
            .map(|&(path, name, _)| (path, name))
            .collect();
        assert_eq!(rows.len(), BY_TYPE.len(), "a type is listed twice");
    }
}
