//! The building blocks for generators.
//!
//! ```
//! use qcheck::{forall, gens};
//!
//! let digits = gens::vec(gens::range(0u8, 9));
//! forall(digits, |v| v.iter().all(|d| *d <= 9));
//! ```

use crate::arbitrary::{shrink_toward_zero, Arbitrary, Shrinker};
use crate::generator::Generator;
use crate::tree::{self, Tree};
use std::fmt::Debug;
use std::rc::Rc;

/// The default generator of a type, with the type's own shrinking.
pub fn any<T: Arbitrary>() -> Generator<T> {
    Generator::new(|g| Tree::unfold(T::arbitrary(g), Rc::new(|v: &T| v.shrink())))
}

/// Always the same value.
pub fn constant<T: Clone + 'static>(value: T) -> Generator<T> {
    Generator::new(move |_| Tree::leaf(value.clone()))
}

/// Integer types `range` works on.
pub trait Int: Copy + Debug + 'static {
    fn to_i128(self) -> i128;
    fn from_i128(n: i128) -> Self;
}

macro_rules! int {
    ($($t:ty),*) => {$(
        impl Int for $t {
            fn to_i128(self) -> i128 {
                self as i128
            }
            fn from_i128(n: i128) -> Self {
                n as $t
            }
        }
    )*};
}

int!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

/// An integer in `lo..=hi`, uniformly. Shrinks toward zero when zero is in
/// the range, otherwise toward the bound nearest zero.
pub fn range<T: Int>(lo: T, hi: T) -> Generator<T> {
    let (lo, hi) = (lo.to_i128(), hi.to_i128());
    assert!(lo <= hi, "range: {lo} is above {hi}");
    let origin = 0.clamp(lo, hi);
    Generator::new(move |g| {
        let shrink = move |v: &i128| -> Shrinker<i128> {
            Box::new(
                shrink_toward_zero(*v - origin)
                    .into_iter()
                    .map(move |d| origin + d)
                    .filter(move |c| (lo..=hi).contains(c)),
            )
        };
        Tree::unfold(g.int_in(lo, hi), Rc::new(shrink))
    })
    .map(T::from_i128)
}

/// One of the given values. Shrinks toward the front of the list, so put
/// the simplest value first.
pub fn elements<T: Clone + 'static>(items: Vec<T>) -> Generator<T> {
    assert!(!items.is_empty(), "elements: nothing to choose from");
    range(0, items.len() - 1).map(move |i| items[i].clone())
}

/// A value from one of the given generators, each equally likely. Shrinks
/// toward the front of the list.
pub fn one_of<T: Clone + 'static>(choices: Vec<Generator<T>>) -> Generator<T> {
    assert!(!choices.is_empty(), "one_of: nothing to choose from");
    range(0, choices.len() - 1).and_then(move |i| choices[i].clone())
}

/// Like `one_of`, with a weight per generator. Shrinking ignores the
/// weights and moves toward the first generator with a non-zero weight.
pub fn frequency<T: Clone + 'static>(choices: Vec<(u32, Generator<T>)>) -> Generator<T> {
    let total: u64 = choices.iter().map(|(w, _)| *w as u64).sum();
    assert!(total > 0, "frequency: the weights sum to zero");
    range(0, total - 1).and_then(move |mut ticket| {
        for (weight, choice) in &choices {
            if ticket < *weight as u64 {
                return choice.clone();
            }
            ticket -= *weight as u64;
        }
        unreachable!("ticket is below the total weight")
    })
}

/// A list whose length is at most the current size.
pub fn vec<T: Clone + 'static>(element: Generator<T>) -> Generator<Vec<T>> {
    Generator::new(move |g| {
        let len = g.below(g.size() as u64 + 1);
        let items = (0..len).map(|_| element.tree(g)).collect();
        tree::sequence(items, 0)
    })
}

/// A list of `min..=max` elements. Shrinking keeps at least `min`.
pub fn vec_len<T: Clone + 'static>(
    element: Generator<T>,
    min: usize,
    max: usize,
) -> Generator<Vec<T>> {
    assert!(min <= max, "vec_len: {min} is above {max}");
    Generator::new(move |g| {
        let len = g.int_in(min as i128, max as i128);
        let items = (0..len).map(|_| element.tree(g)).collect();
        tree::sequence(items, min)
    })
}

/// `None` one time in four. Shrinks to `None`.
pub fn option<T: Clone + 'static>(element: Generator<T>) -> Generator<Option<T>> {
    frequency(vec![(1, constant(None)), (3, element.map(Some))])
}

/// A pair. Shrinks the first component, then the second.
pub fn zip<A, B>(a: Generator<A>, b: Generator<B>) -> Generator<(A, B)>
where
    A: Clone + 'static,
    B: Clone + 'static,
{
    Generator::new(move |g| {
        let first = a.tree(g);
        let second = b.tree(g);
        tree::zip(&first, &second)
    })
}

/// A triple.
pub fn zip3<A, B, C>(a: Generator<A>, b: Generator<B>, c: Generator<C>) -> Generator<(A, B, C)>
where
    A: Clone + 'static,
    B: Clone + 'static,
    C: Clone + 'static,
{
    zip(a, zip(b, c)).map(|(a, (b, c))| (a, b, c))
}

/// A generator that depends on the current size (`?SIZED`).
pub fn sized<T: Clone + 'static>(f: impl Fn(usize) -> Generator<T> + 'static) -> Generator<T> {
    Generator::new(move |g| f(g.size()).tree(g))
}

/// Builds the generator when a value is needed (`?LAZY`). A recursive
/// generator needs this at the point where it refers to itself.
pub fn lazy<T: Clone + 'static>(f: impl Fn() -> Generator<T> + 'static) -> Generator<T> {
    Generator::new(move |g| f().tree(g))
}
