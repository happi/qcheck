//! QuickCheck for APIs.
//!
//! An API is tested from the outside, through an [`Adapter`] that carries a
//! call to the system under test and brings the reply back. The adapter
//! hides the protocol and the implementation language. Two are built in:
//! [`HttpAdapter`] speaks HTTP/1.0, 1.1, 2 and 3, and [`ExecAdapter`] talks
//! to a helper process in any language over JSON lines.
//!
//! [`openapi`] reads an OpenAPI description (2.0, 3.0 or 3.1) and derives, for each
//! operation, a generator of valid requests and the checks a response must
//! pass.

pub mod adapter;
pub mod exec;
pub mod http;
pub mod openapi;
pub mod schema;
pub mod sequence;

pub use adapter::{send, Adapter, Body, HttpRequest, HttpResponse};
pub use exec::ExecAdapter;
pub use http::{HttpAdapter, HttpOptions, HttpVersion};
