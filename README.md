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

## Generators

`quickcheck` picks its inputs from the argument's type. When the type says
too little, build a generator and hand it to `forall`. This follows
`?FORALL(X, Generator, Prop)` in QuviQ QuickCheck and PropEr.

`examples/orders.rs` prices an order with a discount from ten items up. An
order is a unit price between 1 and 500 kr, in öre, and a quantity:

```rust
let order = gens::zip(gens::range(100u64, 50_000), gens::range(1u64, 100));

forall(order, |(price, quantity)| {
    total(price, quantity) >= total(price, quantity - 1)
});
```

Captured output of `QC_SEED=1 cargo run --example orders`:

```
property failed after 47 passing tests
  counterexample: (100, 10)
  before 1 shrinks: (29887, 10)
  reason: property returned false
  rerun with QC_SEED=1
```

The price shrank to 100, the lower bound of its range, and went no further.
A generator carries its own shrinking, so a counterexample is always a value
the generator could have produced.

| qcheck | QuviQ / PropEr | Shrinks toward |
|---|---|---|
| `gens::any::<T>()` | a type as generator | the type's `Arbitrary::shrink` |
| `gens::constant(v)` | `return`, `exactly` | nothing |
| `gens::range(lo, hi)` | `choose`, `range` | zero, or the bound nearest zero |
| `gens::elements(vec)` | `elements` | the front of the list |
| `gens::one_of(vec)` | `oneof`, `union` | the first generator |
| `gens::frequency(vec)` | `frequency` | the first generator with a weight |
| `gens::vec(g)`, `gens::vec_len(g, min, max)` | `list`, `vector` | fewer, then simpler elements |
| `gens::option(g)` | `default` | `None` |
| `gens::zip(a, b)`, `gens::zip3` | a tuple of generators | each component in turn |
| `gens::sized(f)` | `?SIZED` | |
| `gens::lazy(f)` | `?LAZY` | |
| `g.map(f)` | `?LET` | the input of `f` |
| `g.and_then(f)` | `?LET` returning a generator | the first value, then the second |
| `g.such_that(p)` | `?SUCHTHAT` | values that satisfy `p` |
| `g.no_shrink()` | `noshrink` | nothing |
| `g.resize(n)` | `resize` | |

`such_that` panics after 50 rejected draws in a row, PropEr's default for
`constraint_tries`. When the constraint is that strict, construct valid
values with `map` instead.

A recursive generator calls itself through `gens::lazy` and spends a budget
from `gens::sized`, so it ends. `tests/generators.rs` has one for nested
lists.

## Replaying and tuning

Each run picks a fresh seed and prints it on failure. `QC_SEED` replays a
run and `QC_TESTS` changes the test count (default 100). The same settings
are available in code:

```rust
let outcome = QuickCheck::new().seed(42).tests(1000).run(|x: u32| x < 500);
```

`run` returns an `Outcome` to inspect. `check` panics with the report, which
is the form a `#[test]` wants. `run_with` and `check_with` take a generator.

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
`Vec<T>`, `Option<T>`, and tuples of two or three.

## Not covered yet

Compared with QuviQ QuickCheck and PropEr: test case statistics (`collect`,
`classify`, `measure`), state machine models, and timeouts. Compared with
the Rust `quickcheck` crate: floats, `Result`, the standard collections
beyond `Vec`, properties with several arguments, and a `#[quickcheck]`
attribute.

## Running the tests

```
cargo test
scripts/dtest.sh        # same, in a Rust container, for hosts without cargo
```

## License

Licensed under either the MIT license (`LICENSE-MIT`) or the Apache License,
Version 2.0 (`LICENSE-APACHE`), at your option.
