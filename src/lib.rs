//! A small QuickCheck: state a property, test it on random inputs, and get
//! the smallest failing input back.
//!
//! ```
//! use qcheck::quickcheck;
//!
//! quickcheck(|v: Vec<i32>| {
//!     let mut twice = v.clone();
//!     twice.reverse();
//!     twice.reverse();
//!     twice == v
//! });
//! ```
//!
//! A property takes one argument. Use a tuple for several inputs.
//!
//! `quickcheck` picks the generator from the argument's type. `forall`
//! takes a generator you build from the pieces in [`gens`].

mod arbitrary;
mod generator;
pub mod gens;
mod rng;
mod runner;
mod tree;

pub use arbitrary::{no_shrink, Arbitrary, Shrinker};
pub use generator::Generator;
pub use rng::Gen;
pub use runner::{forall, quickcheck, Failure, Outcome, QuickCheck, TestResult, Testable};
pub use tree::{ShrinkFn, Tree};
