//! Runs a property against random inputs and shrinks the first failure.

use crate::arbitrary::Arbitrary;
use crate::generator::Generator;
use crate::gens;
use crate::rng::Gen;
use crate::tree::Tree;
use std::cell::Cell;
use std::fmt::{self, Debug};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

/// What one evaluation of a property says.
#[derive(Debug, Clone, PartialEq)]
pub enum TestResult {
    Pass,
    Fail(String),
    /// The input did not meet the property's precondition. It counts as
    /// neither a pass nor a failure.
    Discard,
}

/// Return types a property may have.
pub trait Testable {
    fn result(self) -> TestResult;
}

impl Testable for TestResult {
    fn result(self) -> TestResult {
        self
    }
}

impl Testable for bool {
    fn result(self) -> TestResult {
        if self {
            TestResult::Pass
        } else {
            TestResult::Fail("property returned false".into())
        }
    }
}

/// Lets a property use `assert!` and return nothing.
impl Testable for () {
    fn result(self) -> TestResult {
        TestResult::Pass
    }
}

impl<T: Testable, E: Debug> Testable for Result<T, E> {
    fn result(self) -> TestResult {
        match self {
            Ok(t) => t.result(),
            Err(e) => TestResult::Fail(format!("{e:?}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Failure<A> {
    /// The smallest failing input found.
    pub shrunk: A,
    /// The input that failed first, before shrinking.
    pub original: A,
    pub shrinks: u32,
    /// Tests that passed before the failure.
    pub passed: u32,
    pub seed: u64,
    pub reason: String,
}

impl<A: Debug> fmt::Display for Failure<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "property failed after {} passing tests", self.passed)?;
        writeln!(f, "  counterexample: {:?}", self.shrunk)?;
        if self.shrinks > 0 {
            writeln!(f, "  before {} shrinks: {:?}", self.shrinks, self.original)?;
        }
        writeln!(f, "  reason: {}", self.reason)?;
        write!(f, "  rerun with QC_SEED={}", self.seed)
    }
}

#[derive(Debug, Clone)]
pub enum Outcome<A> {
    Passed {
        tests: u32,
    },
    /// Too many inputs were discarded to reach the requested test count.
    GaveUp {
        tests: u32,
        discards: u32,
    },
    Failed(Failure<A>),
}

impl<A> Outcome<A> {
    pub fn is_pass(&self) -> bool {
        matches!(self, Outcome::Passed { .. })
    }

    pub fn failure(self) -> Option<Failure<A>> {
        match self {
            Outcome::Failed(f) => Some(f),
            _ => None,
        }
    }
}

/// Run configuration. `QC_SEED` and `QC_TESTS` in the environment set the
/// defaults, which is how a failure reported by `check` is replayed.
#[derive(Debug, Clone)]
pub struct QuickCheck {
    tests: u32,
    max_size: usize,
    max_discards: u32,
    max_shrinks: u32,
    seed: u64,
}

impl Default for QuickCheck {
    fn default() -> Self {
        Self::new()
    }
}

impl QuickCheck {
    pub fn new() -> Self {
        let tests = env_number("QC_TESTS").unwrap_or(100) as u32;
        QuickCheck {
            tests,
            max_size: 100,
            max_discards: tests.saturating_mul(10),
            max_shrinks: 10_000,
            seed: env_number("QC_SEED").unwrap_or_else(clock_seed),
        }
    }

    pub fn tests(mut self, tests: u32) -> Self {
        self.tests = tests;
        self.max_discards = tests.saturating_mul(10);
        self
    }

    pub fn max_size(mut self, max_size: usize) -> Self {
        self.max_size = max_size;
        self
    }

    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Runs the property on values of an `Arbitrary` type.
    pub fn run<A, R, F>(&self, prop: F) -> Outcome<A>
    where
        A: Arbitrary,
        R: Testable,
        F: Fn(A) -> R,
    {
        self.run_with(&gens::any::<A>(), prop)
    }

    /// Runs the property on values drawn from a generator.
    pub fn run_with<T, R, F>(&self, generator: &Generator<T>, prop: F) -> Outcome<T>
    where
        T: Clone + Debug + 'static,
        R: Testable,
        F: Fn(T) -> R,
    {
        let mut g = Gen::new(self.seed, 0);
        let (mut passed, mut discards) = (0u32, 0u32);

        while passed < self.tests {
            // Discards also grow the size, so a precondition that small
            // inputs never meet does not stall the run at size zero.
            let step = (passed + discards) as usize;
            g.set_size((step * (self.max_size + 1) / self.tests as usize).min(self.max_size));

            let tree = generator.tree(&mut g);
            match apply(&prop, tree.value().clone()) {
                TestResult::Pass => passed += 1,
                TestResult::Discard => {
                    discards += 1;
                    if discards > self.max_discards {
                        return Outcome::GaveUp {
                            tests: passed,
                            discards,
                        };
                    }
                }
                TestResult::Fail(reason) => {
                    let original = tree.value().clone();
                    let (shrunk, reason, shrinks) = self.shrink(&prop, tree, reason);
                    return Outcome::Failed(Failure {
                        shrunk,
                        original,
                        shrinks,
                        passed,
                        seed: self.seed,
                        reason,
                    });
                }
            }
        }
        Outcome::Passed { tests: passed }
    }

    /// Runs the property and panics with a report unless every test passes.
    /// This is the form to call from a `#[test]` function.
    #[track_caller]
    pub fn check<A, R, F>(&self, prop: F)
    where
        A: Arbitrary,
        R: Testable,
        F: Fn(A) -> R,
    {
        self.check_with(&gens::any::<A>(), prop)
    }

    /// `check` for values drawn from a generator.
    #[track_caller]
    pub fn check_with<T, R, F>(&self, generator: &Generator<T>, prop: F)
    where
        T: Clone + Debug + 'static,
        R: Testable,
        F: Fn(T) -> R,
    {
        match self.run_with(generator, prop) {
            Outcome::Passed { .. } => {}
            Outcome::GaveUp { tests, discards } => panic!(
                "gave up after {tests} tests and {discards} discards (seed {})",
                self.seed
            ),
            Outcome::Failed(failure) => panic!("{failure}"),
        }
    }

    /// Greedy descent through the shrink tree: move to the first child
    /// that still fails, and stop when none does.
    fn shrink<T, R, F>(&self, prop: &F, tree: Tree<T>, mut reason: String) -> (T, String, u32)
    where
        T: Clone + 'static,
        R: Testable,
        F: Fn(T) -> R,
    {
        let mut current = tree;
        let mut steps = 0;
        'descend: while steps < self.max_shrinks {
            for child in current.children() {
                if let TestResult::Fail(r) = apply(prop, child.value().clone()) {
                    current = child;
                    reason = r;
                    steps += 1;
                    continue 'descend;
                }
            }
            break;
        }
        (current.value().clone(), reason, steps)
    }
}

/// Checks a property on values of an `Arbitrary` type, with the default
/// configuration.
#[track_caller]
pub fn quickcheck<A, R, F>(prop: F)
where
    A: Arbitrary,
    R: Testable,
    F: Fn(A) -> R,
{
    QuickCheck::new().check(prop)
}

/// Checks a property on values drawn from a generator, with the default
/// configuration.
#[track_caller]
pub fn forall<T, R, F>(generator: Generator<T>, prop: F)
where
    T: Clone + Debug + 'static,
    R: Testable,
    F: Fn(T) -> R,
{
    QuickCheck::new().check_with(&generator, prop)
}

thread_local! {
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

static HOOK: Once = Once::new();

/// A panicking property is a failing property. Shrinking makes it panic
/// many times over, so the panic message is silenced on this thread while
/// the property runs. Panics on other threads print as usual.
fn apply<A, R, F>(prop: &F, input: A) -> TestResult
where
    R: Testable,
    F: Fn(A) -> R,
{
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !QUIET.with(Cell::get) {
                previous(info);
            }
        }));
    });

    QUIET.with(|q| q.set(true));
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| prop(input).result()));
    QUIET.with(|q| q.set(false));

    outcome.unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic with a non-string payload".into());
        TestResult::Fail(format!("panicked: {message}"))
    })
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn clock_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // One SplitMix64 step spreads the low clock bits over the whole word.
    Gen::new(nanos, 0).u64()
}
