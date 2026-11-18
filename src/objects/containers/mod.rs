//! Container objects (C9): maps, dicts, lists, tuples, sets, …
//!
//! Lane C9a ports `map.zig` + `objdict.zig` only (map + dict).
//! C9b (list/tuple/array/sequence/binary) and C9c
//! (set/range/slice/property/attrtuple) fill in the rest.

pub mod binary;
pub mod map;
pub mod map_set;
pub mod objarray;
pub mod objarray_make_new;
pub mod objarray_methods;
pub mod objattrtuple;
#[cfg(test)]
pub(crate) mod objattrtuple_tests;
pub mod objdict;
pub mod objdict_collect;
pub mod objdict_methods;
pub mod objfileio;
pub mod objlist;
pub mod objlist_methods;
pub mod objproperty;
#[cfg(test)]
pub(crate) mod objproperty_tests;
pub mod objrange;
#[cfg(test)]
pub(crate) mod objrange_tests;
pub mod objstringio;
pub mod objstringio_iobase;
pub mod objstringio_types;
#[cfg(test)]
pub(crate) mod objstringio_tests;
#[cfg(test)]
pub(crate) mod objstringio_tests_ctx;
#[cfg(test)]
pub(crate) mod objfileio_tests;
pub mod objset;
pub mod objset_methods;
pub mod objslice;
#[cfg(test)]
pub(crate) mod objslice_tests;
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
pub(crate) mod objset_tests;
#[cfg(test)]
pub(crate) mod objset_tests_b;
#[cfg(test)]
pub(crate) mod tests;
