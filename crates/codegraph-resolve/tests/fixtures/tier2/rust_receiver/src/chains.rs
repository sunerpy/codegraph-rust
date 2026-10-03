use crate::util::join;
use crate::{make_opt, root, LineIndex};

pub fn dropped_join(name: &str) -> std::path::PathBuf {
    root().join(name)
}

pub fn broken_chain(name: &str) -> std::path::PathBuf {
    root()
        .join(name)
}

pub fn dropped_take() -> Option<u8> {
    make_opt().take()
}

pub fn explicit_path(idx: &LineIndex) -> String {
    LineIndex::join(idx, ";")
}

pub fn imported_free_fn() -> String {
    join(&["a", "b"])
}
