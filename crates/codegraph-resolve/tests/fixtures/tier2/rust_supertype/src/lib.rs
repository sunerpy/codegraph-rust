mod alias;
mod ports;

use crate::ports::Sha256Port;
use std::error::Error;

pub enum MapperError {
    Error,
    Missing,
}

impl Error for MapperError {}

pub struct Hasher {
    salt: String,
}

impl Sha256Port for Hasher {
    fn hash(&self) -> String {
        String::new()
    }
}
