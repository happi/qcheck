//! Types that can be generated at random and shrunk toward simpler values.

use crate::rng::Gen;
use std::fmt::Debug;

pub type Shrinker<T> = Box<dyn Iterator<Item = T>>;

pub fn no_shrink<T: 'static>() -> Shrinker<T> {
    Box::new(std::iter::empty())
}

pub trait Arbitrary: Clone + Debug + 'static {
    fn arbitrary(g: &mut Gen) -> Self;

    /// Candidates that are simpler than `self`, simplest first.
    ///
    /// Every candidate must be strictly simpler, otherwise shrinking can loop.
    fn shrink(&self) -> Shrinker<Self> {
        no_shrink()
    }
}

impl Arbitrary for () {
    fn arbitrary(_: &mut Gen) -> Self {}
}

impl Arbitrary for bool {
    fn arbitrary(g: &mut Gen) -> Self {
        g.bool()
    }

    fn shrink(&self) -> Shrinker<Self> {
        if *self {
            Box::new(std::iter::once(false))
        } else {
            no_shrink()
        }
    }
}

macro_rules! arbitrary_int {
    ($($t:ty),*) => {$(
        impl Arbitrary for $t {
            fn arbitrary(g: &mut Gen) -> Self {
                let (min, max) = (<$t>::MIN as i128, <$t>::MAX as i128);
                match g.below(10) {
                    // Boundary values find more bugs than their share of
                    // the range suggests.
                    0 => g.choose(&[0, 1, <$t>::MIN, <$t>::MAX]),
                    1 => g.int_in(min, max) as $t,
                    _ => {
                        let s = g.size() as i128;
                        g.int_in(min.max(-s), max.min(s)) as $t
                    }
                }
            }

            fn shrink(&self) -> Shrinker<Self> {
                let max = <$t>::MAX as i128;
                Box::new(
                    shrink_toward_zero(*self as i128)
                        .into_iter()
                        .filter(move |c| *c <= max)
                        .map(|c| c as $t),
                )
            }
        }
    )*};
}

/// Integer shrink candidates, simplest first: the positive twin of a
/// negative number, zero, then x - x/2, x - x/4, ... which closes in on x
/// from the zero side.
pub(crate) fn shrink_toward_zero(x: i128) -> Vec<i128> {
    let mut out = Vec::new();
    if x == 0 {
        return out;
    }
    if x < 0 {
        out.push(-x);
    }
    out.push(0);
    let mut d = x / 2;
    while d != 0 {
        out.push(x - d);
        d /= 2;
    }
    out
}

arbitrary_int!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

/// Characters a failing case is easier to read with, simplest first.
const SIMPLE_CHARS: [char; 8] = ['a', 'b', 'c', 'A', 'B', 'C', '0', ' '];

impl Arbitrary for char {
    fn arbitrary(g: &mut Gen) -> Self {
        match g.below(10) {
            0 => loop {
                if let Some(c) = char::from_u32(g.int_in(0, 0x10FFFF) as u32) {
                    return c;
                }
            },
            1 => g.choose(&['\0', '\t', '\n', '\r', '\u{7f}', 'é', 'ß', '\u{1F600}']),
            _ => g.int_in(0x20, 0x7E) as u8 as char,
        }
    }

    fn shrink(&self) -> Shrinker<Self> {
        let rank = SIMPLE_CHARS
            .iter()
            .position(|c| c == self)
            .unwrap_or(SIMPLE_CHARS.len());
        Box::new(SIMPLE_CHARS[..rank].iter().copied())
    }
}

impl<T: Arbitrary> Arbitrary for Option<T> {
    fn arbitrary(g: &mut Gen) -> Self {
        if g.below(4) == 0 {
            None
        } else {
            Some(T::arbitrary(g))
        }
    }

    fn shrink(&self) -> Shrinker<Self> {
        match self {
            None => no_shrink(),
            Some(x) => Box::new(std::iter::once(None).chain(x.shrink().map(Some))),
        }
    }
}

impl<T: Arbitrary> Arbitrary for Vec<T> {
    fn arbitrary(g: &mut Gen) -> Self {
        let len = g.below(g.size() as u64 + 1);
        (0..len).map(|_| T::arbitrary(g)).collect()
    }

    fn shrink(&self) -> Shrinker<Self> {
        let n = self.len();

        // Drop chunks first, large ones before small ones. Removing elements
        // shortens the case faster than simplifying them does.
        let mut removals = Vec::new();
        let mut k = n;
        while k > 0 {
            let mut start = 0;
            while start + k <= n {
                let mut v = self.clone();
                v.drain(start..start + k);
                removals.push(v);
                start += k;
            }
            k /= 2;
        }

        let original = self.clone();
        let elements = (0..n).flat_map(move |i| {
            let v = original.clone();
            let candidates = v[i].shrink();
            candidates.map(move |x| {
                let mut w = v.clone();
                w[i] = x;
                w
            })
        });

        Box::new(removals.into_iter().chain(elements))
    }
}

impl Arbitrary for String {
    fn arbitrary(g: &mut Gen) -> Self {
        Vec::<char>::arbitrary(g).into_iter().collect()
    }

    fn shrink(&self) -> Shrinker<Self> {
        let chars: Vec<char> = self.chars().collect();
        Box::new(chars.shrink().map(|v| v.into_iter().collect()))
    }
}

impl<A: Arbitrary, B: Arbitrary> Arbitrary for (A, B) {
    fn arbitrary(g: &mut Gen) -> Self {
        (A::arbitrary(g), B::arbitrary(g))
    }

    fn shrink(&self) -> Shrinker<Self> {
        let (a, b) = self.clone();
        let (a2, b2) = self.clone();
        Box::new(
            a.shrink()
                .map(move |x| (x, b.clone()))
                .chain(b2.shrink().map(move |y| (a2.clone(), y))),
        )
    }
}

impl<A: Arbitrary, B: Arbitrary, C: Arbitrary> Arbitrary for (A, B, C) {
    fn arbitrary(g: &mut Gen) -> Self {
        (A::arbitrary(g), B::arbitrary(g), C::arbitrary(g))
    }

    fn shrink(&self) -> Shrinker<Self> {
        // Reuse the pair logic by nesting.
        let nested = (self.0.clone(), (self.1.clone(), self.2.clone()));
        Box::new(nested.shrink().map(|(a, (b, c))| (a, b, c)))
    }
}
