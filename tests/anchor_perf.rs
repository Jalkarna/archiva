//! Anchor-extraction performance regression gate (audit blocker B4).
//!
//! The extractor had O(n²) hotspots (per-token prefix re-scans and a per-anchor
//! linear insert) that turned large or declaration-dense files into multi-second
//! hangs — and hooks read those as freezes. These tests assert extraction of a
//! large, dense single file stays well under a wall-clock budget, so a
//! reintroduced quadratic fails the build.
//!
//! Budgets are deliberately loose (seconds, not milliseconds) to stay robust on
//! slow/shared CI runners while still catching an O(n²) regression, which would
//! blow past them by orders of magnitude. Override with ARCHIVA_PERF_BUDGET_MS.

use std::time::{Duration, Instant};

use archiva::core::anchor::extract_anchors;
use archiva::core::paths::RelativePath;

fn budget() -> Duration {
    let ms: u64 = std::env::var("ARCHIVA_PERF_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4_000);
    Duration::from_millis(ms)
}

fn time_extraction(path: &str, source: &str) -> Duration {
    let rel = RelativePath::new(path).unwrap();
    let start = Instant::now();
    let extraction = extract_anchors(&rel, source);
    let elapsed = start.elapsed();
    // Sanity: it actually produced anchors (didn't early-out).
    assert!(
        !extraction.anchors.is_empty(),
        "expected anchors from {path}"
    );
    elapsed
}

#[test]
fn dense_rust_file_extraction_is_not_quadratic() {
    let n = 40_000;
    let mut source = String::with_capacity(n * 32);
    for i in 0..n {
        source.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
    }
    let elapsed = time_extraction("src/dense.rs", &source);
    assert!(
        elapsed < budget(),
        "dense Rust extraction took {elapsed:?}, over budget {:?} (possible O(n^2) regression)",
        budget()
    );
}

#[test]
fn dense_typescript_file_extraction_is_not_quadratic() {
    let n = 40_000;
    let mut source = String::with_capacity(n * 40);
    for i in 0..n {
        source.push_str(&format!("export function f{i}() {{ return {i}; }}\n"));
    }
    let elapsed = time_extraction("src/dense.ts", &source);
    assert!(
        elapsed < budget(),
        "dense TS extraction took {elapsed:?}, over budget {:?} (possible O(n^2) regression)",
        budget()
    );
}

#[test]
fn large_rust_file_with_methods_is_not_quadratic() {
    // Many impl methods exercise the impl/method membership checks.
    let n = 20_000;
    let mut source = String::from("struct S;\nimpl S {\n");
    for i in 0..n {
        source.push_str(&format!("    fn m{i}(&self) {{ let _ = {i}; }}\n"));
    }
    source.push_str("}\n");
    let elapsed = time_extraction("src/methods.rs", &source);
    assert!(
        elapsed < budget(),
        "large impl extraction took {elapsed:?}, over budget {:?} (possible O(n^2) regression)",
        budget()
    );
}
