//! Resolution reads only what extraction would accept (upstream v1.6.1 #1553):
//! a regular file no larger than the extraction size limit, checked before any
//! byte is read, with the rejection cached for the pass like any other read.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::config::DEFAULT_MAX_FILE_SIZE;
use codegraph_resolve::{
    ReferenceResolver, ResolutionContext, SnapshotResolutionContext, StoreResolutionContext,
};
use codegraph_store::Store;

static NONCE: AtomicU64 = AtomicU64::new(0);

const LIMIT: usize = DEFAULT_MAX_FILE_SIZE as usize;

struct Project {
    root: PathBuf,
    store: Option<Store>,
}

impl Drop for Project {
    fn drop(&mut self) {
        drop(self.store.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Project {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codegraph-resolution-reads-{tag}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create fixture root");
        let store = Store::open(&root.join("index.db")).expect("open store");
        Self {
            root,
            store: Some(store),
        }
    }

    fn store(&self) -> &Store {
        self.store.as_ref().expect("store")
    }

    fn root_str(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(path, content).expect("write fixture");
    }

    /// A 2 MiB sparse package archive with a gzip header, as `file:` package
    /// metadata can point an import at.
    fn sparse_archive(&self) -> &'static str {
        let relative = "node_modules/example/react_native_openharmony.har";
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(&path, [0x1f, 0x8b]).expect("write archive header");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_len(2 * 1024 * 1024))
            .expect("extend archive");
        relative
    }
}

fn contexts(project: &Project) -> (StoreResolutionContext<'_>, SnapshotResolutionContext) {
    (
        StoreResolutionContext::new(project.store(), project.root_str()),
        SnapshotResolutionContext::from_store(project.store(), project.root_str())
            .expect("snapshot"),
    )
}

#[test]
fn reads_stop_at_the_extraction_size_limit_in_both_contexts() {
    let project = Project::new("limit");
    let at_limit = "a".repeat(LIMIT);
    project.write("small.ts", "export const answer = 42;\n");
    project.write("boundary.ts", &at_limit);
    project.write("bundle.js", &"a".repeat(LIMIT + 1));
    std::fs::create_dir_all(project.root.join("directory")).expect("mkdir");
    let archive = project.sparse_archive();

    let (store_context, snapshot) = contexts(&project);
    for context in [&store_context as &dyn ResolutionContext, &snapshot] {
        assert_eq!(
            context.read_file("small.ts").as_deref(),
            Some("export const answer = 42;\n")
        );
        assert!(context.read_file("boundary.ts") == Some(at_limit.clone()));
        for rejected in ["bundle.js", archive, "directory", "missing.ts"] {
            assert_eq!(
                context.read_file(rejected).map(|content| content.len()),
                None,
                "{rejected}"
            );
            assert!(!context.is_file_readable(rejected), "{rejected}");
        }
    }
}

#[test]
fn a_rejection_is_cached_for_the_pass_and_the_next_pass_rereads() {
    let project = Project::new("cache");
    let archive = project.sparse_archive();
    let (store_context, snapshot) = contexts(&project);
    for context in [&store_context as &dyn ResolutionContext, &snapshot] {
        assert_eq!(context.read_file(archive), None);
    }

    project.write(archive, "export const repaired = true;");
    for context in [&store_context as &dyn ResolutionContext, &snapshot] {
        assert_eq!(context.read_file(archive), None);
    }
    let (store_context, snapshot) = contexts(&project);
    for context in [&store_context as &dyn ResolutionContext, &snapshot] {
        assert_eq!(
            context.read_file(archive).as_deref(),
            Some("export const repaired = true;")
        );
    }
}

/// A project that raises `max_file_size` indexes larger files, so resolution
/// must keep reading them: the limit follows the project, through every context
/// the resolver builds.
#[test]
fn a_raised_max_file_size_reaches_every_context() {
    let project = Project::new("raised");
    let generated = "a".repeat(LIMIT + 1);
    project.write("generated.ts", &generated);
    let raised = 2 * DEFAULT_MAX_FILE_SIZE;

    let store_context =
        StoreResolutionContext::new(project.store(), project.root_str()).with_max_file_size(raised);
    let snapshot = SnapshotResolutionContext::from_store(project.store(), project.root_str())
        .expect("snapshot")
        .with_max_file_size(raised);
    let resolver = ReferenceResolver::new(project.root_str()).with_max_file_size(raised);
    let resolver_context = resolver.store_context(project.store());
    for context in [
        &store_context as &dyn ResolutionContext,
        &snapshot,
        &resolver_context,
    ] {
        assert!(context.read_file("generated.ts") == Some(generated.clone()));
    }
    let default_context = ReferenceResolver::new(project.root_str()).store_context(project.store());
    assert_eq!(
        default_context
            .read_file("generated.ts")
            .map(|content| content.len()),
        None
    );
}

/// A FIFO would block a plain read forever; it is not a regular file, so it is
/// rejected before it is opened.
#[cfg(unix)]
#[test]
fn a_fifo_is_rejected_without_blocking() {
    let project = Project::new("fifo");
    let fifo = project.root.join("pipe.ts");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo failed");

    let root = project.root_str();
    let db = project.root.join("index.db");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let store = Store::open(Path::new(&db)).expect("open store");
        let store_read = StoreResolutionContext::new(&store, root.clone()).read_file("pipe.ts");
        let snapshot_read = SnapshotResolutionContext::from_store(&store, root)
            .expect("snapshot")
            .read_file("pipe.ts");
        let _ = sender.send((store_read, snapshot_read));
    });
    let reads = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("reading a FIFO blocked");
    assert_eq!(reads, (None, None));
}

/// The framework pass reads project files through the same guard, so a file
/// extraction skipped for its size yields no framework output either.
#[test]
fn framework_extraction_skips_a_file_too_large_to_extract() {
    let project = Project::new("framework");
    project.write(
        "src-tauri/src/main.rs",
        "#[tauri::command]\nfn save_config() {}\n",
    );
    project.write(
        "src-tauri/tauri.conf.json",
        "{ \"productName\": \"x\", \"identifier\": \"dev.x\", \"build\": {}, \"app\": {} }\n",
    );
    let call = "import { invoke } from '@tauri-apps/api/core';\ninvoke('save_config');\n";
    project.write("src/app.ts", call);
    project.write("src/bundle.ts", &format!("{call}{}", " ".repeat(LIMIT)));

    let mut resolver = ReferenceResolver::new(project.root_str());
    {
        let context = resolver.store_context(project.store());
        resolver.initialize(&context);
    }
    assert!(resolver.has_framework_resolvers());
    let collection = resolver.collect_framework_extraction_with(
        &["src/app.ts".to_string(), "src/bundle.ts".to_string()],
        &codegraph_resolve::framework::FrameworkExtractionContext::without_config(
            project.root_str(),
        ),
        &codegraph_extract::ExtensionOverrides::default(),
    );
    let sources = collection
        .unresolved_references
        .iter()
        .map(|reference| reference.file_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(sources, vec!["src/app.ts"]);
}
