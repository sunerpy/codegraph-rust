pub trait Greet {
    fn greet(&self) -> String;
}

pub struct UnitStruct;

pub struct BraceStruct {
    pub x: u32,
}

pub struct TupleStruct(pub u8);

impl Greet for UnitStruct {
    fn greet(&self) -> String {
        "unit".to_string()
    }
}

impl Greet for BraceStruct {
    fn greet(&self) -> String {
        "brace".to_string()
    }
}

impl Greet for TupleStruct {
    fn greet(&self) -> String {
        "tuple".to_string()
    }
}

pub union Bits {
    pub i: u32,
    pub f: f32,
}

impl Bits {
    pub fn raw(&self) -> u32 {
        unsafe { self.i }
    }
}

impl Greet for Bits {
    fn greet(&self) -> String {
        "bits".to_string()
    }
}

pub fn make_unit() -> UnitStruct {
    UnitStruct
}

pub fn make_bits() -> Bits {
    Bits { i: 0 }
}
