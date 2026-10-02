//! Embeds the committed viewer bundle (`viewer/`, written by `npm run build` in
//! `ui/`) into the library as a sorted `(path, bytes)` table.
//!
//! The bundle is committed so that `cargo build` and `cargo install --git` never
//! need Node; CI rebuilds it and fails when the committed copy differs. The table
//! is sorted by path so the generated source, and therefore the binary, does not
//! depend on directory iteration order.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(root, &path, out);
        } else if kind.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("walked path is under the viewer root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let viewer = manifest.join("viewer");
    println!("cargo:rerun-if-changed=viewer");
    println!("cargo:rerun-if-changed=build.rs");

    let mut files = Vec::new();
    collect(&viewer, &viewer, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("viewer_assets.rs");
    let mut src = String::from("/// Every file of the embedded viewer bundle, sorted by path.\n");
    src.push_str("pub static VIEWER_FILES: &[(&str, &[u8])] = &[\n");
    for (rel, path) in &files {
        src.push_str(&format!(
            "    ({rel:?}, include_bytes!({:?})),\n",
            path.display().to_string()
        ));
    }
    src.push_str("];\n");
    let mut f = fs::File::create(&out).expect("create viewer_assets.rs");
    f.write_all(src.as_bytes()).expect("write viewer_assets.rs");
}
