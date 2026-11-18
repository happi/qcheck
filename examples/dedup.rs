//! A property that is false, to show what a failure report looks like.
//!
//!   cargo run --example dedup

use qcheck::quickcheck;

/// Removes duplicates. The bug: it only compares neighbours.
fn dedup(mut v: Vec<u8>) -> Vec<u8> {
    v.dedup();
    v
}

fn main() {
    quickcheck(|v: Vec<u8>| {
        let out = dedup(v);
        let mut seen = out.clone();
        seen.sort();
        seen.dedup();
        seen.len() == out.len()
    });
}
