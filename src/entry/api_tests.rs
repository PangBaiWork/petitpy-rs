//! Rust-side tests for the embedding API (`entry::api`,
//! F1): the handle table (tagging, free-slot reuse,
//! cross-interpreter rejection, slot 0 never handed out),
//! the reentrant lock (a nested API call from a host
//! function), per-interpreter isolation, and handle
//! survival under GC stress (`NPY_GC_STRESS=10` — the
//! original's stress run, which only catches unrooted
//! handles when the threshold takes effect).
//!
//! The C acceptance (`tests/embed/api_test.c`, run
//! UNMODIFIED against the built library) covers the same
//! ground end-to-end; these tests pin the Rust-side
//! mechanisms directly.

use std::ffi::c_char;
use std::ptr::null_mut;

use super::api::{
    ppy_define_function, ppy_eval, ppy_exec, ppy_free, ppy_get_global, ppy_new, ppy_new_int,
    ppy_release, ppy_set_global, ppy_to_int, ppy_type_name, Interp, PpyFunction, PpyHandle,
    PPY_ERROR, PPY_OK,
};

fn new_interp() -> *mut Interp {
    let py = unsafe { ppy_new(null_mut()) };
    assert!(!py.is_null(), "ppy_new(NULL) must return an interpreter");
    py
}

fn exec_ok(py: *mut Interp, src: &[u8]) {
    assert_eq!(
        unsafe { ppy_exec(py, src.as_ptr() as *const c_char, src.len(), c"<test>".as_ptr()) },
        PPY_OK,
        "exec of {:?} must succeed",
        String::from_utf8_lossy(src)
    );
}

fn new_int(py: *mut Interp, v: i64) -> PpyHandle {
    let mut h: PpyHandle = 0;
    assert_eq!(unsafe { ppy_new_int(py, v, &mut h) }, PPY_OK);
    h
}

fn to_int(py: *mut Interp, h: PpyHandle) -> i64 {
    let mut n: i64 = -1;
    assert_eq!(unsafe { ppy_to_int(py, h, &mut n) }, PPY_OK);
    n
}

#[test]
fn handle_tagging_slot0_and_free() {
    let py = new_interp();
    let h = new_int(py, 42);
    // A handle is never 0 (slot 0 is never handed out).
    assert_ne!(h, 0, "slot 0 must never be handed out");
    assert_eq!(to_int(py, h), 42);
    // Slot 0 is invalid everywhere.
    assert!(unsafe { ppy_type_name(py, 0) }.is_null());
    let mut n: i64 = -1;
    assert_eq!(unsafe { ppy_to_int(py, 0, &mut n) }, PPY_ERROR);
    unsafe { ppy_release(py, 0) }; // no-op, must not fault
    // Releasing a handle invalidates it.
    unsafe { ppy_release(py, h) };
    assert_eq!(unsafe { ppy_to_int(py, h, &mut n) }, PPY_ERROR);
    assert!(unsafe { ppy_type_name(py, h) }.is_null());
    unsafe { ppy_free(py) };
}

#[test]
fn handle_free_slot_reuse() {
    let py = new_interp();
    let h1 = new_int(py, 1);
    let _h2 = new_int(py, 2);
    unsafe { ppy_release(py, h1) };
    // The first free slot is reused (the Zig's free list).
    let h3 = new_int(py, 3);
    assert_eq!(h3, h1, "a freed slot must be reused");
    assert_eq!(to_int(py, h3), 3);
    unsafe { ppy_free(py) };
}

#[test]
fn cross_interp_handle_rejection() {
    let py1 = new_interp();
    let py2 = new_interp();
    let h = new_int(py1, 7);
    // A handle of one interpreter is invalid in another.
    let mut n: i64 = -1;
    assert_eq!(unsafe { ppy_to_int(py2, h, &mut n) }, PPY_ERROR);
    assert!(unsafe { ppy_type_name(py2, h) }.is_null());
    // Releasing it in the wrong interpreter is a no-op —
    // the value stays alive in its own interpreter.
    unsafe { ppy_release(py2, h) };
    assert_eq!(to_int(py1, h), 7);
    unsafe { ppy_free(py1) };
    unsafe { ppy_free(py2) };
}

#[test]
fn per_interp_isolation() {
    let py1 = new_interp();
    let py2 = new_interp();
    exec_ok(py1, b"x = 1");
    exec_ok(py2, b"x = 2");
    let mut h1: PpyHandle = 0;
    let mut h2: PpyHandle = 0;
    assert_eq!(
        unsafe { ppy_get_global(py1, c"x".as_ptr(), &mut h1) },
        PPY_OK
    );
    assert_eq!(
        unsafe { ppy_get_global(py2, c"x".as_ptr(), &mut h2) },
        PPY_OK
    );
    assert_eq!(to_int(py1, h1), 1);
    assert_eq!(to_int(py2, h2), 2);
    // A global set in one interpreter does not appear in the
    // other.
    let v = new_int(py1, 99);
    assert_eq!(
        unsafe { ppy_set_global(py1, c"y".as_ptr(), v) },
        PPY_OK
    );
    let mut missing: PpyHandle = 0;
    assert_eq!(
        unsafe { ppy_get_global(py2, c"y".as_ptr(), &mut missing) },
        PPY_ERROR
    );
    unsafe { ppy_free(py1) };
    unsafe { ppy_free(py2) };
}

/// A host function that calls the API again (nested
/// `ppy_eval` from inside a host call): the reentrant lock
/// must let the same thread back in instead of deadlocking.
/// The nested result handle is handed back as this call's
/// result (the dispatch releases it after copying the
/// value out).
unsafe extern "C" fn nested_host(
    py: *mut Interp,
    _userdata: *mut std::ffi::c_void,
    _argc: usize,
    _argv: *const PpyHandle,
    result: *mut PpyHandle,
) -> i32 {
    let src = b"40 + 2";
    let mut r: PpyHandle = 0;
    let st = ppy_eval(py, src.as_ptr() as *const c_char, src.len(), &mut r);
    if st != PPY_OK {
        return st;
    }
    // FFI invariant: `result` points to a writable handle
    // (the dispatch always passes one).
    unsafe { *result = r };
    PPY_OK
}

#[test]
fn reentrant_lock_nested_host_call() {
    let py = new_interp();
    let fun: PpyFunction = nested_host;
    assert_eq!(
        unsafe {
            ppy_define_function(py, c"host".as_ptr(), c"nested".as_ptr(), fun, null_mut())
        },
        PPY_OK
    );
    exec_ok(py, b"import host\nv = host.nested()");
    let mut h: PpyHandle = 0;
    assert_eq!(
        unsafe { ppy_get_global(py, c"v".as_ptr(), &mut h) },
        PPY_OK
    );
    assert_eq!(to_int(py, h), 42);
    unsafe { ppy_free(py) };
}

#[test]
fn handles_survive_gc_stress() {
    // The original's stress run (`NPY_GC_STRESS=10`): every
    // 10th allocation collects, so a handle that is not a GC
    // root is reclaimed mid-test and reads back wrong (or
    // faults). The env var is read by `gc_init` at `ppy_new`
    // time; it is left set (other tests' interps only get
    // more frequent — still correct — collections).
    std::env::set_var("NPY_GC_STRESS", "10");
    let py = new_interp();
    let mut handles = Vec::new();
    for i in 0..50i64 {
        handles.push((new_int(py, i), i));
    }
    // Allocate heavily in __main__ (thousands of objects).
    exec_ok(py, b"x = [list(range(100)) for _ in range(200)]");
    for (h, i) in &handles {
        assert_eq!(
            to_int(py, *h),
            *i,
            "handle {h} lost its value under GC stress"
        );
    }
    unsafe { ppy_free(py) };
}
