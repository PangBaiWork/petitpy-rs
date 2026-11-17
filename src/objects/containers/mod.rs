//! Container objects (C9): maps, dicts, lists, tuples, sets, …
//!
//! Lane C9a ports `map.zig` + `objdict.zig` only (map + dict).
//! C9b (list/tuple/array/sequence/binary) and C9c
//! (set/range/slice/property/attrtuple) fill in the rest.

pub mod binary;
pub mod map;
pub mod map_set;
pub mod objarray;
pub mod objarray_methods;
pub mod objdict;
pub mod objdict_methods;
pub mod objlist;
pub mod objlist_methods;
pub mod objtuple;
pub mod sequence;

#[cfg(test)]
pub(crate) mod binary_tests;
#[cfg(test)]
pub(crate) mod map_tests;
#[cfg(test)]
pub(crate) mod objarray_tests;
#[cfg(test)]
pub(crate) mod objlist_tests;
#[cfg(test)]
pub(crate) mod objtuple_tests;
#[cfg(test)]
pub(crate) mod sequence_tests;
#[cfg(test)]
pub(crate) mod objdict_tests;
#[cfg(test)]
pub(crate) mod tests;
