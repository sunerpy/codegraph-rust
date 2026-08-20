use crate::{Bits, Greet, UnitStruct};

pub fn describe_unit(u: &UnitStruct) -> String {
    u.greet()
}

pub fn describe_bits(b: &Bits) -> String {
    b.greet()
}
