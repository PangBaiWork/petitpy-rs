//! Piped-stdout buffering tests for lane P14.
//!
//! The reference buffers piped stdout (glibc stdio on the
//! `stdout` FILE*: fully-buffered, 4096 bytes, line-buffered
//! on a tty); Rust's `Stdout` used to be a `LineWriter` that
//! flushed on every `\n`. These tests pin the ARRIVAL TIME of
//! piped output, not just its bytes: a `print` followed by a
//! 1 s sleep must not surface before the sleep ends, while
//! `print(flush=True)` and `os.write(1, ...)` (a raw `write(2)`
//! on the reference) surface at once. Thresholds are loose
//! (the 1.5-core VM is slow and tests run in parallel): the
//! buffered case asserts nothing arrives in the first 0.5 s,
//! the immediate cases assert the first byte within 2 s.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("p14-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `src` under the built binary with piped stdout;
/// return (rc, [(arrival_secs, chunk)]). `name` must be
/// unique per test — the tests run in parallel in one
/// process, so a shared scratch dir would race (one test
/// deleting another's case file mid-run).
fn run_timed(name: &str, src: &str) -> (i32, Vec<(f64, Vec<u8>)>) {
    let dir = scratch(name);
    let path = dir.join("case.py");
    std::fs::write(&path, src).unwrap();
    let t0 = Instant::now();
    let mut child = Command::new(bin_petitpy())
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<(f64, Vec<u8>)>();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match out.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = tx.send((t0.elapsed().as_secs_f64(), buf[..n].to_vec()));
                }
                Err(_) => break,
            }
        }
    });
    let status = child.wait().unwrap();
    let _ = reader.join();
    let mut arrivals = Vec::new();
    while let Ok(a) = rx.try_recv() {
        arrivals.push(a);
    }
    let _ = std::fs::remove_dir_all(&dir);
    (
        status.code().unwrap_or(-1),
        arrivals,
    )
}

fn content(arrivals: &[(f64, Vec<u8>)]) -> Vec<u8> {
    arrivals.iter().flat_map(|(_, c)| c.iter().copied()).collect()
}

#[test]
fn p14_piped_print_is_buffered() {
    let (rc, arrivals) = run_timed("buffered", "import time\nprint('hello')\ntime.sleep(1)\nprint('bye')\n");
    assert_eq!(rc, 0);
    assert_eq!(content(&arrivals), b"hello\nbye\n");
    // Nothing may surface while the program sleeps: the first
    // chunk must arrive after 0.5 s (the sleep is 1 s; the old
    // write-through delivered 'hello' at ~0 s).
    let first = arrivals.first().map(|(t, _)| *t).unwrap_or(99.0);
    assert!(first > 0.5, "first chunk arrived at {first:.2}s, expected > 0.5s: {arrivals:?}");
}

#[test]
fn p14_print_flush_true_is_immediate() {
    let (rc, arrivals) = run_timed("flush", "import time\nprint('x', flush=True)\ntime.sleep(1)\nprint('y')\n");
    assert_eq!(rc, 0);
    assert_eq!(content(&arrivals), b"x\ny\n");
    let first = arrivals.first().map(|(t, _)| *t).unwrap_or(99.0);
    assert!(first < 2.0, "flush=True output arrived at {first:.2}s, expected < 2.0s");
    // The second line still waits for the sleep.
    assert!(arrivals.len() >= 2, "expected two arrivals, got {arrivals:?}");
    assert!(arrivals[1].0 > 0.5, "second chunk at {:.2}s, expected > 0.5s", arrivals[1].0);
}

#[test]
fn p14_os_write_is_immediate() {
    let (rc, arrivals) =
        run_timed("oswrite", "import os, time\nprint('a')\nos.write(1, b'b')\ntime.sleep(1)\n");
    assert_eq!(rc, 0);
    // `os.write` bypasses the buffer ('b' first), the buffered
    // `print` output surfaces at exit ('a' after the sleep).
    assert_eq!(content(&arrivals), b"ba\n");
    let first = arrivals.first().map(|(t, _)| *t).unwrap_or(99.0);
    assert!(first < 2.0, "os.write output arrived at {first:.2}s, expected < 2.0s");
    let last = arrivals.last().map(|(t, _)| *t).unwrap_or(0.0);
    assert!(last > 0.5, "buffered print output at {last:.2}s, expected > 0.5s");
}
