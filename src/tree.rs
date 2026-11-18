//! Shrink trees: a generated value together with its simpler variants.

use crate::arbitrary::Shrinker;
use std::rc::Rc;

/// A value and, lazily, the trees of the values it can shrink to.
///
/// A generator returns a tree, so every way of building a generator also
/// says how its values shrink. Mapping a generator maps the tree, and
/// filtering a generator drops the branches that break the filter.
pub struct Tree<T> {
    value: T,
    children: Rc<dyn Fn() -> Shrinker<Tree<T>>>,
}

/// Lists the simpler variants of a value, simplest first.
pub type ShrinkFn<T> = dyn Fn(&T) -> Shrinker<T>;

impl<T: Clone> Clone for Tree<T> {
    fn clone(&self) -> Self {
        Tree {
            value: self.value.clone(),
            children: self.children.clone(),
        }
    }
}

impl<T: Clone + 'static> Tree<T> {
    pub fn new(value: T, children: impl Fn() -> Shrinker<Tree<T>> + 'static) -> Self {
        Tree {
            value,
            children: Rc::new(children),
        }
    }

    /// A value that does not shrink.
    pub fn leaf(value: T) -> Self {
        Tree::new(value, || Box::new(std::iter::empty()))
    }

    /// Grows a tree by applying a shrink function to each value in turn.
    pub fn unfold(value: T, shrink: Rc<ShrinkFn<T>>) -> Self {
        let root = value.clone();
        Tree::new(value, move || {
            let shrink = shrink.clone();
            Box::new(shrink(&root).map(move |v| Tree::unfold(v, shrink.clone())))
        })
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    /// The simpler variants, simplest first.
    pub fn children(&self) -> Shrinker<Tree<T>> {
        (self.children)()
    }

    pub fn map<U: Clone + 'static>(&self, f: Rc<dyn Fn(T) -> U>) -> Tree<U> {
        let this = self.clone();
        Tree::new(f(self.value.clone()), move || {
            let f = f.clone();
            Box::new(this.children().map(move |c| c.map(f.clone())))
        })
    }

    /// Keeps the values that satisfy `keep`. The root is assumed to.
    ///
    /// A rejected value is replaced by those of its own children that pass,
    /// so the search steps over one invalid value: an odd number can shrink
    /// past the even number between it and the next odd one. Two invalid
    /// values in a row end that branch, which bounds the work.
    pub fn filter(&self, keep: Rc<dyn Fn(&T) -> bool>) -> Tree<T> {
        let this = self.clone();
        Tree::new(self.value.clone(), move || {
            let keep = keep.clone();
            Box::new(this.children().flat_map(move |child| -> Shrinker<Tree<T>> {
                if keep(&child.value) {
                    return Box::new(std::iter::once(child.filter(keep.clone())));
                }
                let (test, keep) = (keep.clone(), keep.clone());
                Box::new(
                    child
                        .children()
                        .filter(move |c| test(&c.value))
                        .map(move |c| c.filter(keep.clone())),
                )
            }))
        })
    }
}

/// A pair shrinks its first component, then its second.
pub fn zip<A, B>(a: &Tree<A>, b: &Tree<B>) -> Tree<(A, B)>
where
    A: Clone + 'static,
    B: Clone + 'static,
{
    let (a, b) = (a.clone(), b.clone());
    Tree::new((a.value.clone(), b.value.clone()), move || {
        let (a1, b1) = (a.clone(), b.clone());
        let (a2, b2) = (a.clone(), b.clone());
        Box::new(
            a1.children()
                .map(move |ac| zip(&ac, &b1))
                .chain(b2.children().map(move |bc| zip(&a2, &bc))),
        )
    })
}

/// A list shrinks by dropping chunks, large ones first, down to `min_len`
/// elements, and then by shrinking one element at a time.
pub fn sequence<T: Clone + 'static>(items: Vec<Tree<T>>, min_len: usize) -> Tree<Vec<T>> {
    let value = items.iter().map(|t| t.value.clone()).collect();
    Tree::new(value, move || {
        let n = items.len();

        let mut cuts = Vec::new();
        let mut k = n.saturating_sub(min_len);
        while k > 0 {
            let mut start = 0;
            while start + k <= n {
                cuts.push((start, k));
                start += k;
            }
            k /= 2;
        }
        let whole = items.clone();
        let removals = cuts.into_iter().map(move |(start, k)| {
            let mut rest = whole.clone();
            rest.drain(start..start + k);
            sequence(rest, min_len)
        });

        let whole = items.clone();
        let elements = (0..n).flat_map(move |i| {
            let whole = whole.clone();
            whole[i].children().map(move |c| {
                let mut next = whole.clone();
                next[i] = c;
                sequence(next, min_len)
            })
        });

        Box::new(removals.chain(elements))
    })
}
