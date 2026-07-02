//! Aggressive byte-level panic fuzzer for Archiva's hand-rolled parsers and
//! the anchor extractor. The goal is to enumerate the *entire* panic class on
//! committed/shared input (audit blocker B1), not to point-patch known cases.
//!
//! Run: `cargo run --release --example panic_fuzz [iterations] [seed]`
//!
//! It drives every parse entry point that can see attacker-/corruption-
//! controlled bytes under `catch_unwind` and prints any input that panics,
//! plus a byte-level minimization so the failing case is small enough to turn
//! into a regression test.

use std::panic::{self, AssertUnwindSafe};

use archiva::core::anchor::extract_anchors;
use archiva::core::decision::{parse_write_decision_input_json, why_for_line_from_dlog};
use archiva::core::dlog::parse_dlog_yaml;
use archiva::core::dmap::parse_dmap;
use archiva::core::fingerprint::fingerprint;
use archiva::core::json::parse as parse_json;
use archiva::core::paths::RelativePath;
use archiva::core::yaml::parse_yaml;

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }
    fn next_u64(&mut self) -> u64 {
        let mut v = self.state;
        v ^= v << 13;
        v ^= v >> 7;
        v ^= v << 17;
        self.state = v;
        v
    }
    fn usize(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() as usize) % n
        }
    }
}

/// Every parser that can receive committed or shared bytes. Each takes a string
/// (we feed it lossy-decoded arbitrary bytes) and returns nothing useful — we
/// only care whether it unwinds.
fn drive(target: usize, input: &str) {
    match target {
        0 => {
            let _ = parse_yaml(input);
        }
        1 => {
            let _ = parse_json(input);
        }
        2 => {
            let _ = parse_dlog_yaml(input);
        }
        3 => {
            let _ = parse_dmap(input);
        }
        4 => {
            let _ = parse_write_decision_input_json(input);
        }
        5 => {
            let _ = fingerprint(input);
        }
        6 => {
            for ext in ["ts", "tsx", "rs", "c", "cc", "cpp", "h", "hpp"] {
                let path = format!("src/fuzz.{ext}");
                if let Ok(rel) = RelativePath::new(&path) {
                    let _ = extract_anchors(&rel, input);
                }
            }
        }
        7 => {
            if let Ok(rel) = RelativePath::new("src/fuzz.ts") {
                let dlog = parse_dlog_yaml(input).ok();
                let _ = why_for_line_from_dlog(dlog.as_ref(), &rel, 1);
            }
        }
        _ => unreachable!(),
    }
}

const NUM_TARGETS: usize = 8;

/// Tokens that exercise the structural edge cases of the hand-rolled parsers.
const FRAGMENTS: &[&str] = &[
    "'",
    "''",
    "'''",
    "\"",
    "\"\"",
    "\\",
    "\\u",
    "\\U",
    "\\x",
    "\\u{",
    "{",
    "}",
    "[",
    "]",
    ":",
    ": ",
    ",",
    "- ",
    "-",
    "? ",
    "|",
    "|-",
    "|+",
    ">",
    ">-",
    "#",
    "# c\n",
    "\n",
    "\r\n",
    "\r",
    "\t",
    " ",
    "  ",
    "   ",
    "key:",
    "key: value",
    "k: '",
    "k: \"",
    "a: |",
    "b: >",
    "- - -",
    "!!str",
    "!!null",
    "&a",
    "*a",
    "<<:",
    "0x",
    "1e",
    "-",
    ".",
    "0.0.0",
    "true",
    "false",
    "null",
    "~",
    "\u{0085}",
    "\u{00a0}",
    "\u{2028}",
    "\u{feff}",
    "é",
    "🦀",
    "\u{0}",
    "\u{1}",
    "schema:",
    "decisions:",
    "id:",
    "fingerprint:",
    "lines_hint:",
    "chose:",
    "because:",
    "timestamp:",
    "history:",
    "rejected:",
    "file:",
    "fn:",
    "    ",
    "        ",
    "{}",
    "[]",
    "[{",
    "{[",
    "::",
    "''''",
    "\"\\\"",
    "0123456789012345678901234567890",
    "1.7976931348623157e309",
];

fn random_bytes(rng: &mut Rng, max: usize) -> Vec<u8> {
    let n = rng.usize(max) + 1;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        // Bias toward structural / whitespace / high bytes that trip slicing.
        let pick = rng.usize(10);
        let b = match pick {
            0..=3 => {
                let structural = b"'\"\\{}[]:,- \n\r\t#|>?~&*!.0123456789";
                structural[rng.usize(structural.len())]
            }
            4..=6 => rng.usize(256) as u8,
            _ => (0x80 + rng.usize(0x80)) as u8, // continuation / lead bytes
        };
        v.push(b);
    }
    v
}

fn random_fragmented(rng: &mut Rng, max_frags: usize) -> String {
    let n = rng.usize(max_frags) + 1;
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(FRAGMENTS[rng.usize(FRAGMENTS.len())]);
        if rng.usize(5) == 0 {
            // splice an arbitrary byte sequence (lossy) for UTF-8 boundary cases
            let bytes = random_bytes(rng, 4);
            s.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    s
}

fn deep_nest(rng: &mut Rng) -> String {
    // Provoke recursion: deeply nested flow collections, mappings, sequences.
    // Depth is bounded so each generated input stays O(depth) in size — 2000
    // levels is far above any real source nesting and well past the extractor's
    // 256-level bound, so it still exercises the depth guard, but keeps the
    // fuzzer fast (an earlier O(depth^2) generator produced ~200 MB strings).
    let depth = 200 + rng.usize(2_000);
    let kind = rng.usize(4);
    match kind {
        0 => "[".repeat(depth) + &"]".repeat(depth),
        1 => "{".repeat(depth),
        2 => {
            // Nested mappings with a fixed two-space indent per level (linear
            // in depth, unlike a per-level-growing indent).
            let mut s = String::new();
            for i in 0..depth {
                for _ in 0..(i % 8) {
                    s.push(' ');
                }
                s.push_str("a:\n");
            }
            s
        }
        _ => {
            // nested sequence "- - - ... x"
            let mut s = String::new();
            for _ in 0..depth {
                s.push_str("- ");
            }
            s.push('x');
            s
        }
    }
}

fn run_silent<F: FnOnce()>(f: F) -> bool {
    // Returns true if it panicked.
    panic::catch_unwind(AssertUnwindSafe(f)).is_err()
}

fn minimize(target: usize, input: &[u8]) -> Vec<u8> {
    let mut cur = input.to_vec();
    // Greedy byte-removal minimization.
    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0;
        while i < cur.len() {
            let mut cand = cur.clone();
            cand.remove(i);
            let s = String::from_utf8_lossy(&cand).into_owned();
            if run_silent(|| drive(target, &s)) {
                cur = cand;
                changed = true;
            } else {
                i += 1;
            }
        }
    }
    cur
}

fn main() {
    // Silence panic output during fuzzing; we report ourselves.
    panic::set_hook(Box::new(|_| {}));

    let args: Vec<String> = std::env::args().collect();
    let iters: u64 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);
    let seed: u64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED_1234);

    let mut rng = Rng::new(seed);
    let mut found: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut seen_min: std::collections::HashSet<(usize, Vec<u8>)> =
        std::collections::HashSet::new();

    for i in 0..iters {
        let target = rng.usize(NUM_TARGETS);
        let mode = rng.usize(10);
        let input: Vec<u8> = match mode {
            0..=3 => random_bytes(&mut rng, 64),
            4..=7 => random_fragmented(&mut rng, 12).into_bytes(),
            8 => deep_nest(&mut rng).into_bytes(),
            _ => {
                // mutate a known dlog-ish skeleton
                let mut base =
                    b"file: a.ts\nschema: 1\ndecisions:\n  fn:f:\n    id: dec_001\n    chose: x\n"
                        .to_vec();
                let cuts = rng.usize(6) + 1;
                for _ in 0..cuts {
                    if base.is_empty() {
                        break;
                    }
                    let pos = rng.usize(base.len());
                    let op = rng.usize(3);
                    match op {
                        0 => {
                            base[pos] = (rng.usize(256)) as u8;
                        }
                        1 => {
                            base.insert(pos, FRAGMENTS[rng.usize(FRAGMENTS.len())].as_bytes()[0]);
                        }
                        _ => {
                            base.remove(pos);
                        }
                    }
                }
                base
            }
        };

        let s = String::from_utf8_lossy(&input).into_owned();
        if run_silent(|| drive(target, &s)) {
            let min = minimize(target, &input);
            let key = (target, min.clone());
            if seen_min.insert(key.clone()) {
                found.push(key);
                eprintln!(
                    "PANIC target={target} after {i} iters: {:?}",
                    String::from_utf8_lossy(&min)
                );
            }
        }
    }

    eprintln!(
        "\n=== fuzz complete: {iters} iters, {} distinct panics ===",
        found.len()
    );
    if found.is_empty() {
        println!("OK: no panics found");
    } else {
        for (t, m) in &found {
            println!(
                "target={t} min_bytes={:?} as_str={:?}",
                m,
                String::from_utf8_lossy(m)
            );
        }
        std::process::exit(2);
    }
}
