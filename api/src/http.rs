//! The HTTP adapter.

use crate::adapter::{Adapter, Body, HttpRequest, HttpResponse, HTTP};
use reqwest::blocking::Client;
use reqwest::{redirect, Method, Version};
use serde_json::Value;
use std::time::Duration;

/// Which protocol version requests go out on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpVersion {
    /// HTTP/2 when a TLS server offers it, otherwise HTTP/1.1.
    Auto,
    Http10,
    Http11,
    /// HTTP/2 without negotiation: over TLS only `h2` is offered, and over
    /// plain TCP the connection starts with the HTTP/2 preface (h2c).
    Http2,
    /// HTTP/3 over QUIC. Needs an `https` URL.
    Http3,
}

impl HttpVersion {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "auto" => HttpVersion::Auto,
            "1.0" => HttpVersion::Http10,
            "1.1" | "1" => HttpVersion::Http11,
            "2" | "2.0" => HttpVersion::Http2,
            "3" | "3.0" => HttpVersion::Http3,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            HttpVersion::Auto => "HTTP (negotiated)",
            HttpVersion::Http10 => "HTTP/1.0",
            HttpVersion::Http11 => "HTTP/1.1",
            HttpVersion::Http2 => "HTTP/2",
            HttpVersion::Http3 => "HTTP/3",
        }
    }

    /// The curl flag that selects this version.
    pub fn curl_flag(self) -> Option<&'static str> {
        match self {
            HttpVersion::Auto => None,
            HttpVersion::Http10 => Some("--http1.0"),
            HttpVersion::Http11 => Some("--http1.1"),
            HttpVersion::Http2 => Some("--http2-prior-knowledge"),
            HttpVersion::Http3 => Some("--http3-only"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpOptions {
    pub version: HttpVersion,
    pub timeout: Duration,
    /// Sent with every request, for example an `Authorization` header.
    pub headers: Vec<(String, String)>,
    /// Accept any TLS certificate. For test servers with self-signed ones.
    pub insecure: bool,
}

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions {
            version: HttpVersion::Auto,
            timeout: Duration::from_secs(30),
            headers: Vec::new(),
            insecure: false,
        }
    }
}

pub struct HttpAdapter {
    client: Client,
    base_url: String,
    options: HttpOptions,
}

impl HttpAdapter {
    /// `base_url` is everything in front of the request path, such as
    /// `http://localhost:8080/v1`.
    pub fn new(base_url: &str, options: HttpOptions) -> Result<Self, String> {
        let base_url = base_url.trim_end_matches('/').to_string();
        if options.version == HttpVersion::Http3 && !base_url.starts_with("https://") {
            return Err("HTTP/3 runs over QUIC with TLS and needs an https:// base URL".into());
        }

        // Redirects are replies like any other, so they are not followed.
        let mut builder = Client::builder()
            .redirect(redirect::Policy::none())
            .timeout(options.timeout)
            .tls_danger_accept_invalid_certs(options.insecure);
        builder = match options.version {
            HttpVersion::Auto => builder,
            HttpVersion::Http10 | HttpVersion::Http11 => builder.http1_only(),
            HttpVersion::Http2 => builder.http2_prior_knowledge(),
            HttpVersion::Http3 => builder.http3_prior_knowledge(),
        };
        let client = builder.build().map_err(|e| format!("HTTP client: {e}"))?;

        Ok(HttpAdapter {
            client,
            base_url,
            options,
        })
    }

    pub fn url(&self, request: &HttpRequest) -> String {
        format!("{}{}", self.base_url, request.target())
    }

    pub fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|_| format!("invalid HTTP method {:?}", request.method))?;
        let mut builder = self.client.request(method, self.url(request));

        builder = match self.options.version {
            HttpVersion::Http10 => builder.version(Version::HTTP_10),
            HttpVersion::Http3 => builder.version(Version::HTTP_3),
            _ => builder,
        };
        for (name, value) in self.options.headers.iter().chain(&request.headers) {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            let has_type = request
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
            if !has_type {
                let media = match body {
                    Body::Json(_) => "application/json",
                    Body::Form(_) => "application/x-www-form-urlencoded",
                    Body::Text(_) => "text/plain; charset=utf-8",
                };
                builder = builder.header("content-type", media);
            }
            builder = builder.body(request.body_text().unwrap_or_default());
        }

        let response = builder.send().map_err(describe_error)?;
        let status = response.status().as_u16();
        let version = format!("{:?}", response.version());
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = response.text().map_err(describe_error)?;

        Ok(HttpResponse {
            status,
            version,
            headers,
            body,
        })
    }
}

impl Adapter for HttpAdapter {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, String> {
        if operation != HTTP {
            return Err(format!("the HTTP adapter has no operation {operation:?}"));
        }
        let request: HttpRequest = serde_json::from_value(input.clone())
            .map_err(|e| format!("not an HTTP request: {e}"))?;
        let response = self.send(&request)?;
        serde_json::to_value(response).map_err(|e| e.to_string())
    }

    fn describe(&self) -> String {
        format!("{} over {}", self.base_url, self.options.version.label())
    }
}

/// reqwest reports the outermost error. The cause chain says what happened.
fn describe_error(error: reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        text.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    text
}
