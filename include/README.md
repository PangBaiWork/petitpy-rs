# `include/petitpy.h` — provenance

`petitpy.h` is the ORIGINAL's header, copied verbatim from
`~/workspace/petitpy-orig/include/petitpy.h` (petitpy 0.9.0,
Abilian, `devel` HEAD `df7de3a`). It is NOT a Rust-side
invention: the `ppy_*` C API in `src/entry/api.rs`
implements exactly the surface this header declares
(`PPY_API_VERSION` 1, `ppy_status` 0/1/2, the `ppy_config` /
`ppy_function` / `ppy_write_fn` types, all 39 exports).

The Rust implementation lives in `src/entry/api.rs`; the
library artifacts are `target/release/libpetitpy_rs.so`
(`cdylib`) and `target/release/libpetitpy_rs.a`
(`staticlib`) — see `scripts/f1_result.txt` §2 for the
link commands the original's `make test-embed` uses.
