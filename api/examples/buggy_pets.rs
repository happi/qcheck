//! A pet API with five planted bugs, for qcheck to find. Three show on a
//! single request and two only across a sequence of calls. It serves
//! `examples/pets.yaml` over HTTP/1.0, HTTP/1.1 and HTTP/2 (h2c).
//!
//!   cargo run -p qcheck-api --example buggy_pets          # port 8642
//!   cargo run -p qcheck-api --bin qcheck -- openapi api/examples/pets.yaml \
//!       --base-url http://127.0.0.1:8642
#![allow(dead_code)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

fn pet(id: i64) -> Value {
    json!({"id": id, "name": format!("pet{id}"), "age": id % 30})
}

/// The pets that requests have created, by id.
#[derive(Default)]
pub struct Store {
    pets: BTreeMap<i64, Value>,
    created: i64,
}

/// The API itself: a status and a JSON body for each request. A null body
/// is sent as an empty one.
pub fn route(
    store: &Mutex<Store>,
    method: &Method,
    path: &str,
    query: &str,
    body: &[u8],
) -> (u16, Value) {
    let mut store = store.lock().expect("the store lock");
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match (method.as_str(), segments.as_slice()) {
        ("GET", ["health"]) => (200, json!({"status": "ok"})),

        ("GET", ["pets"]) => {
            let limit = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("limit="))
                .and_then(|n| n.parse::<i64>().ok())
                .unwrap_or(10);
            // Bug 1: the page count divides by the limit.
            if limit == 0 {
                return (500, json!({"error": "attempt to divide by zero"}));
            }
            (200, Value::Array((1..=limit.min(3)).map(pet).collect()))
        }

        ("GET", ["pets", id]) => match id.parse::<i64>() {
            Ok(id) if store.pets.contains_key(&id) => (200, store.pets[&id].clone()),
            // Bug 2: pets imported from the old system have no name.
            Ok(id) if id > 1000 => (200, json!({"id": id, "name": null, "age": 3})),
            Ok(id) if id % 2 == 0 => (200, pet(id)),
            _ => (404, json!({"error": "no such pet"})),
        },

        ("POST", ["pets"]) => {
            let Ok(new) = serde_json::from_slice::<Value>(body) else {
                return (400, json!({"error": "the body is not JSON"}));
            };
            let (Some(name), Some(age)) = (new["name"].as_str(), new["age"].as_i64()) else {
                return (400, json!({"error": "name and age are required"}));
            };
            // Bug 3: the column is ten characters wide, the API says twenty.
            if name.chars().count() > 10 {
                return (422, json!({"error": "name too long"}));
            }
            // Created pets get odd ids from 7 up; the even ids are the
            // built-in pets above.
            store.created += 1;
            let id = 5 + 2 * store.created;
            let mut stored = json!({"id": id, "name": name, "age": age});
            if let Some(tag) = new.get("tag") {
                stored["tag"] = tag.clone();
            }
            // Bug 4: the oldest allowed age is 30, and this check drops it.
            // The pet is reported as created and can never be read.
            if age < 30 {
                store.pets.insert(id, stored.clone());
            }
            (201, stored)
        }

        ("DELETE", ["pets", id]) => match id.parse::<i64>() {
            Ok(id) if store.pets.contains_key(&id) => {
                // Bug 5: a tagged pet is kept "for the tag index", so it
                // can still be read after its delete succeeded.
                if store.pets[&id].get("tag").is_none() {
                    store.pets.remove(&id);
                }
                (204, Value::Null)
            }
            _ => (404, json!({"error": "no such pet"})),
        },

        // Not part of the described API. It returns what arrived, so a test
        // can compare a request with what the server saw.
        (_, ["echo"]) => (
            200,
            json!({"method": method.as_str(), "query": query, "body": String::from_utf8_lossy(body)}),
        ),

        _ => (404, json!({"error": "no such route"})),
    }
}

async fn handle(
    store: Arc<Mutex<Store>>,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let version = format!("{:?}", request.version());
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or("").to_string();
    let body = match request.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => Bytes::new(),
    };

    let (status, json) = route(&store, &method, &path, &query, &body);
    let text = match json {
        Value::Null => String::new(),
        json => json.to_string(),
    };
    let response = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        // Lets a client confirm which protocol version the server saw.
        .header("x-request-version", version)
        .body(Full::new(Bytes::from(text)))
        .expect("a valid response");
    Ok(response)
}

fn serve(listener: TcpListener) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let store = Arc::new(Mutex::new(Store::default()));
    runtime.block_on(async {
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let listener = tokio::net::TcpListener::from_std(listener).expect("a tokio listener");
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let store = store.clone();
            tokio::spawn(async move {
                let connection = Builder::new(TokioExecutor::new());
                let service = service_fn(move |request| handle(store.clone(), request));
                let _ = connection
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
}

/// Starts the server on a free port, on a background thread.
pub fn spawn() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let address = listener.local_addr().expect("a local address");
    std::thread::spawn(move || serve(listener));
    address
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:8642").expect("port 8642 is free");
    println!("buggy pets on http://127.0.0.1:8642");
    serve(listener);
}
