//! qcheck testing qcheck: properties about its own parser, generators and
//! wire encoding.

#[path = "../examples/buggy_pets.rs"]
mod server;

use qcheck::{forall, gens, Gen, Generator, QuickCheck};
use qcheck_api::adapter::{form_encode, percent_encode};
use qcheck_api::openapi::Spec;
use qcheck_api::{Body, HttpAdapter, HttpOptions, HttpRequest, HttpVersion};
use serde_json::{json, Value};

fn text() -> Generator<String> {
    gens::any::<String>()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).expect("two hex digits"));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("UTF-8")
}

#[test]
fn percent_encoding_round_trips_and_leaves_only_safe_characters() {
    forall(text(), |original| {
        let encoded = percent_encode(&original);
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b)),
            "{encoded}"
        );
        percent_decode(&encoded) == original
    });
}

#[test]
fn a_form_decodes_to_the_pairs_it_was_built_from() {
    forall(gens::vec_len(gens::zip(text(), text()), 0, 6), |pairs| {
        let encoded = form_encode(&pairs);
        let decoded: Vec<(String, String)> = encoded
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').expect("key=value");
                (percent_decode(k), percent_decode(v))
            })
            .collect();
        decoded == pairs
    });
}

/// What the server receives is what the request said, on every HTTP
/// version the test server speaks.
#[test]
fn a_request_arrives_as_it_was_written() {
    let address = server::spawn();
    let json_body = gens::zip(text(), gens::any::<i64>()).map(|(s, n)| json!({"text": s, "n": n}));
    let requests = gens::zip(
        gens::vec_len(gens::zip(text(), text()), 0, 4),
        gens::option(json_body),
    );

    for version in [HttpVersion::Http10, HttpVersion::Http11, HttpVersion::Http2] {
        let options = HttpOptions {
            version,
            ..HttpOptions::default()
        };
        let adapter = HttpAdapter::new(&format!("http://{address}"), options).unwrap();
        // Each test is a network round trip, so a smaller run per version.
        QuickCheck::new()
            .tests(40)
            .check_with(&requests, |(query, body)| {
                let request = HttpRequest {
                    method: "POST".into(),
                    path: "/echo".into(),
                    query: query.clone(),
                    headers: vec![],
                    body: body.clone().map(Body::Json),
                };
                let response = adapter.send(&request).expect("a response");
                let seen: Value = serde_json::from_str(&response.body).expect("JSON");

                assert_eq!(seen["query"], form_encode(&query), "{version:?}");
                let sent = body.map(|b| b.to_string()).unwrap_or_default();
                assert_eq!(seen["body"], sent, "{version:?}");
            });
    }
}

/// JSON documents built from the vocabulary of OpenAPI, so that random
/// values land where the reader looks for structure.
fn document(budget: usize) -> Generator<Value> {
    let scalar = gens::one_of(vec![
        gens::elements(vec![
            json!("2.0"),
            json!("3.1.0"),
            json!("object"),
            json!("array"),
            json!("string"),
            json!("integer"),
            json!("query"),
            json!("path"),
            json!("body"),
            json!("#/definitions/a"),
            json!("#/paths"),
            json!("#"),
            json!("application/json"),
            Value::Null,
            json!(true),
        ]),
        gens::range(-3i64, 300).map(|n| json!(n)),
        text().map(Value::String),
    ]);
    if budget == 0 {
        return scalar;
    }
    let keys = vec![
        "swagger",
        "openapi",
        "paths",
        "/a/{id}",
        "get",
        "post",
        "parameters",
        "name",
        "in",
        "required",
        "schema",
        "type",
        "$ref",
        "items",
        "properties",
        "enum",
        "allOf",
        "oneOf",
        "const",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "minLength",
        "maxItems",
        "multipleOf",
        "responses",
        "200",
        "default",
        "content",
        "application/json",
        "requestBody",
        "definitions",
        "a",
        "consumes",
        "produces",
        "format",
        "collectionFormat",
        "servers",
        "url",
    ];
    let child = move || gens::lazy(move || document(budget / 2));
    let object = gens::vec_len(gens::zip(gens::elements(keys), child()), 0, 6).map(|members| {
        Value::Object(
            members
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    });
    let array = gens::vec_len(child(), 0, 4).map(Value::Array);
    gens::frequency(vec![(1, scalar), (4, object), (1, array)])
}

/// An operation with parameters, a request body and a response in the
/// places the reader looks, each described by a random schema.
fn operation() -> Generator<Value> {
    let schema = || gens::sized(document);
    let parameter = gens::zip3(
        gens::elements(vec!["id", "q", "x"]),
        gens::elements(vec![
            "path", "query", "header", "cookie", "formData", "body",
        ]),
        schema(),
    )
    .map(|(name, location, schema)| {
        // OpenAPI 2.0 puts the schema keywords on the parameter itself.
        let mut parameter = schema.as_object().cloned().unwrap_or_default();
        parameter.insert("name".into(), json!(name));
        parameter.insert("in".into(), json!(location));
        parameter.insert("schema".into(), schema);
        Value::Object(parameter)
    });
    gens::zip3(
        gens::vec_len(parameter, 0, 4),
        gens::option(schema()),
        schema(),
    )
    .map(|(parameters, body, response)| {
        let mut operation = json!({
            "parameters": parameters,
            "responses": {"200": {
                "schema": response.clone(),
                "content": {"application/json": {"schema": response}},
            }},
        });
        if let Some(body) = body {
            operation["requestBody"] = json!({"content": {"application/json": {"schema": body}}});
        }
        operation
    })
}

/// A description with a valid version field and random everything else.
fn description() -> Generator<Value> {
    let version = gens::elements(vec![
        ("swagger", "2.0"),
        ("openapi", "3.0.3"),
        ("openapi", "3.1.0"),
    ]);
    let operations = gens::zip(operation(), operation());
    gens::zip3(version, operations, gens::sized(document)).map(|((key, v), (get, post), shared)| {
        json!({
            key: v,
            "paths": {"/a/{id}": {"get": get, "post": post}},
            "definitions": {"a": shared},
        })
    })
}

/// A description can be wrong in any way. Reading it, building request
/// generators from it, and checking a response against it must not panic.
#[test]
fn no_description_makes_the_reader_panic() {
    QuickCheck::new()
        .tests(300)
        .max_size(40)
        .check_with(&description(), |root| {
            let spec = Spec::parse(&root.to_string()).expect("the version field is valid");
            let mut g = Gen::new(1, 20);
            let response = qcheck_api::HttpResponse {
                status: 200,
                version: String::new(),
                headers: vec![("content-type".into(), "application/json".into())],
                body: r#"{"a": [1, "x", null], "id": 7}"#.into(),
            };
            for operation in spec.operations() {
                for _ in 0..3 {
                    operation.requests().sample(&mut g);
                }
                let _ = operation.check(&response, &qcheck_api::openapi::Check::ALL);
            }
        });
}
