//! A re-export scan reads a barrel file without copying it into characters
//! (W10-08). This test binary counts its own allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use codegraph_core::types::Language;
use codegraph_resolve::import_resolver::extract_re_exports;

struct Counting;

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

fn count(bytes: usize) {
    let _ = ALLOCATED.try_with(|total| total.set(total.get() + bytes));
}

// SAFETY: every call forwards to `System` with the caller's layout; the count
// is a plain thread-local cell that never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn re_export_scan_allocates_no_character_copy_of_the_file() {
    // About 64 KiB of TypeScript with no comment and no export.
    let line = "const value = compute(input, other);\n";
    let text = line.repeat(65_536 / line.len());
    // The first scan compiles the patterns and fills their caches.
    assert!(extract_re_exports(&text, Language::TypeScript).is_empty());
    let before = ALLOCATED.with(Cell::get);
    let found = extract_re_exports(&text, Language::TypeScript);
    let used = ALLOCATED.with(Cell::get) - before;
    assert!(found.is_empty());
    assert!(
        used <= 2 * text.len(),
        "a re-export scan of {} bytes allocated {used} bytes",
        text.len()
    );
}
