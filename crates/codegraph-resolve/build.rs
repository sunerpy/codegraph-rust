//! Records the build identity the synthesis stage writes beside its pass
//! records (W9-00 row 27, H3): the package version and an FNV-1a digest of
//! the sources the stage's output follows from (this crate's and the core,
//! extract and store crates' `src`, and the lockfile), walked in path order,
//! so the digest does not depend on directory iteration order or on where
//! the checkout lives.

use std::fs;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(&path, out);
        } else if kind.is_file() {
            out.push(path);
        }
    }
}

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let crates = manifest.parent().expect("the crate sits in crates/");
    let roots = [
        ("resolve", manifest.join("src")),
        ("core", crates.join("codegraph-core").join("src")),
        ("extract", crates.join("codegraph-extract").join("src")),
        ("store", crates.join("codegraph-store").join("src")),
    ];
    let lockfile = crates.parent().map(|root| root.join("Cargo.lock"));
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, root) in &roots {
        println!("cargo:rerun-if-changed={}", root.display());
        let mut files = Vec::new();
        collect(root, &mut files);
        let mut named: Vec<(String, PathBuf)> = files
            .into_iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(root)
                    .expect("walked path is under its root")
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                (format!("{name}/{relative}"), path)
            })
            .collect();
        named.sort();
        for (relative, path) in named {
            fnv(&mut hash, relative.as_bytes());
            fnv(&mut hash, &[0xff]);
            if let Ok(bytes) = fs::read(&path) {
                fnv(&mut hash, &bytes);
            }
            fnv(&mut hash, &[0xff]);
        }
    }
    if let Some(lockfile) = lockfile {
        println!("cargo:rerun-if-changed={}", lockfile.display());
        if let Ok(bytes) = fs::read(&lockfile) {
            fnv(&mut hash, &bytes);
        }
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    println!("cargo:rustc-env=CODEGRAPH_SYNTHESIS_BUILD={version}+{hash:016x}");
}
