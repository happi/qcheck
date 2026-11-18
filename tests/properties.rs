use qcheck::{quickcheck, Arbitrary, Gen, Outcome, QuickCheck, Shrinker, TestResult};

#[test]
fn reverse_twice_is_identity() {
    quickcheck(|v: Vec<i32>| {
        let mut w = v.clone();
        w.reverse();
        w.reverse();
        w == v
    });
}

#[test]
fn addition_commutes() {
    quickcheck(|(a, b): (i32, i32)| a.wrapping_add(b) == b.wrapping_add(a));
}

#[test]
fn string_round_trips_through_chars() {
    quickcheck(|s: String| s.chars().collect::<String>() == s);
}

#[test]
fn false_property_shrinks_to_the_boundary() {
    let failure = QuickCheck::new()
        .seed(1)
        .tests(1000)
        .run(|v: Vec<i32>| v.iter().all(|x| *x < 10))
        .failure()
        .expect("some list holds an element of 10 or more");
    assert_eq!(failure.shrunk, vec![10]);
}

#[test]
fn shrinking_works_across_tuple_fields() {
    let failure = QuickCheck::new()
        .seed(2)
        .tests(1000)
        .run(|(a, b): (u32, u32)| a < 5 || b < 7)
        .failure()
        .expect("both fields get large");
    assert_eq!(failure.shrunk, (5, 7));
}

#[test]
fn a_panic_is_a_failure_and_still_shrinks() {
    let failure = QuickCheck::new()
        .seed(3)
        .tests(1000)
        .run(|s: String| assert!(s.chars().count() < 3, "too long"))
        .failure()
        .expect("some string has three characters");
    assert_eq!(failure.shrunk, "aaa");
    assert!(failure.reason.contains("too long"), "{}", failure.reason);
}

#[test]
fn same_seed_gives_the_same_failure() {
    let run = || {
        QuickCheck::new()
            .seed(42)
            .run(|v: Vec<u8>| v.len() < 5)
            .failure()
            .expect("lists reach five elements")
    };
    let (first, second) = (run(), run());
    assert_eq!(first.original, second.original);
    assert_eq!(first.passed, second.passed);
}

#[test]
fn discards_do_not_count_as_tests() {
    let outcome = QuickCheck::new().seed(4).run(|v: Vec<i8>| {
        if v.is_empty() {
            return TestResult::Discard;
        }
        let max = *v.iter().max().unwrap();
        if v.iter().all(|x| *x <= max) {
            TestResult::Pass
        } else {
            TestResult::Fail("max is not the maximum".into())
        }
    });
    assert!(
        matches!(outcome, Outcome::Passed { tests: 100 }),
        "{outcome:?}"
    );
}

#[test]
fn an_unmeetable_precondition_gives_up() {
    let outcome = QuickCheck::new().seed(5).run(|_: u8| TestResult::Discard);
    assert!(
        matches!(outcome, Outcome::GaveUp { tests: 0, .. }),
        "{outcome:?}"
    );
}

#[test]
fn integer_shrinks_move_toward_zero() {
    for x in [1i64, -1, 7, -7, 1000, i64::MAX, i64::MIN] {
        for candidate in x.shrink() {
            let closer = candidate.unsigned_abs() < x.unsigned_abs();
            let flipped = x < 0 && x.checked_neg() == Some(candidate);
            assert!(closer || flipped, "{x} shrank to {candidate}");
        }
    }
    assert_eq!(0i64.shrink().count(), 0);
}

/// A user-defined type takes one `impl`.
#[derive(Clone, Debug, PartialEq)]
struct Even(u32);

impl Arbitrary for Even {
    fn arbitrary(g: &mut Gen) -> Self {
        Even(g.below(g.size() as u64 + 1) as u32 * 2)
    }

    fn shrink(&self) -> Shrinker<Self> {
        // Shrink the half and double it, so every candidate stays even.
        Box::new((self.0 / 2).shrink().map(|half| Even(half * 2)))
    }
}

#[test]
fn custom_types_generate_and_shrink() {
    quickcheck(|Even(n)| n % 2 == 0);

    let failure = QuickCheck::new()
        .seed(6)
        .run(|Even(n)| n < 20)
        .failure()
        .expect("even numbers reach 20");
    assert_eq!(failure.shrunk, Even(20));
}
