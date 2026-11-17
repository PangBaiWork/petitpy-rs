//! Memory management: GC arena (`memory/gc.zig` port, C4) + raw
//! allocation helpers (`memory/alloc.zig` port).
//! See `docs/value-obj-design.md`.

pub mod alloc;
pub mod gc;
pub mod pairheap;
pub mod ringbuf;
