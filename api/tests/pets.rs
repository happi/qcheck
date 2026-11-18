//! The generated checks against a server with three planted bugs.

#[path = "../examples/buggy_pets.rs"]
mod server;

use qcheck_api::openapi::{check_operation, check_spec, Check, Failure, Options, Spec, Verdict};
use qcheck_api::{Adapter, Body, ExecAdapter, HttpAdapter, HttpOptions, HttpRequest, HttpVersion};
use serde_json::json;

fn spec() -> Spec {
    Spec::load(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/pets.yaml")).expect("the spec loads")
}

fn adapter(version: HttpVersion) -> HttpAdapter {
    let options = HttpOptions {
        version,
        ..HttpOptions::default()
    };
    HttpAdapter::new(&format!("http://{}", server::spawn()), options).expect("an adapter")
}

fn options() -> Options {
    Options {
        seed: Some(1),
        ..Options::default()
    }
}

fn failure(spec: &Spec, adapter: &dyn Adapter, operation: &str) -> Failure {
    let reports = check_spec(spec, adapter, &options(), Some(operation));
    assert_eq!(reports.len(), 1, "one operation named {operation}");
    match &reports[0].verdict {
        Verdict::Failed(failure) => (**failure).clone(),
        other => panic!("{operation} should fail, got {other:?}"),
    }
}

#[test]
fn a_passing_operation_reports_the_statuses_it_saw() {
    let reports = check_spec(
        &spec(),
        &adapter(HttpVersion::Http11),
        &options(),
        Some("health"),
    );
    match &reports[0].verdict {
        Verdict::Passed { statuses, .. } => assert_eq!(statuses, &[(200, 100)]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_correct_operation_passes() {
    let reports = check_spec(
        &spec(),
        &adapter(HttpVersion::Http11),
        &options(),
        Some("health"),
    );
    assert!(matches!(
        reports[0].verdict,
        Verdict::Passed { tests: 100, .. }
    ));
}

#[test]
fn a_server_error_shrinks_to_the_one_parameter_that_causes_it() {
    let failure = failure(&spec(), &adapter(HttpVersion::Http11), "listPets");
    assert_eq!(failure.violation.check, Check::NoServerError);
    // The optional `tag` is gone and only the limit of zero remains.
    assert_eq!(failure.request.target(), "/pets?limit=0");
    assert_eq!(failure.response.unwrap().status, 500);
}

#[test]
fn a_schema_violation_shrinks_to_the_boundary() {
    let failure = failure(&spec(), &adapter(HttpVersion::Http11), "getPet");
    assert_eq!(failure.violation.check, Check::ResponseSchema);
    assert_eq!(
        failure.violation.message,
        "$.name: expected string, got null"
    );
    assert_eq!(failure.request.path, "/pets/1001");
}

#[test]
fn an_undocumented_status_shrinks_the_request_body() {
    let failure = failure(&spec(), &adapter(HttpVersion::Http11), "createPet");
    assert_eq!(failure.violation.check, Check::DocumentedStatus);
    assert!(failure
        .violation
        .message
        .starts_with("status 422 is not documented"));
    assert_eq!(
        failure.request.body,
        Some(Body::Json(json!({"name": "aaaaaaaaaaa", "age": 0})))
    );
}

/// The same API described in OpenAPI 3.1. Its description lists `4XX` for
/// `createPet`, so the 422 is documented there and that operation passes.
#[test]
fn an_openapi_3_description_finds_the_same_bugs() {
    let spec = Spec::load(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/pets-v3.yaml"
    ))
    .unwrap();
    assert_eq!(spec.base_url().as_deref(), Some("http://127.0.0.1:8642"));
    let adapter = adapter(HttpVersion::Http11);

    let list = failure(&spec, &adapter, "listPets");
    assert_eq!(list.violation.check, Check::NoServerError);
    assert_eq!(list.request.target(), "/pets?limit=0");

    let get = failure(&spec, &adapter, "getPet");
    assert_eq!(get.violation.message, "$.name: expected string, got null");
    assert_eq!(get.request.path, "/pets/1001");

    for operation in ["health", "createPet"] {
        let reports = check_spec(&spec, &adapter, &options(), Some(operation));
        assert!(
            matches!(reports[0].verdict, Verdict::Passed { tests: 100, .. }),
            "{operation}"
        );
    }
}

/// With `id` pinned to 2000 half the time, getPet meets the unnamed pet
/// on a known id. A fixture does not shrink, so the report keeps either
/// that id or the boundary a generated id shrank to.
#[test]
fn a_fixture_value_reaches_the_path_that_needs_existing_data() {
    let spec = spec();
    let operation = spec
        .operations()
        .into_iter()
        .find(|o| o.matches("getPet"))
        .unwrap();
    let mut g = qcheck::Gen::new(1, 10);
    let fixtures = vec![("id".to_string(), json!(2000))];
    let requests = operation.requests_with(&fixtures);
    let pinned = (0..200)
        .filter(|_| requests.sample(&mut g).path == "/pets/2000")
        .count();
    assert!((60..140).contains(&pinned), "{pinned} of 200");

    let options = Options {
        fixtures,
        ..options()
    };
    match check_operation(&operation, &adapter(HttpVersion::Http11), &options) {
        Verdict::Failed(failure) => {
            let path = failure.request.path.as_str();
            assert!(matches!(path, "/pets/2000" | "/pets/1001"), "{path}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_documented_5xx_is_not_a_server_error_but_default_does_not_excuse_one() {
    let describe = |responses: &str| {
        let text = format!(
            r#"{{"swagger": "2.0", "paths": {{"/pets": {{"get": {{"operationId": "listPets",
                "parameters": [{{"name": "limit", "in": "query", "type": "integer", "enum": [0]}}],
                "responses": {responses}}}}}}}}}"#
        );
        Spec::parse(&text).unwrap()
    };
    let adapter = adapter(HttpVersion::Http11);
    let run = |spec: &Spec| {
        check_spec(spec, &adapter, &options(), None)
            .remove(0)
            .verdict
    };

    // `GET /pets?limit=0` answers 500 on the example server.
    let promised = describe(r#"{"200": {"description": "ok"}, "500": {"description": "known"}}"#);
    assert!(matches!(run(&promised), Verdict::Passed { .. }));
    let ranged = describe(r#"{"200": {"description": "ok"}, "5XX": {"description": "known"}}"#);
    assert!(matches!(run(&ranged), Verdict::Passed { .. }));
    let catch_all = describe(r#"{"default": {"description": "anything"}}"#);
    match run(&catch_all) {
        Verdict::Failed(failure) => assert_eq!(failure.violation.check, Check::NoServerError),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_filter_with_a_star_is_a_pattern_for_the_operation_id() {
    let spec = spec();
    let ids = |filter: &str| -> Vec<String> {
        let matching = spec.operations().into_iter().filter(|o| o.matches(filter));
        matching.filter_map(|o| o.operation_id).collect()
    };
    assert_eq!(ids("*Pet"), ["createPet", "getPet", "deletePet"]);
    assert_eq!(
        ids("*Pet*"),
        ["listPets", "createPet", "getPet", "deletePet"]
    );
    assert_eq!(ids("get*"), ["getPet"]);
    assert_eq!(ids("getPet"), ["getPet"]);
    assert_eq!(ids("GET /pets"), ["listPets", "getPet"]);
    assert_eq!(ids("/pets/{id}"), ["getPet", "deletePet"]);
}

#[test]
fn path_parameters_are_never_dot_segments() {
    let spec = Spec::parse(
        r#"{"swagger": "2.0", "paths": {"/files/{name}": {"get": {
            "parameters": [{"name": "name", "in": "path", "required": true, "type": "string", "maxLength": 2}],
            "responses": {"200": {"description": "ok"}}}}}}"#,
    )
    .unwrap();
    let requests = spec.operations()[0].requests();
    qcheck::QuickCheck::new()
        .tests(2000)
        .check_with(&requests, |request| {
            !matches!(request.path.as_str(), "/files/." | "/files/..")
        });
}

#[test]
fn a_disabled_check_is_not_reported() {
    let options = Options {
        checks: vec![Check::Transport, Check::NoServerError],
        ..options()
    };
    let reports = check_spec(
        &spec(),
        &adapter(HttpVersion::Http11),
        &options,
        Some("createPet"),
    );
    assert!(matches!(reports[0].verdict, Verdict::Passed { .. }));
}

#[test]
fn an_unreachable_server_fails_the_transport_check() {
    // Port 9 (discard) on localhost is closed on any sane machine.
    let adapter = HttpAdapter::new("http://127.0.0.1:9", HttpOptions::default()).unwrap();
    let spec = spec();
    let operation = spec
        .operations()
        .into_iter()
        .find(|o| o.matches("health"))
        .unwrap();
    match check_operation(&operation, &adapter, &options()) {
        Verdict::Failed(failure) => assert_eq!(failure.violation.check, Check::Transport),
        other => panic!("expected a transport failure, got {other:?}"),
    }
}

/// The server echoes the protocol version it saw, so this confirms what
/// went over the wire and not only what the client asked for.
#[test]
fn each_http_version_reaches_the_server_as_that_version() {
    let request = HttpRequest {
        method: "GET".into(),
        path: "/health".into(),
        query: vec![],
        headers: vec![],
        body: None,
    };
    for (version, expected) in [
        (HttpVersion::Http10, "HTTP/1.0"),
        (HttpVersion::Http11, "HTTP/1.1"),
        (HttpVersion::Http2, "HTTP/2.0"),
        (HttpVersion::Auto, "HTTP/1.1"),
    ] {
        let response = adapter(version).send(&request).expect("a response");
        assert_eq!(response.status, 200, "{version:?}");
        assert_eq!(
            response.header("x-request-version"),
            Some(expected),
            "{version:?}"
        );
    }
}

#[test]
fn the_same_bugs_are_found_over_http_1_0_and_http_2() {
    for version in [HttpVersion::Http10, HttpVersion::Http2] {
        let failure = failure(&spec(), &adapter(version), "listPets");
        assert_eq!(failure.request.target(), "/pets?limit=0", "{version:?}");
    }
}

#[test]
fn http_3_refuses_a_plain_http_url() {
    let options = HttpOptions {
        version: HttpVersion::Http3,
        ..HttpOptions::default()
    };
    let error = HttpAdapter::new("http://127.0.0.1:1", options)
        .err()
        .expect("an error");
    assert!(error.contains("https"), "{error}");
}

#[test]
fn a_helper_process_serves_the_same_checks() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("python3 is not installed; skipping the helper test");
        return;
    }
    let helper = concat!(
        "python3 ",
        env!("CARGO_MANIFEST_DIR"),
        "/examples/pets_helper.py"
    );
    let adapter = ExecAdapter::new(helper).expect("the helper starts");
    let spec = spec();

    let reports = check_spec(&spec, &adapter, &options(), Some("health"));
    assert!(matches!(
        reports[0].verdict,
        Verdict::Passed { tests: 100, .. }
    ));

    let failure = failure(&spec, &adapter, "createPet");
    assert_eq!(failure.violation.check, Check::ResponseSchema);
    assert_eq!(
        failure.violation.message,
        "$.age: expected integer, got string"
    );
    assert_eq!(
        failure.request.body,
        Some(Body::Json(json!({"name": "a", "age": 0})))
    );
}

#[test]
fn descriptions_that_are_not_openapi_are_refused() {
    assert!(Spec::parse("openapi: 4.0.0")
        .err()
        .unwrap()
        .contains("not supported"));
    assert!(Spec::parse("{}").err().unwrap().contains("version field"));
}
