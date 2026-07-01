//! Panic-safety CI gate (audit blocker B1).
//!
//! Archiva must never panic or abort on committed/shared input: one malformed
//! byte in a `.dlog` or a source file must not take down `status`/`lint`/
//! `session-start` or the long-lived MCP server. These tests exercise every
//! parser and the anchor extractor over adversarial bytes and pathological
//! nesting, and fail the build if any input panics (a caught unwind) or aborts
//! (the test binary crashes, which fails the job).

use std::panic::{self, AssertUnwindSafe};

use archiva::core::anchor::extract_anchors;
use archiva::core::dlog::parse_dlog_yaml;
use archiva::core::dmap::parse_dmap;
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

const FRAGMENTS: &[&str] = &[
    "'",
    "''",
    "'''",
    "\"",
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
    ">",
    ">-",
    "#",
    "\n",
    "\r\n",
    "\r",
    "\t",
    " ",
    "  ",
    "key:",
    "k: '",
    "k: \"",
    "a: |",
    "b: >",
    "!!str",
    "&a",
    "*a",
    "<<:",
    "0x",
    "1e",
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
    "schema:",
    "decisions:",
    "id:",
    "fingerprint:",
    "lines_hint:",
    "chose:",
    "because:",
    "timestamp:",
    "{}",
    "[]",
    "::",
    "\"\\\"",
    "1.7976931348623157e309",
];

fn random_bytes(rng: &mut Rng, max: usize) -> Vec<u8> {
    let n = rng.usize(max) + 1;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let b = match rng.usize(10) {
            0..=3 => {
                let s = b"'\"\\{}[]:,- \n\r\t#|>?~&*!.0123456789";
                s[rng.usize(s.len())]
            }
            4..=6 => rng.usize(256) as u8,
            _ => (0x80 + rng.usize(0x80)) as u8,
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
            s.push_str(&String::from_utf8_lossy(&random_bytes(rng, 4)));
        }
    }
    s
}

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
        _ => {
            for ext in ["ts", "tsx", "rs", "c", "cc", "cpp", "h", "hpp"] {
                if let Ok(rel) = RelativePath::new(&format!("src/fuzz.{ext}")) {
                    let _ = extract_anchors(&rel, input);
                }
            }
        }
    }
}

const NUM_TARGETS: usize = 5;

#[test]
fn parsers_and_extractor_never_panic_on_arbitrary_bytes() {
    // Bounded soak so it stays a fast PR gate; the standalone `panic_fuzz`
    // example runs millions of iterations for deeper campaigns.
    let iters: u64 = std::env::var("ARCHIVA_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);

    let prior = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));

    let mut rng = Rng::new(0x5EED_1234);
    let mut failures: Vec<(usize, String)> = Vec::new();
    for _ in 0..iters {
        let target = rng.usize(NUM_TARGETS);
        let input = if rng.usize(2) == 0 {
            String::from_utf8_lossy(&random_bytes(&mut rng, 64)).into_owned()
        } else {
            random_fragmented(&mut rng, 12)
        };
        if panic::catch_unwind(AssertUnwindSafe(|| drive(target, &input))).is_err() {
            failures.push((target, input));
            if failures.len() >= 8 {
                break;
            }
        }
    }

    panic::set_hook(prior);
    assert!(
        failures.is_empty(),
        "parser/extractor panicked on committed-style input: {:?}",
        failures
            .iter()
            .map(|(t, s)| (t, s.chars().take(60).collect::<String>()))
            .collect::<Vec<_>>()
    );
}

/// Specific B1 regressions the audit named, pinned so they can never regress.
#[test]
fn named_b1_parser_regressions_do_not_panic() {
    let prior = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));

    let cases = [
        "chose: '\n",                      // lone single quote (was yaml.rs:700)
        "k: |\n      a\n     \u{e9}xxx\n", // mid-codepoint block-scalar slice
        "k: |\n",                          // empty literal block scalar
        "k: >\n",                          // empty folded block scalar
        "k: |",                            // block scalar at EOF, no content
    ];
    let mut panicked = Vec::new();
    for case in cases {
        if panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = parse_yaml(case);
            let _ = parse_dlog_yaml(case);
        }))
        .is_err()
        {
            panicked.push(case);
        }
    }

    panic::set_hook(prior);
    assert!(
        panicked.is_empty(),
        "these inputs still panic: {panicked:?}"
    );
}

/// Deeply nested source must not overflow the stack and abort. Runs on a small
/// (512 KiB) stack so a regressed or missing depth bound crashes the test
/// binary here (failing the job) rather than in production. Extraction must
/// return and mark the result incomplete.
#[test]
fn deep_nesting_does_not_overflow_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let rel = RelativePath::new("src/deep.rs").unwrap();
            let n = 100_000;
            let mut deep = String::with_capacity(2 * n + 16);
            deep.push_str("fn f() {");
            for _ in 0..n {
                deep.push('{');
            }
            for _ in 0..n {
                deep.push('}');
            }
            deep.push('}');
            let extraction = extract_anchors(&rel, &deep);
            // The depth bound stops descent and reports incompleteness rather
            // than silently dropping anchors.
            assert!(!extraction.complete);
        })
        .expect("spawn small-stack extraction thread");

    handle
        .join()
        .expect("deep nesting overflowed the stack (depth bound regressed)");
}

#[test]
fn deep_template_literal_nesting_does_not_overflow_stack() {
    // Nested `${ `${ … }` }` template interpolation recurses once per level in
    // the JS/TS tokenizer and if-block/complexity scanners. Without a depth
    // bound this overflows the stack — an uncatchable abort that kills
    // `status`/`lint` and the long-lived MCP server on a single committed file.
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let rel = RelativePath::new("src/deep.ts").unwrap();
            let n = 100_000;
            let mut deep = String::with_capacity(6 * n + 16);
            deep.push_str("const x = ");
            for _ in 0..n {
                deep.push_str("`${");
            }
            deep.push('1');
            for _ in 0..n {
                deep.push_str("}`");
            }
            deep.push(';');
            // Must return rather than abort; the result is conservative.
            let _ = extract_anchors(&rel, &deep);
        })
        .expect("spawn small-stack template-literal extraction thread");

    handle
        .join()
        .expect("deep template nesting overflowed the stack (depth bound regressed)");
}

#[test]
fn deep_else_if_chain_does_not_overflow_stack() {
    // `if (…) {} else if (…) {} else if …` recurses once per link through
    // `if_statement_end`/`statement_end`. Without a depth bound a long chain in
    // a committed `.ts`/`.c`/`.cpp` file overflows the stack (uncatchable abort).
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let rel = RelativePath::new("src/chain.ts").unwrap();
            let n = 200_000;
            let mut chain = String::with_capacity(16 * n + 32);
            chain.push_str("function f(){\n");
            for _ in 0..n {
                chain.push_str("if(a&&b){} else ");
            }
            chain.push_str("{}\n}\n");
            let _ = extract_anchors(&rel, &chain);
        })
        .expect("spawn small-stack else-if extraction thread");

    handle
        .join()
        .expect("deep else-if chain overflowed the stack (depth bound regressed)");
}

#[test]
fn deep_destructuring_binding_does_not_overflow_stack() {
    // `const {a:{a:{a: … x}}} = y;` recurses once per nesting level through the
    // destructuring binding-pattern collectors. Without a depth bound a deeply
    // nested pattern in a committed `.ts`/`.js` file overflows the stack — an
    // uncatchable abort reachable from the MCP/lint/status extractor path.
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let rel = RelativePath::new("src/destructure.ts").unwrap();
            let n = 100_000;
            let mut source = String::with_capacity(4 * n + 32);
            source.push_str("const ");
            for _ in 0..n {
                source.push_str("{a:");
            }
            source.push('x');
            for _ in 0..n {
                source.push('}');
            }
            source.push_str(" = y;\n");
            let _ = extract_anchors(&rel, &source);
        })
        .expect("spawn small-stack destructuring extraction thread");

    handle
        .join()
        .expect("deep destructuring overflowed the stack (depth bound regressed)");
}
