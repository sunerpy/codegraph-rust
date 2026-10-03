use crate::outcome::Outcome::Err;

pub fn top_level_use() -> crate::outcome::Outcome {
    Err(1)
}

pub mod child {
    pub fn parent_use_does_not_leak() -> Result<u8, u8> {
        Err(2)
    }
}

pub mod globbed {
    use super::*;

    pub fn child_glob() -> crate::outcome::Outcome {
        Err(3)
    }
}

pub mod braced {
    use crate::outcome::Outcome::{Err, Ok};

    pub fn braced_use() -> crate::outcome::Outcome {
        let _ = Ok(0);
        Err(4)
    }
}

pub mod scoped {
    pub fn fn_local_use() -> crate::outcome::Outcome {
        use crate::outcome::Outcome::Err;
        Err(5)
    }

    pub fn sibling_fn() -> Result<u8, u8> {
        Err(6)
    }

    // use crate::outcome::Outcome::Err;
    pub fn commented_use() -> Result<u8, u8> {
        Err(7)
    }
}

pub mod aliased {
    use crate::outcome::Outcome::Err as Failure;

    pub fn alias_is_not_err() -> Result<u8, u8> {
        let _ = Failure(0);
        Err(8)
    }
}
