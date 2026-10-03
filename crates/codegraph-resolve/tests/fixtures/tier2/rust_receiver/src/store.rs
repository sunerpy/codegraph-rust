pub struct Store;

pub fn helper() -> u8 {
    2
}

pub fn store() -> Store {
    Store
}

pub fn lookup() -> usize {
    store().nodes_by_ids(&[1, 2])
}

impl Store {
    pub fn nodes_by_ids(&self, ids: &[u8]) -> usize {
        ids.len()
    }

    pub fn helper(&self) -> u8 {
        1
    }

    pub fn only_method(&self) -> u8 {
        3
    }
}

pub fn calls_helper() -> u8 {
    helper()
}

pub fn calls_only_method() -> u8 {
    only_method()
}
