//! The boundary between checks and the system under test.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Carries one call to the system under test.
///
/// A call is an operation name and a JSON input; the reply is a JSON
/// output. `Err` means the call did not complete (connection refused, a
/// timeout, a helper that died). An API that answers with an error status
/// has still answered, and that is an `Ok`.
pub trait Adapter {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, String>;

    /// A short description for reports, such as the URL and HTTP version.
    fn describe(&self) -> String;
}

/// The operation name of an HTTP-shaped call. Its input is an
/// [`HttpRequest`] and its output an [`HttpResponse`], both as JSON, so any
/// adapter can serve the checks generated from an OpenAPI description.
pub const HTTP: &str = "http";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    /// The path below the base URL, percent-encoded, with a leading slash.
    pub path: String,
    #[serde(default)]
    pub query: Vec<(String, String)>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<Body>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Body {
    Json(Value),
    Form(Vec<(String, String)>),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    /// The protocol version the reply arrived on, such as `HTTP/2.0`.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: String,
}

impl HttpRequest {
    /// The path with its query string.
    pub fn target(&self) -> String {
        if self.query.is_empty() {
            return self.path.clone();
        }
        format!("{}?{}", self.path, form_encode(&self.query))
    }

    /// The body as it goes on the wire.
    pub fn body_text(&self) -> Option<String> {
        self.body.as_ref().map(|body| match body {
            Body::Json(value) => value.to_string(),
            Body::Form(fields) => form_encode(fields),
            Body::Text(text) => text.clone(),
        })
    }
}

impl HttpResponse {
    /// The value of a header, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The media type of the body, without parameters, in lower case.
    pub fn media_type(&self) -> Option<String> {
        let value = self.header("content-type")?;
        Some(value.split(';').next()?.trim().to_ascii_lowercase())
    }
}

/// Sends an HTTP-shaped call through any adapter.
pub fn send(adapter: &dyn Adapter, request: &HttpRequest) -> Result<HttpResponse, String> {
    let input = serde_json::to_value(request).map_err(|e| e.to_string())?;
    let output = adapter.call(HTTP, &input)?;
    serde_json::from_value(output)
        .map_err(|e| format!("adapter reply is not an HTTP response: {e}"))
}

/// Percent-encodes everything outside the unreserved set of RFC 3986.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn form_encode(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}
