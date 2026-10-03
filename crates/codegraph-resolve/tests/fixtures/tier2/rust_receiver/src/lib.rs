pub mod chains;
pub mod outcome;
pub mod store;
pub mod util;
pub mod variants;

pub struct LineIndex;

impl LineIndex {
    pub fn join(&self, sep: &str) -> String {
        sep.to_string()
    }

    pub fn joined(&self) -> String {
        self.join(",")
    }
}

pub struct Cache;

impl Cache {
    pub fn take(&mut self) -> Option<u8> {
        None
    }
}

pub fn root() -> std::path::PathBuf {
    std::path::PathBuf::new()
}

pub fn make_opt() -> Option<u8> {
    None
}
