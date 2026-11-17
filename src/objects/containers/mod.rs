//! Container objects (C9): maps, dicts, lists, tuples, sets, …
//!
//! Lane C9a ports `map.zig` + `objdict.zig` only (map + dict).
//! C9b (list/tuple/array/sequence/binary) and C9c
//! (set/range/slice/property/attrtuple) fill in the rest.

pub mod map;
pub mod map_set;
pub mod objdict;
pub mod objdict_methods;

#[cfg(test)]
pub(crate) mod map_tests;
#[cfg(test)]
pub(crate) mod objdict_tests;
#[cfg(test)]
pub(crate) mod tests;
