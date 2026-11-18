//! A pet API with three planted bugs, for qcheck to find. It serves
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
use std::convert::Infallible;
use std::net::{SocketAddr, TcpListener};

fn pet(id: i64) -> Value {
    json!({"id": id, "name": format!("pet{id}"), "age": id % 30})
}

/// The API itself: a status and a JSON body for each request.
pub fn route(method: &Method, path: &str, query: &str, body: &[u8]) -> (u16, Value) {
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
            let mut stored = json!({"id": 7, "name": name, "age": age});
            if let Some(tag) = new.get("tag") {
                stored["tag"] = tag.clone();
            }
            (201, stored)
        }

        _ => (404, json!({"error": "no such route"})),
    }
}

async fn handle(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let version = format!("{:?}", request.version());
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or("").to_string();
    let body = match request.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => Bytes::new(),
    };

    let (status, json) = route(&method, &path, &query, &body);
    let response = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        // Lets a client confirm which protocol version the server saw.
        .header("x-request-version", version)
        .body(Full::new(Bytes::from(json.to_string())))
        .expect("a valid response");
    Ok(response)
}

fn serve(listener: TcpListener) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    runtime.block_on(async {
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let listener = tokio::net::TcpListener::from_std(listener).expect("a tokio listener");
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(async move {
                let connection = Builder::new(TokioExecutor::new());
                let _ = connection
                    .serve_connection(TokioIo::new(stream), service_fn(handle))
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
