pub trait Source {
    fn read(&mut self) -> usize;
}

pub struct FileSource {
    pub n: usize,
}

impl Source for FileSource {
    fn read(&mut self) -> usize {
        self.n
    }
}

pub struct BufSource<T> {
    pub inner: T,
}

impl<T> Source for BufSource<T> {
    fn read(&mut self) -> usize {
        0
    }
}

pub struct Parents<'a> {
    pub current: &'a u32,
}

impl<'a> Iterator for Parents<'a> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        None
    }
}

pub struct Wrapper {
    pub n: usize,
}

impl Source for &Wrapper {
    fn read(&mut self) -> usize {
        1
    }
}

pub mod nested {
    pub struct Scoped {
        pub n: usize,
    }
}

impl Source for nested::Scoped {
    fn read(&mut self) -> usize {
        2
    }
}

pub struct Own {
    pub n: usize,
}

impl From<u32> for Own {
    fn from(n: u32) -> Self {
        Own { n: n as usize }
    }
}

pub trait Base {
    fn id(&self) -> u32;
}

impl Base for (u32, u32) {
    fn id(&self) -> u32 {
        0
    }
}

impl Base for dyn Base {
    fn id(&self) -> u32 {
        1
    }
}

impl Base for *const u8 {
    fn id(&self) -> u32 {
        2
    }
}

impl Base for u32 {
    fn id(&self) -> u32 {
        3
    }
}
