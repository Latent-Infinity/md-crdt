//! Build a document of size N and hold it for RSS sampling.
//!
//! Usage:
//!   cargo run --release --manifest-path md-crdt-yrs-bench/Cargo.toml --example memory_probe -- md_crdt 10000
//!   cargo run --release --manifest-path md-crdt-yrs-bench/Cargo.toml --example memory_probe -- yrs 10000
//!
//! Prefer wrapping with `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux).
//! Run each engine in a **separate process** — do not compare allocators in one process.

use md_crdt_yrs_bench::sizes::fill_text;
use md_crdt_yrs_bench::{MdCrdtAdapter, TextEngine, YrsAdapter};
use std::env;
use std::hint::black_box;
use std::io::{Write, stdin, stdout};
use std::thread;
use std::time::Duration;

fn hold_document<E: TextEngine>(engine: &str, n: usize, hold_ms: u64) {
    let seed = E::seed(1, &fill_text(n));
    let document = E::restore(&seed);
    drop(seed);

    println!(
        "engine={engine} n={n} visible_len={}",
        document.visible_len()
    );
    let _ = stdout().flush();
    // Keep the live document allocated while the external process records RSS.
    thread::sleep(Duration::from_millis(hold_ms));
    // Optional: wait for enter if HOLD_STDIN=1.
    if env::var("HOLD_STDIN").ok().as_deref() == Some("1") {
        let mut line = String::new();
        let _ = stdin().read_line(&mut line);
    }
    black_box(&document);
}

fn main() {
    let mut args = env::args().skip(1);
    let engine = args.next().expect("engine: md_crdt|yrs");
    let n: usize = args
        .next()
        .expect("n: text length")
        .parse()
        .expect("n parses");
    let hold_ms: u64 = args
        .next()
        .unwrap_or_else(|| "500".into())
        .parse()
        .expect("hold_ms");

    match engine.as_str() {
        "md_crdt" => hold_document::<MdCrdtAdapter>(&engine, n, hold_ms),
        "yrs" => hold_document::<YrsAdapter>(&engine, n, hold_ms),
        other => panic!("unknown engine {other}; use md_crdt or yrs"),
    }
}
