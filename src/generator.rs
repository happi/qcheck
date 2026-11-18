//! Generators as values.

use crate::rng::Gen;
use crate::tree::Tree;
use std::rc::Rc;

/// How many values `such_that` draws before it gives up, as in PropEr's
/// `constraint_tries`.
const CONSTRAINT_TRIES: usize = 50;

/// A recipe for random values of type `T` that also knows how they shrink.
///
/// Build one from the functions in [`crate::gens`] and the methods here.
/// Cloning is cheap; a generator is a shared closure.
pub struct Generator<T> {
    run: Rc<Run<T>>,
}

type Run<T> = dyn Fn(&mut Gen) -> Tree<T>;

impl<T> Clone for Generator<T> {
    fn clone(&self) -> Self {
        Generator {
            run: self.run.clone(),
        }
    }
}

impl<T: Clone + 'static> Generator<T> {
    /// A generator from a function that builds the value's shrink tree.
    pub fn new(run: impl Fn(&mut Gen) -> Tree<T> + 'static) -> Self {
        Generator { run: Rc::new(run) }
    }

    /// One value with its shrink tree.
    pub fn tree(&self, g: &mut Gen) -> Tree<T> {
        (self.run)(g)
    }

    /// One value.
    pub fn sample(&self, g: &mut Gen) -> T {
        self.tree(g).value().clone()
    }

    /// Transforms each value (`?LET` with a plain result). Shrinking
    /// happens on the input, so every shrunk value is still an `f(x)`.
    pub fn map<U: Clone + 'static>(self, f: impl Fn(T) -> U + 'static) -> Generator<U> {
        let f: Rc<dyn Fn(T) -> U> = Rc::new(f);
        Generator::new(move |g| self.tree(g).map(f.clone()))
    }

    /// Picks the next generator from a generated value (`?LET` with a
    /// generator result). Shrinks the first value, rebuilding the second
    /// from the same random stream, and then shrinks the second.
    pub fn and_then<U: Clone + 'static>(
        self,
        f: impl Fn(T) -> Generator<U> + 'static,
    ) -> Generator<U> {
        let f: Rc<dyn Fn(T) -> Generator<U>> = Rc::new(f);
        Generator::new(move |g| {
            let outer = self.tree(g);
            bind(&outer, f.clone(), g.u64(), g.size())
        })
    }

    /// Keeps the values that satisfy `keep` (`?SUCHTHAT`). Shrinking stays
    /// inside the constraint.
    ///
    /// Panics when 50 draws in a row are rejected. A constraint that strict
    /// is better written with `map`.
    pub fn such_that(self, keep: impl Fn(&T) -> bool + 'static) -> Self {
        let keep: Rc<dyn Fn(&T) -> bool> = Rc::new(keep);
        Generator::new(move |g| {
            let size = g.size();
            for attempt in 0..CONSTRAINT_TRIES {
                // Small sizes may hold no valid value at all, so each
                // rejection widens the search.
                g.set_size(size + attempt);
                let tree = self.tree(g);
                if keep(tree.value()) {
                    g.set_size(size);
                    return tree.filter(keep.clone());
                }
            }
            panic!("such_that: {CONSTRAINT_TRIES} values in a row failed the constraint");
        })
    }

    /// Values from this generator are reported as generated.
    pub fn no_shrink(self) -> Self {
        Generator::new(move |g| Tree::leaf(self.sample(g)))
    }

    /// Runs this generator at a fixed size, whatever the test run is at.
    pub fn resize(self, size: usize) -> Self {
        Generator::new(move |g| {
            let outer = g.size();
            g.set_size(size);
            let tree = self.tree(g);
            g.set_size(outer);
            tree
        })
    }
}

fn bind<T, U>(outer: &Tree<T>, f: Rc<dyn Fn(T) -> Generator<U>>, seed: u64, size: usize) -> Tree<U>
where
    T: Clone + 'static,
    U: Clone + 'static,
{
    let inner = f(outer.value().clone()).tree(&mut Gen::new(seed, size));
    let outer = outer.clone();
    Tree::new(inner.value().clone(), move || {
        let f = f.clone();
        Box::new(
            outer
                .children()
                .map(move |o| bind(&o, f.clone(), seed, size))
                .chain(inner.children()),
        )
    })
}
