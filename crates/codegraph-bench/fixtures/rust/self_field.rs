pub struct Inner {
    pub n: usize,
}

impl Inner {
    pub fn run(&mut self) {
        self.n += 1;
    }

    pub fn take(&mut self) {}
}

pub struct Outer {
    pub inner: Inner,
}

impl Outer {
    pub fn run(&mut self) {
        self.inner.run();
    }
}

pub struct Boxed {
    pub inner: Box<Inner>,
}

impl Boxed {
    pub fn go(&mut self) {
        self.inner.run();
    }
}

pub struct Borrowed<'a> {
    pub inner: &'a mut Inner,
}

impl<'a> Borrowed<'a> {
    pub fn go(&mut self) {
        self.inner.run();
    }
}

pub struct Optional {
    pub inner: Option<Inner>,
}

impl Optional {
    pub fn go(&mut self) {
        self.inner.take();
    }
}

pub struct Scanner {
    pub items: std::vec::IntoIter<u8>,
}

impl Scanner {
    pub fn next(&mut self) -> Option<u8> {
        self.items.next()
    }
}

pub struct Other;

impl Other {
    pub fn next(&mut self) -> Option<u8> {
        None
    }
}

pub struct Holder<T> {
    pub item: T,
}

impl<T> Holder<T> {
    pub fn go(&mut self) {
        self.item.run();
    }
}

pub struct Countdown {
    pub n: usize,
}

impl Countdown {
    pub fn run(&mut self) {
        if self.n > 0 {
            self.n -= 1;
            self.run();
        }
    }
}
