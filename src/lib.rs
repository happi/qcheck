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

mod arbitrary;
mod gen;
mod runner;

pub use arbitrary::{no_shrink, Arbitrary, Shrinker};
pub use gen::Gen;
pub use runner::{quickcheck, Failure, Outcome, QuickCheck, TestResult, Testable};
