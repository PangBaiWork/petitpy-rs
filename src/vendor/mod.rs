//! Vendor C carve-out, ported to Rust (Lane C).
//!
//! First chunk: the MicroPython bignum engine (`py/mpz.c` + `py/mpz.h`,
//! vendor MicroPython `b14d129a1`). Later chunks in this lane port the
//! remaining vendor C files (formatfloat, objfloat, objint, objint_mpz,
//! modio, objstringio, builtinhelp, warning, modre, re1.5).
//!
//! Layout mirrors the C file:
//! - [`mpn`] — the internal `mpn_*` digit-array primitives.
//! - [`mpz`] — the public `BigInt` type (`mpz_t` + `mpz_*` setters,
//!   conversions, comparison, hash).
//! - [`mpz_ops`] — the `mpz_*_inpl` arithmetic / bitwise / shift /
//!   power / divmod operations.
//! - [`re15`] — the re1.5 regex engine (Lane RE15), ported from
//!   petitpy's patched copy `port/re1.5-patched/` (int16 jump
//!   offsets), not the vanilla vendor `lib/re1.5/`.
//!
//! Configuration this port targets (petitpy `port/mpconfigport.h`,
//! full config): `MPZ_DIG_SIZE` is 32 on x86-64, so a digit is a
//! `u32` and a double digit is a `u64`. `MICROPY_OPT_MPZ_BITWISE` is
//! 0 (ROM level is MINIMUM), so the general `mpn_*_neg` bitwise
//! forms are the ones compiled in the reference build; the
//! `MICROPY_OPT_MPZ_BITWISE` variants (`mpn_and`, `mpn_or`, `mpn_xor`)
//! are not ported — they are compiled out of the original too.
//! `MICROPY_PY_BUILTINS_FLOAT` is 1, so the float conversions are
//! included.

pub mod mpn;
pub mod mpz;
pub mod mpz_ops;
pub mod re15;

pub use mpz::BigInt;

#[cfg(test)]
mod mpz_tests;
#[cfg(test)]
mod probe;
#[cfg(test)]
mod re15_probe;
#[cfg(test)]
mod re15_tests;
