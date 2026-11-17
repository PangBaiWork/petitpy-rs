//! Iterator objects (Lane C11 — `src/objects/iters/` in the
//! original). See [`iter`] for the shared convention, the tag
//! table, the seams, and the `len == 2` dispatch collision.

pub mod iter;
pub mod objdeque;
pub mod objenumerate;
pub mod objfilter;
pub mod objgetitemiter;
pub mod objmap;
pub mod objnamedtuple;
pub mod objreversed;
pub mod objzip;

#[cfg(test)]
pub(crate) mod tests;
