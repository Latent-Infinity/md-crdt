//! Print md-crdt exclusive-span attribution for competitive-shaped workloads.
//!
//! Requires `--features sub_probes`. Not a competitive timing; never ratio vs Yrs.
//!
//! ```bash
//! cargo run --manifest-path md-crdt-yrs-bench/Cargo.toml --example sub_probes --features sub_probes --release
//! ```

use md_crdt_yrs_bench::sub_probes::{
    attribute_apply_remote_lag, attribute_insert_middle, format_attribution_table,
};

fn main() {
    let target = std::env::args().nth(1).unwrap_or_else(|| "all".to_string());
    if !matches!(target.as_str(), "all" | "insert" | "apply") {
        eprintln!("usage: sub_probes [all|insert|apply]");
        std::process::exit(2);
    }

    // Warm once so first-run noise is outside printed tables.
    if target != "apply" {
        let _ = attribute_insert_middle(256);
    }
    if target != "insert" {
        let _ = attribute_apply_remote_lag(256, 4);
    }

    println!("# md-crdt sub-probe attribution\n");
    println!("Exclusive spans via `perf_trace`. Wall is a separate timer around the same work.\n");

    if target != "apply" {
        let (snap, wall) = attribute_insert_middle(1_000);
        print!(
            "{}",
            format_attribution_table("insert_middle n=1_000", &snap, wall)
        );

        let (snap, wall) = attribute_insert_middle(10_000);
        print!(
            "{}",
            format_attribution_table("insert_middle n=10_000", &snap, wall)
        );
    }

    if target != "insert" {
        let (snap, wall) = attribute_apply_remote_lag(1_000, 100);
        print!(
            "{}",
            format_attribution_table("apply_remote full+lag n=1_000 k=100", &snap, wall)
        );

        let (snap, wall) = attribute_apply_remote_lag(10_000, 100);
        print!(
            "{}",
            format_attribution_table("apply_remote full+lag n=10_000 k=100", &snap, wall)
        );
    }
}
