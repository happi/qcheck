//! Sequences of calls against the example server, which has two bugs that
//! no single request can show.

#[path = "../examples/buggy_pets.rs"]
mod server;

use qcheck_api::openapi::{Check, Operation, Options, Spec};
use qcheck_api::sequence::{check_sequences, SequenceFailure, SequenceVerdict};
use qcheck_api::{Body, HttpAdapter, HttpOptions};
use serde_json::Value;

fn adapter() -> HttpAdapter {
    HttpAdapter::new(
        &format!("http://{}", server::spawn()),
        HttpOptions::default(),
    )
    .unwrap()
}

/// The pet operations of the OpenAPI 3.1 description, which documents 4XX
/// for createPet. `getPet` runs without the schema check, because its
/// single-request bug (an unnamed pet) is not what these tests are about.
fn operations(without: &[Check]) -> Vec<(Operation, Vec<Check>)> {
    let spec = Spec::load(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/pets-v3.yaml"
    ))
    .unwrap();
    let wanted = ["createPet", "getPet", "deletePet"];
    let chosen = spec
        .operations()
        .into_iter()
        .filter(|o| wanted.iter().any(|id| o.matches(id)));
    chosen
        .map(|operation| {
            let mut checks = Check::ALL.to_vec();
            checks.retain(|check| !without.contains(check));
            if operation.matches("getPet") {
                checks.retain(|check| *check != Check::ResponseSchema);
            }
            (operation, checks)
        })
        .collect()
}

fn options() -> Options {
    Options {
        seed: Some(1),
        sequences: 300,
        ..Options::default()
    }
}

fn failure(without: &[Check]) -> SequenceFailure {
    match check_sequences(&operations(without), &adapter(), &options()) {
        SequenceVerdict::Failed(failure) => *failure,
        other => panic!("expected a failing sequence, got {other:?}"),
    }
}

fn shape(failure: &SequenceFailure) -> Vec<(&str, Option<u16>)> {
    let calls = failure.calls.iter();
    calls
        .map(|call| (call.request.method.as_str(), call.status))
        .collect()
}

fn json_body(failure: &SequenceFailure, call: usize) -> &Value {
    match &failure.calls[call].request.body {
        Some(Body::Json(body)) => body,
        other => panic!("call {call} has no JSON body: {other:?}"),
    }
}

/// The server keeps its pets between sequences, so the shortest failing
/// sequence need not create the pet itself: it may find one that an
/// earlier sequence left behind. Either way it ends with a delete that
/// succeeds and a read of the same pet that succeeds too.
#[test]
fn a_deleted_pet_that_can_still_be_read_shrinks_to_three_calls() {
    let failure = failure(&[Check::CreatedIsReadable]);
    assert_eq!(failure.violation.check, Check::DeletedStaysDeleted);

    let calls = shape(&failure);
    assert!(calls.len() <= 3, "{calls:?}");
    assert_eq!(
        calls[calls.len() - 2..],
        [("DELETE", Some(204)), ("GET", Some(200))]
    );
    let [.., delete, read] = failure.calls.as_slice() else {
        panic!("{calls:?}");
    };
    assert_eq!(delete.request.path, read.request.path);
    // Only a tagged pet survives its delete, so when the sequence creates
    // the pet, the tag stays in the body through shrinking.
    if calls[0].0 == "POST" {
        assert!(json_body(&failure, 0).get("tag").is_some());
    }
}

#[test]
fn a_created_pet_that_cannot_be_read_shrinks_to_two_calls() {
    let failure = failure(&[Check::DeletedStaysDeleted]);
    assert_eq!(failure.violation.check, Check::CreatedIsReadable);
    assert_eq!(shape(&failure), [("POST", Some(201)), ("GET", Some(404))]);
    // The pet that is lost is the one at the oldest allowed age.
    assert_eq!(json_body(&failure, 0)["age"], 30);
    assert_eq!(json_body(&failure, 0)["name"], "a");
}

#[test]
fn without_the_sequence_checks_the_calls_pass_and_reuse_ids() {
    let without = [Check::DeletedStaysDeleted, Check::CreatedIsReadable];
    match check_sequences(&operations(&without), &adapter(), &options()) {
        SequenceVerdict::Passed {
            sequences,
            calls,
            reused,
            statuses,
        } => {
            assert_eq!(sequences, 300);
            assert!(
                calls >= 300 && reused > 50,
                "{calls} calls, {reused} reused"
            );
            // Reads and deletes reached pets that exist, which no single
            // generated request does for a created pet.
            let count = |wanted: u16| {
                statuses
                    .iter()
                    .find(|(s, _)| *s == wanted)
                    .map_or(0, |(_, n)| *n)
            };
            assert!(count(204) > 10, "{statuses:?}");
            assert!(count(200) > 10, "{statuses:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn sequences_can_be_turned_off() {
    let options = Options {
        sequences: 0,
        ..options()
    };
    let verdict = check_sequences(&operations(&[]), &adapter(), &options);
    assert!(matches!(verdict, SequenceVerdict::Skipped(_)));
}

#[test]
fn a_failing_reset_command_is_reported() {
    let options = Options {
        reset: Some("exit 3".to_string()),
        ..options()
    };
    match check_sequences(&operations(&[]), &adapter(), &options) {
        SequenceVerdict::Failed(failure) => {
            assert_eq!(failure.violation.check, Check::Transport);
            assert!(failure.calls.is_empty());
        }
        other => panic!("{other:?}"),
    }
}
