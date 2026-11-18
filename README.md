# qcheck

A small QuickCheck in Rust. You state a property, qcheck tests it on random
inputs, and a failure comes back as the smallest input it can find. The
library has no dependencies.

## A property

A property is a function from one generated value to `bool`. Use a tuple
when it needs several inputs.

```rust
use qcheck::quickcheck;

#[test]
fn reverse_twice_is_identity() {
    quickcheck(|v: Vec<i32>| {
        let mut w = v.clone();
        w.reverse();
        w.reverse();
        w == v
    });
}
```

A property may also return `()` and use `assert!`, return a `Result`, or
return `TestResult` to discard inputs that miss a precondition.

## A failure

`examples/dedup.rs` tests a `dedup` that only compares neighbours. This is
the captured output of `QC_SEED=1 cargo run --example dedup`:

```
property failed after 8 passing tests
  counterexample: [2, 0, 2]
  before 3 shrinks: [2, 7, 2, 228, 2, 83, 3]
  reason: property returned false
  rerun with QC_SEED=1
```

The seven-element list is what the generator produced. The three-element
list is what shrinking left: two equal values with something between them,
which is the bug.

Shrinking is greedy. It moves to the first simpler input that still fails
and stops when no single step does. `[1, 0, 1]` is simpler than `[2, 0, 2]`,
but reaching it means changing two elements at once, so qcheck stops here.

## Replaying and tuning

Each run picks a fresh seed and prints it on failure. `QC_SEED` replays a
run and `QC_TESTS` changes the test count (default 100). The same settings
are available in code:

```rust
let outcome = QuickCheck::new().seed(42).tests(1000).run(|x: u32| x < 500);
```

`run` returns an `Outcome` to inspect. `check` panics with the report, which
is the form a `#[test]` wants.

## Your own types

Implement `Arbitrary`. `arbitrary` builds a value from the `Gen`, and
`shrink` lists strictly simpler candidates, simplest first. `shrink` is
optional; without it failures are reported as generated.

```rust
impl Arbitrary for Even {
    fn arbitrary(g: &mut Gen) -> Self {
        Even(g.below(g.size() as u64 + 1) as u32 * 2)
    }

    fn shrink(&self) -> Shrinker<Self> {
        Box::new((self.0 / 2).shrink().map(|half| Even(half * 2)))
    }
}
```

Built in: `bool`, the integer types up to 64 bits, `char`, `String`,
`Vec<T>`, `Option<T>`, and tuples of two or three. Floats, maps, and
functions are not covered yet.

## Running the tests

```
cargo test
scripts/dtest.sh        # same, in a Rust container, for hosts without cargo
```

## License

Licensed under either the MIT license (`LICENSE-MIT`) or the Apache License,
Version 2.0 (`LICENSE-APACHE`), at your option.
