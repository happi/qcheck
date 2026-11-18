//! Checks generated from an OpenAPI description, version 2.0 (Swagger),
//! 3.0 or 3.1.
//!
//! Each operation in the description becomes one property: for every valid
//! request, the response passes the checks in [`Check`].

use crate::adapter::{percent_encode, send, Adapter, Body, HttpRequest, HttpResponse};
use crate::schema;
use qcheck::{gens, Generator, Outcome, QuickCheck, TestResult};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

const METHODS: [&str; 7] = ["get", "put", "post", "delete", "options", "head", "patch"];

/// What a response is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// The call completed: a response came back at all.
    Transport,
    /// The status is below 500, or is a 5xx the description lists by code
    /// or range.
    NoServerError,
    /// The status is listed for the operation, or the operation has a
    /// `default` response.
    DocumentedStatus,
    /// The media type of the body is one the description lists for the
    /// status.
    ContentType,
    /// The body conforms to the schema documented for the status.
    ResponseSchema,
    /// In a sequence: after a DELETE succeeded, a GET of the same path
    /// does not succeed.
    DeletedStaysDeleted,
    /// In a sequence: after a POST created a resource, a GET of it is
    /// not answered with 404.
    CreatedIsReadable,
}

impl Check {
    pub const ALL: [Check; 7] = [
        Check::Transport,
        Check::NoServerError,
        Check::DocumentedStatus,
        Check::ContentType,
        Check::ResponseSchema,
        Check::DeletedStaysDeleted,
        Check::CreatedIsReadable,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Check::Transport => "transport",
            Check::NoServerError => "no-server-error",
            Check::DocumentedStatus => "documented-status",
            Check::ContentType => "content-type",
            Check::ResponseSchema => "response-schema",
            Check::DeletedStaysDeleted => "deleted-stays-deleted",
            Check::CreatedIsReadable => "created-is-readable",
        }
    }

    pub fn parse(name: &str) -> Option<Check> {
        Check::ALL.into_iter().find(|c| c.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    pub check: Check,
    pub message: String,
}

/// A parsed description.
pub struct Spec {
    root: Rc<Value>,
    /// True for OpenAPI 3.x, false for 2.0.
    v3: bool,
}

impl Spec {
    /// Parses JSON or YAML.
    pub fn parse(text: &str) -> Result<Spec, String> {
        let root: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => serde_yaml::from_str(text).map_err(|e| format!("not JSON or YAML: {e}"))?,
        };
        let field = |name| root.get(name).and_then(Value::as_str);
        let v3 = match (field("swagger"), field("openapi")) {
            (Some("2.0"), _) => false,
            (None, Some(version)) if version.starts_with("3.") => true,
            (Some(other), _) | (None, Some(other)) => {
                return Err(format!(
                    "version {other:?} is not supported; OpenAPI 2.0, 3.0 and 3.1 are"
                ))
            }
            (None, None) => {
                return Err(
                    "no `swagger` or `openapi` version field; is this an OpenAPI description?"
                        .into(),
                )
            }
        };
        Ok(Spec {
            root: Rc::new(root),
            v3,
        })
    }

    /// Loads a description from a file, or from an `http(s)` URL such as
    /// the `/openapi.json` that many frameworks serve.
    pub fn load(location: &str) -> Result<Spec, String> {
        let text = if is_url(location) {
            reqwest::blocking::get(location)
                .and_then(|response| response.error_for_status())
                .and_then(|response| response.text())
                .map_err(|e| format!("{location}: {e}"))?
        } else {
            std::fs::read_to_string(location).map_err(|e| format!("{location}: {e}"))?
        };
        Spec::parse(&text).map_err(|e| format!("{location}: {e}"))
    }

    /// The base URL the description itself names, when it names one:
    /// `host` and `basePath` in 2.0, the first absolute server URL in 3.x.
    pub fn base_url(&self) -> Option<String> {
        if self.v3 {
            let url = self.root.pointer("/servers/0/url")?.as_str()?;
            let absolute = url.starts_with("http://") || url.starts_with("https://");
            return (absolute && !url.contains('{')).then(|| url.trim_end_matches('/').to_string());
        }
        let host = self.root.get("host")?.as_str()?;
        let schemes = self.root.get("schemes").and_then(Value::as_array);
        let scheme = schemes
            .and_then(|s| s.iter().filter_map(Value::as_str).find(|s| *s == "https"))
            .or_else(|| schemes.and_then(|s| s.first()?.as_str()))
            .unwrap_or("https");
        let base_path = self
            .root
            .get("basePath")
            .and_then(Value::as_str)
            .unwrap_or("");
        Some(format!(
            "{scheme}://{host}{}",
            base_path.trim_end_matches('/')
        ))
    }

    /// Every operation, in the order of the description.
    pub fn operations(&self) -> Vec<Operation> {
        let mut operations = Vec::new();
        let paths = self.root.get("paths").and_then(Value::as_object);
        for (path, item) in paths.into_iter().flatten() {
            for method in METHODS {
                if let Some(operation) = item.get(method) {
                    operations.push(Operation::new(
                        &self.root, self.v3, path, method, item, operation,
                    ));
                }
            }
        }
        operations
    }
}

#[derive(Clone)]
pub(crate) struct Parameter {
    pub(crate) name: String,
    /// `path`, `query`, `header`, `cookie`, `formData`, `body`, or `form`
    /// for a URL-encoded body described by one object schema.
    pub(crate) location: String,
    pub(crate) required: bool,
    pub(crate) schema: Value,
    pub(crate) collection_format: String,
}

/// What the description says about the response for one status.
#[derive(Clone)]
struct Documented {
    schema: Option<Value>,
    /// The media types the response may have. Empty when none are named.
    media: Vec<String>,
}

/// One method on one path.
#[derive(Clone)]
pub struct Operation {
    pub(crate) root: Rc<Value>,
    pub method: String,
    pub path: String,
    pub operation_id: Option<String>,
    pub(crate) parameters: Vec<Parameter>,
    /// The media type of a JSON request body.
    body_media: String,
    /// The `Accept` header, when the description gives grounds for one.
    accept: Option<String>,
    /// Status code, range such as `2XX`, or `default`, to its response.
    responses: Vec<(String, Documented)>,
    /// Why the operation cannot be checked, when it cannot.
    pub unsupported: Option<String>,
    /// Parts of the description that generation does not enforce.
    pub warnings: Vec<String>,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.method, self.path)?;
        if let Some(id) = &self.operation_id {
            write!(f, " ({id})")?;
        }
        Ok(())
    }
}

fn strings(value: Option<&Value>) -> Option<Vec<String>> {
    Some(
        value?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

fn mentions_pattern(root: &Value, schema: &Value, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    let Value::Object(map) = schema::resolve(root, schema) else {
        return false;
    };
    let deeper = |value: &Value| mentions_pattern(root, value, depth + 1);
    map.contains_key("pattern")
        || map.iter().any(|(key, value)| match key.as_str() {
            "items" => deeper(value),
            "properties" => value.as_object().is_some_and(|p| p.values().any(deeper)),
            "allOf" | "oneOf" | "anyOf" => value.as_array().is_some_and(|a| a.iter().any(deeper)),
            _ => false,
        })
}

fn is_url(location: &str) -> bool {
    location.starts_with("http://") || location.starts_with("https://")
}

/// The scheme and authority of a URL: where a description fetched over
/// HTTP was served from, which is where its API most likely runs.
pub fn origin(location: &str) -> Option<String> {
    if !is_url(location) {
        return None;
    }
    let (scheme, rest) = location.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(format!("{scheme}://{authority}"))
}

fn is_json(media: &str) -> bool {
    let media = media.split(';').next().unwrap_or("").trim();
    media == "application/json" || media.ends_with("+json")
}

/// True when `media` falls under `pattern`, which may be `*/*` or `type/*`.
fn media_matches(pattern: &str, media: &str) -> bool {
    let pattern = pattern.split(';').next().unwrap_or("").trim();
    match pattern.split_once('/') {
        Some(("*", "*")) => true,
        Some((kind, "*")) => media.split('/').next() == Some(kind),
        _ => pattern.eq_ignore_ascii_case(media),
    }
}

impl Operation {
    fn new(
        root: &Rc<Value>,
        v3: bool,
        path: &str,
        method: &str,
        item: &Value,
        operation: &Value,
    ) -> Operation {
        let mut unsupported = None;
        let mut warnings = Vec::new();

        // Parameters on the path apply to each of its operations, and an
        // operation replaces one by repeating its name and location.
        let mut parameters: Vec<Parameter> = Vec::new();
        let declared = [item.get("parameters"), operation.get("parameters")];
        for parameter in declared
            .into_iter()
            .flatten()
            .filter_map(Value::as_array)
            .flatten()
        {
            let parameter = schema::resolve(root, parameter);
            let field = |name: &str| parameter.get(name).and_then(Value::as_str).unwrap_or("");
            let (name, location) = (field("name").to_string(), field("in").to_string());
            if field("type") == "file" {
                unsupported = Some("file uploads are not supported".to_string());
            }
            // In 2.0 only a body has a schema; the other parameters carry
            // the schema keywords themselves. In 3.x each has a schema.
            let mut schema = match (v3, location.as_str()) {
                (true, _) | (false, "body") => {
                    parameter.get("schema").cloned().unwrap_or(json!({}))
                }
                (false, _) => parameter.clone(),
            };
            // OpenAPI 3 lets the parameter carry the example, beside its
            // schema. The generator looks for it on the schema.
            if let (true, Some(example), Some(map)) =
                (v3, parameter.get("example"), schema.as_object_mut())
            {
                map.entry("example").or_insert(example.clone());
            }
            let collection_format = match (v3, field("collectionFormat"), field("style")) {
                (false, "", _) => "csv",
                (false, format, _) => format,
                (true, _, "spaceDelimited") => "ssv",
                (true, _, "pipeDelimited") => "pipes",
                // `form` style explodes by default: one pair per item.
                (true, _, _) if matches!(location.as_str(), "query" | "cookie") => {
                    match parameter.get("explode") {
                        Some(Value::Bool(false)) => "csv",
                        _ => "multi",
                    }
                }
                (true, _, _) => "csv",
            };
            let parsed = Parameter {
                required: location == "path"
                    || parameter.get("required") == Some(&Value::Bool(true)),
                collection_format: collection_format.to_string(),
                name,
                location,
                schema,
            };
            match parameters
                .iter_mut()
                .find(|p| p.name == parsed.name && p.location == parsed.location)
            {
                Some(existing) => *existing = parsed,
                None => parameters.push(parsed),
            }
        }

        let inherited =
            |name: &str| strings(operation.get(name)).or_else(|| strings(root.get(name)));
        let mut body_media = "application/json".to_string();
        let mut accept = None;

        if v3 {
            if let Some(body) = operation.get("requestBody") {
                let body = schema::resolve(root, body);
                let content = body.get("content").and_then(Value::as_object);
                let find = |wanted: &dyn Fn(&str) -> bool| {
                    content.and_then(|c| c.iter().find(|(media, _)| wanted(media)))
                };
                let json_body = find(&is_json);
                let form_body = find(&|m| m == "application/x-www-form-urlencoded");
                match json_body.or(form_body) {
                    Some((media, described)) => {
                        if json_body.is_some() {
                            body_media = media.clone();
                        }
                        parameters.push(Parameter {
                            name: "body".to_string(),
                            location: if json_body.is_some() { "body" } else { "form" }.to_string(),
                            required: body.get("required") == Some(&Value::Bool(true)),
                            schema: described.get("schema").cloned().unwrap_or(json!({})),
                            collection_format: "multi".to_string(),
                        });
                    }
                    None => {
                        let offered: Vec<&str> = content
                            .map(|c| c.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        unsupported = Some(format!(
                            "request bodies of type {} are not supported",
                            offered.join(", ")
                        ));
                    }
                }
            }
        } else {
            let consumes = inherited("consumes").unwrap_or(vec!["application/json".to_string()]);
            let has = |location: &str| parameters.iter().any(|p| p.location == location);
            match consumes.iter().find(|m| is_json(m)) {
                Some(media) => body_media = media.clone(),
                None if has("body") => {
                    unsupported = Some(format!(
                        "request bodies of type {} are not supported",
                        consumes.join(", ")
                    ));
                }
                None => {}
            }
            if has("formData")
                && !consumes
                    .iter()
                    .any(|m| m == "application/x-www-form-urlencoded")
            {
                unsupported.get_or_insert("multipart form data is not supported".to_string());
            }
        }

        for parameter in &parameters {
            if mentions_pattern(root, &parameter.schema, 0) {
                warnings.push(format!(
                    "`pattern` on parameter {:?} is not enforced; generated values may not match it",
                    parameter.name
                ));
            }
        }

        let produces = inherited("produces").unwrap_or(vec!["application/json".to_string()]);
        if !v3 {
            accept = Some(produces.join(", "));
        }
        let mut responses = Vec::new();
        let listed = operation.get("responses").and_then(Value::as_object);
        for (status, response) in listed.into_iter().flatten() {
            let response = schema::resolve(root, response);
            let documented = if v3 {
                let content = response.get("content").and_then(Value::as_object);
                Documented {
                    schema: content
                        .and_then(|c| c.iter().find(|(media, _)| is_json(media)))
                        .and_then(|(_, described)| described.get("schema").cloned()),
                    media: content
                        .map(|c| c.keys().cloned().collect())
                        .unwrap_or_default(),
                }
            } else {
                let schema = response.get("schema").cloned();
                Documented {
                    media: if schema.is_some() {
                        produces.clone()
                    } else {
                        Vec::new()
                    },
                    schema,
                }
            };
            responses.push((status.to_uppercase(), documented));
        }

        Operation {
            root: root.clone(),
            method: method.to_uppercase(),
            path: path.to_string(),
            operation_id: operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::to_string),
            parameters,
            body_media,
            accept,
            responses,
            unsupported,
            warnings,
        }
    }

    /// True when `filter` is the operation id or occurs in `METHOD /path`.
    /// A filter with `*` in it is a pattern for the operation id, where
    /// each `*` stands for any run of characters.
    pub fn matches(&self, filter: &str) -> bool {
        let id = self.operation_id.as_deref();
        if filter.contains('*') {
            return id.is_some_and(|id| glob(filter, id));
        }
        id == Some(filter) || format!("{} {}", self.method, self.path).contains(filter)
    }

    /// A generator of requests the description calls valid. Optional
    /// parameters come and go, and shrink to absent.
    pub fn requests(&self) -> Generator<HttpRequest> {
        self.requests_with(&[])
    }

    /// Like `requests`, with values known to exist on the server. A
    /// fixture is a parameter or body property name and a value; half the
    /// requests that have that parameter use it. Requests are independent,
    /// so without one an id-based route is only ever asked for ids that
    /// do not exist.
    pub fn requests_with(&self, fixtures: &[(String, Value)]) -> Generator<HttpRequest> {
        let operation = self.clone();
        self.values(fixtures)
            .map(move |values| operation.build(&values))
    }

    /// One value per parameter, in parameter order; `None` for an optional
    /// parameter that is left out. `build` turns them into a request.
    pub(crate) fn values(&self, fixtures: &[(String, Value)]) -> Generator<Vec<Option<Value>>> {
        let known = |name: &str| -> Vec<Value> {
            let matching = fixtures.iter().filter(|(n, _)| n == name);
            matching.map(|(_, value)| value.clone()).collect()
        };

        let mut values: Generator<Vec<Option<Value>>> = gens::constant(Vec::new());
        for parameter in &self.parameters {
            let resolved = schema::resolve(&self.root, &parameter.schema);
            let plain_string = resolved.get("type").and_then(Value::as_str) == Some("string")
                && resolved.get("enum").is_none()
                && resolved.get("format").is_none();
            let mut value = if parameter.location == "path" && plain_string {
                // An empty segment or a slash would address another path,
                // and so would `.` and `..`, which every URL parser folds
                // away before the request is sent.
                let field = |name| {
                    resolved
                        .get(name)
                        .and_then(Value::as_u64)
                        .map(|n| n as usize)
                };
                let min = field("minLength").unwrap_or(1).max(1);
                let max = field("maxLength").unwrap_or(min + 12);
                schema::text(schema::PATH_TEXT, min, max)
                    .such_that(|segment| segment != "." && segment != "..")
                    .map(Value::String)
            } else {
                schema::generator(&self.root, &parameter.schema)
            };

            if matches!(parameter.location.as_str(), "body" | "form") {
                // A fixture named after a property of the body goes there.
                let properties = resolved.get("properties").and_then(Value::as_object);
                for name in properties.into_iter().flat_map(|p| p.keys()) {
                    let fixed = known(name);
                    if fixed.is_empty() {
                        continue;
                    }
                    let name = name.clone();
                    let choice = gens::option(gens::elements(fixed));
                    value = gens::zip(value, choice).map(move |(mut body, fixed)| {
                        if let (Some(fixed), Some(map)) = (fixed, body.as_object_mut()) {
                            map.insert(name.clone(), fixed);
                        }
                        body
                    });
                }
            } else {
                let fixed = known(&parameter.name);
                if !fixed.is_empty() {
                    // The generated value comes first, so shrinking tries one
                    // in place of a fixture. A fixture itself does not
                    // shrink: it is a value that exists, not a size.
                    value = gens::frequency(vec![(1, value), (1, gens::elements(fixed))]);
                }
            }

            let value = if parameter.required {
                value.map(Some)
            } else {
                gens::option(value)
            };
            values = gens::zip(values, value).map(|(mut all, one)| {
                all.push(one);
                all
            });
        }

        values
    }

    pub(crate) fn build(&self, values: &[Option<Value>]) -> HttpRequest {
        let mut request = HttpRequest {
            method: self.method.clone(),
            path: self.path.clone(),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
        };
        if let Some(accept) = &self.accept {
            request.headers.push(("accept".to_string(), accept.clone()));
        }
        let (mut form, mut cookies) = (Vec::new(), Vec::new());

        for (parameter, value) in self.parameters.iter().zip(values) {
            let Some(value) = value else { continue };
            let name = parameter.name.clone();
            let format = parameter.collection_format.as_str();
            match parameter.location.as_str() {
                "path" => {
                    let segment = percent_encode(&flat(value, format));
                    request.path = request.path.replace(&format!("{{{name}}}"), &segment);
                }
                "query" => spread(&mut request.query, name, value, format),
                "formData" => spread(&mut form, name, value, format),
                "cookie" => spread(&mut cookies, name, value, format),
                "header" => request.headers.push((name, flat(value, format))),
                "body" => {
                    request
                        .headers
                        .push(("content-type".to_string(), self.body_media.clone()));
                    request.body = Some(Body::Json(value.clone()));
                }
                // One object schema describes the whole form.
                "form" => {
                    for (field, member) in value.as_object().into_iter().flatten() {
                        spread(&mut form, field.clone(), member, format);
                    }
                    request.body = Some(Body::Form(Vec::new()));
                }
                _ => {}
            }
        }
        if !cookies.is_empty() {
            let pairs: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
            request
                .headers
                .push(("cookie".to_string(), pairs.join("; ")));
        }
        if !form.is_empty() || request.body == Some(Body::Form(Vec::new())) {
            request.body = Some(Body::Form(form));
        }
        request
    }

    /// The documented response for a status: the exact code, else its
    /// range (`4XX`), else `default`.
    fn documented(&self, status: u16) -> Option<&Documented> {
        let exact = status.to_string();
        let range = format!("{}XX", status / 100);
        let keys = [exact.as_str(), range.as_str(), "DEFAULT"];
        let found = keys
            .into_iter()
            .find_map(|key| self.responses.iter().find(|(k, _)| k == key));
        found.map(|(_, documented)| documented)
    }

    /// The first enabled check the response fails, in the order of
    /// [`Check::ALL`].
    pub fn check(&self, response: &HttpResponse, enabled: &[Check]) -> Result<(), Violation> {
        let fail = |check, message| Err(Violation { check, message });
        let on = |check| enabled.contains(&check);
        let status = response.status;

        // A 5xx the description lists by code or as `5XX` is an answer the
        // API promises, such as 501 for an endpoint it declines to
        // implement. `default` does not count: it would excuse every crash.
        let range = format!("{}XX", status / 100);
        let promised = self
            .responses
            .iter()
            .any(|(key, _)| *key == status.to_string() || *key == range);
        if on(Check::NoServerError) && status >= 500 && !promised {
            return fail(
                Check::NoServerError,
                format!("server error: status {status}"),
            );
        }
        let Some(documented) = self.documented(status) else {
            if on(Check::DocumentedStatus) {
                let mut known: Vec<String> = self
                    .responses
                    .iter()
                    .map(|(k, _)| k.to_lowercase())
                    .collect();
                known.sort();
                return fail(
                    Check::DocumentedStatus,
                    format!(
                        "status {status} is not documented (documented: {})",
                        known.join(", ")
                    ),
                );
            }
            return Ok(());
        };

        let media = response.media_type().unwrap_or_default();
        let has_body = !response.body.is_empty();
        if on(Check::ContentType)
            && has_body
            && !documented.media.is_empty()
            && !documented.media.iter().any(|m| media_matches(m, &media))
        {
            return fail(
                Check::ContentType,
                format!(
                    "content type {:?} is not documented for status {status} ({})",
                    response.header("content-type").unwrap_or(""),
                    documented.media.join(", ")
                ),
            );
        }
        let Some(schema) = &documented.schema else {
            return Ok(());
        };
        if on(Check::ResponseSchema) && (is_json(&media) || media.is_empty()) {
            let body: Value = match serde_json::from_str(&response.body) {
                Ok(body) => body,
                Err(e) => return fail(Check::ResponseSchema, format!("body is not JSON: {e}")),
            };
            let errors = schema::validate(&self.root, schema, &body);
            if !errors.is_empty() {
                return fail(Check::ResponseSchema, errors.join("; "));
            }
        }
        Ok(())
    }
}

fn glob(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, rest)) => {
            text.starts_with(head)
                && (head.len()..=text.len())
                    .filter(|i| text.is_char_boundary(*i))
                    .any(|i| glob(rest, &text[i..]))
        }
    }
}

/// A parameter value as one string. Arrays are joined by the collection
/// format; `multi` is handled by the caller.
pub(crate) fn flat(value: &Value, collection_format: &str) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(items) => {
            let separator = match collection_format {
                "ssv" => " ",
                "tsv" => "\t",
                "pipes" => "|",
                _ => ",",
            };
            let parts: Vec<String> = items.iter().map(|item| flat(item, "csv")).collect();
            parts.join(separator)
        }
        other => other.to_string(),
    }
}

fn spread(
    fields: &mut Vec<(String, String)>,
    name: String,
    value: &Value,
    collection_format: &str,
) {
    match value {
        Value::Array(items) if collection_format == "multi" => {
            for item in items {
                fields.push((name.clone(), flat(item, "csv")));
            }
        }
        _ => fields.push((name, flat(value, collection_format))),
    }
}

/// How a run is configured.
#[derive(Clone, Debug)]
pub struct Options {
    /// Requests per operation.
    pub tests: u32,
    pub seed: Option<u64>,
    /// The most requests to spend shrinking one failure.
    pub max_shrinks: u32,
    pub checks: Vec<Check>,
    /// Values known to exist on the server, by parameter or body property
    /// name. See [`Operation::requests_with`].
    pub fixtures: Vec<(String, Value)>,
    /// How many sequences of calls to run after the single-request checks.
    pub sequences: u32,
    /// The most calls in one sequence.
    pub sequence_length: usize,
    /// A shell command that puts the server back in a known state. It
    /// runs before each sequence.
    pub reset: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tests: 100,
            seed: None,
            max_shrinks: 500,
            checks: Check::ALL.to_vec(),
            fixtures: Vec::new(),
            sequences: 50,
            sequence_length: 8,
            reset: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Failure {
    pub violation: Violation,
    /// The smallest request found that fails the same check.
    pub request: HttpRequest,
    /// The response to that request, when one came back.
    pub response: Option<HttpResponse>,
    /// The request that failed first.
    pub original: HttpRequest,
    pub shrinks: u32,
    /// Requests that passed before the failure.
    pub passed: u32,
}

#[derive(Clone, Debug)]
pub enum Verdict {
    Passed {
        tests: u32,
        /// How many responses had each status, in status order.
        statuses: Vec<(u16, u32)>,
    },
    Failed(Box<Failure>),
    Skipped(String),
}

pub struct Report {
    pub operation: Operation,
    pub verdict: Verdict,
}

/// Runs the generated checks for one operation.
pub fn check_operation(operation: &Operation, adapter: &dyn Adapter, options: &Options) -> Verdict {
    if let Some(reason) = &operation.unsupported {
        return Verdict::Skipped(reason.clone());
    }

    // The latest failing exchange. Shrinking only ever moves to a request
    // that fails, so at the end this is the exchange for the shrunk one.
    let last: RefCell<Option<(Violation, Option<HttpResponse>)>> = RefCell::new(None);
    // The check that failed first. Shrinking keeps to it, so a small
    // request that fails for some other reason is not mistaken for a
    // simpler form of this failure.
    let pinned: RefCell<Option<Check>> = RefCell::new(None);

    // A check that passes on nothing but rejections says little, so the
    // statuses seen are part of the result.
    let statuses: RefCell<std::collections::BTreeMap<u16, u32>> = RefCell::default();

    let property = |request: HttpRequest| {
        let (violation, response) = match send(adapter, &request) {
            Err(message) if options.checks.contains(&Check::Transport) => {
                let check = Check::Transport;
                (Violation { check, message }, None)
            }
            Err(_) => return TestResult::Discard,
            Ok(response) => {
                *statuses.borrow_mut().entry(response.status).or_default() += 1;
                match operation.check(&response, &options.checks) {
                    Ok(()) => return TestResult::Pass,
                    Err(violation) => (violation, Some(response)),
                }
            }
        };
        let target = *pinned.borrow_mut().get_or_insert(violation.check);
        if violation.check != target {
            return TestResult::Pass;
        }
        let message = violation.message.clone();
        *last.borrow_mut() = Some((violation, response));
        TestResult::Fail(message)
    };

    let mut config = QuickCheck::new()
        .tests(options.tests)
        .max_shrinks(options.max_shrinks);
    if let Some(seed) = options.seed {
        config = config.seed(seed);
    }
    match config.run_with(&operation.requests_with(&options.fixtures), property) {
        Outcome::Passed { tests } => Verdict::Passed {
            tests,
            statuses: statuses.into_inner().into_iter().collect(),
        },
        Outcome::GaveUp { .. } => Verdict::Skipped("no request completed".to_string()),
        Outcome::Failed(failure) => {
            let (violation, response) = last.into_inner().expect("a failure was recorded");
            Verdict::Failed(Box::new(Failure {
                violation,
                request: failure.shrunk,
                response,
                original: failure.original,
                shrinks: failure.shrinks,
                passed: failure.passed,
            }))
        }
    }
}

/// Runs the generated checks for every operation that `filter` selects.
pub fn check_spec(
    spec: &Spec,
    adapter: &dyn Adapter,
    options: &Options,
    filter: Option<&str>,
) -> Vec<Report> {
    spec.operations()
        .into_iter()
        .filter(|operation| filter.is_none_or(|f| operation.matches(f)))
        .map(|operation| Report {
            verdict: check_operation(&operation, adapter, options),
            operation,
        })
        .collect()
}
