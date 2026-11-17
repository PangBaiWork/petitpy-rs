# vendor/

## qstrdefs.generated.h

Verbatim copy of `vendor/micropython_embed/genhdr/qstrdefs.generated.h`
from petitpy-orig at HEAD `df7de3a` (SHA-256
`c70113060dbf16faf8cd06a4b3a345aaf9fc434cfb65ed1476fefb99e6879196`).
It is generated in the original repo by `make -C port` (MicroPython's
`makeqstrdata.py` scanning the C sources); the file is vendored here so
the Rust crate is self-contained and `build.rs` can derive the qstr
table from it at build time, exactly as the Zig original parses it at
comptime.

To update after a vendor refresh: copy the fresh
`qstrdefs.generated.h` over this file and rebuild. `build.rs` panics if
the QDEF0/QDEF1 counts drift from N_STATIC/N_MAIN, and the tests in
`src/compiler/qstr.rs` re-parse this header independently and assert
every entry matches the generated table.
