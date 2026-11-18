use qcheck::{forall, gens, Gen, Generator, QuickCheck};
use std::fmt::Debug;

/// The shrunk counterexample of a property that is expected to fail.
fn shrunk<T: Clone + Debug + 'static>(
    seed: u64,
    generator: Generator<T>,
    prop: impl Fn(T) -> bool,
) -> T {
    QuickCheck::new()
        .seed(seed)
        .tests(1000)
        .run_with(&generator, prop)
        .failure()
        .expect("the property should fail")
        .shrunk
}

#[test]
fn range_stays_in_bounds() {
    forall(gens::range(-3i8, 7), |n| (-3..=7).contains(&n));
    forall(gens::range(u64::MAX - 1, u64::MAX), |n| n >= u64::MAX - 1);
}

#[test]
fn range_shrinks_to_the_bound_nearest_zero() {
    assert_eq!(shrunk(1, gens::range(10u32, 500), |_| false), 10);
    assert_eq!(shrunk(1, gens::range(-500i32, -10), |_| false), -10);
    assert_eq!(shrunk(1, gens::range(-500i32, 500), |_| false), 0);
    assert_eq!(shrunk(1, gens::range(10u32, 500), |n| n < 123), 123);
}

#[test]
fn map_shrinks_inside_its_image() {
    let even = gens::range(0u32, 1000).map(|n| n * 2);
    forall(even.clone(), |n| n % 2 == 0);
    assert_eq!(shrunk(2, even, |n| n < 41), 42);
}

#[test]
fn such_that_shrinks_inside_the_constraint() {
    let odd = gens::any::<i32>().such_that(|n| n % 2 != 0);
    forall(odd.clone(), |n| n % 2 != 0);
    assert_eq!(shrunk(3, odd, |n| n < 50), 51);
}

#[test]
#[should_panic(expected = "such_that")]
fn such_that_reports_an_unmeetable_constraint() {
    forall(gens::any::<u8>().such_that(|_| false), |_| true);
}

#[test]
fn and_then_makes_the_second_generator_depend_on_the_first() {
    // A length, then a list of exactly that many digits.
    let sized_list = gens::range(0usize, 8)
        .and_then(|n| gens::zip(gens::constant(n), gens::vec_len(gens::range(0u8, 9), n, n)));
    forall(sized_list.clone(), |(n, v)| v.len() == n);

    let (n, v) = shrunk(4, sized_list, |(_, v)| v.iter().sum::<u8>() < 12);
    assert_eq!(v.len(), n, "shrinking kept the dependency");
    assert_eq!(v.iter().sum::<u8>(), 12, "{v:?}");
}

#[test]
fn elements_and_one_of_shrink_toward_the_front() {
    let word = gens::elements(vec!["alpha", "beta", "gamma"]);
    assert_eq!(shrunk(5, word.clone(), |_| false), "alpha");
    assert_eq!(shrunk(5, word, |w| w != "gamma"), "gamma");

    let number = gens::one_of(vec![gens::range(0i32, 9), gens::range(100, 999)]);
    forall(number.clone(), |n| {
        (0..=9).contains(&n) || (100..=999).contains(&n)
    });
    assert_eq!(shrunk(5, number.clone(), |_| false), 0);
    assert_eq!(shrunk(5, number, |n| n < 10), 100);
}

#[test]
fn frequency_follows_the_weights() {
    let coin = gens::frequency(vec![(9, gens::constant('h')), (1, gens::constant('t'))]);
    let mut g = Gen::new(6, 10);
    let tails = (0..10_000).filter(|_| coin.sample(&mut g) == 't').count();
    assert!((700..1300).contains(&tails), "{tails} tails in 10000");

    let never = gens::frequency(vec![(0, gens::constant(1)), (1, gens::constant(2))]);
    forall(never, |n| n == 2);
}

#[test]
fn vec_len_keeps_its_minimum_while_shrinking() {
    let lists = gens::vec_len(gens::range(0u8, 200), 2, 6);
    forall(lists.clone(), |v| (2..=6).contains(&v.len()));
    assert_eq!(shrunk(7, lists.clone(), |_| false), vec![0, 0]);
    assert_eq!(
        shrunk(7, lists, |v| v.iter().all(|x| *x < 100)),
        vec![0, 100]
    );
}

#[test]
fn vec_shrinks_like_the_arbitrary_vec() {
    let lists = gens::vec(gens::range(0i32, 1000));
    assert_eq!(shrunk(8, lists, |v| v.iter().all(|x| *x < 10)), vec![10]);
}

#[test]
fn option_shrinks_to_none() {
    assert_eq!(
        shrunk(9, gens::option(gens::range(1u8, 9)), |_| false),
        None
    );
    assert_eq!(
        shrunk(9, gens::option(gens::range(1u8, 9)), |o| o.is_none()),
        Some(1)
    );
}

#[test]
fn zip3_shrinks_every_component() {
    let triple = gens::zip3(
        gens::range(0u8, 99),
        gens::any::<bool>(),
        gens::range(0i16, 99),
    );
    assert_eq!(
        shrunk(10, triple, |(a, b, c)| a < 3 || !b || c < 4),
        (3, true, 4)
    );
}

#[test]
fn no_shrink_reports_the_value_as_generated() {
    let failure = QuickCheck::new()
        .seed(11)
        .run_with(&gens::range(0u32, 1000).no_shrink(), |n| n < 5)
        .failure()
        .expect("values reach 5");
    assert_eq!(failure.shrinks, 0);
    assert_eq!(failure.shrunk, failure.original);
}

#[test]
fn sized_and_resize_control_the_size() {
    forall(gens::sized(gens::constant).resize(7), |size| size == 7);
    forall(gens::vec(gens::any::<u8>()).resize(3), |v| v.len() <= 3);
}

/// A recursive generator: nested lists, shallower as the size runs out.
#[derive(Clone, Debug, PartialEq)]
enum Nest {
    Leaf,
    Node(Vec<Nest>),
}

fn depth(n: &Nest) -> usize {
    match n {
        Nest::Leaf => 0,
        Nest::Node(kids) => 1 + kids.iter().map(depth).max().unwrap_or(0),
    }
}

fn nest(budget: usize) -> Generator<Nest> {
    if budget == 0 {
        return gens::constant(Nest::Leaf);
    }
    gens::one_of(vec![
        gens::constant(Nest::Leaf),
        gens::vec_len(gens::lazy(move || nest(budget / 2)), 0, 3).map(Nest::Node),
    ])
}

#[test]
fn recursive_generators_terminate_and_shrink() {
    let nests = gens::sized(nest);
    forall(nests.clone(), |n| depth(&n) <= 8);

    let deep = shrunk(12, nests, |n| depth(&n) < 2);
    assert_eq!(deep, Nest::Node(vec![Nest::Node(vec![])]));
}
