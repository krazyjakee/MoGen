//! FBX object-ID allocator. Object IDs are 64-bit integers, unique per file.
//!
//! ID `0` is reserved for the implicit `RootNode` referenced by every
//! Documents block, so the allocator starts above it. We don't try to be
//! globally unique across files — a fresh allocator is built per export.

/// Sequential i64 dispenser for FBX object IDs.
pub(super) struct IdAllocator {
    next: i64,
}

impl IdAllocator {
    pub fn new() -> Self {
        // Start at a comfortable offset so IDs don't visually collide with
        // the well-known `RootNode = 0` and any small constants that crop
        // up in inspectors. Picked to match what the official Autodesk
        // exporter emits — Blender's importer doesn't care about the
        // value, but matching it makes diffs against reference files
        // easier to read.
        Self { next: 1_000_000_000 }
    }

    /// Allocate the next unused ID.
    pub fn alloc(&mut self) -> i64 {
        let id = self.next;
        self.next += 1;
        id
    }
}
