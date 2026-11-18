//! Checks generated from an OpenAPI 2.0 (Swagger) description.
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
    /// The status is below 500.
    NoServerError,
    /// The status is listed for the operation, or the operation has a
    /// `default` response.
    DocumentedStatus,
    /// A response with a documented schema has a media type from `produces`.
    ContentType,
    /// The body conforms to the schema documented for the status.
    ResponseSchema,
}

impl Check {
    pub const ALL: [Check; 5] = [
        Check::Transport,
        Check::NoServerError,
        Check::DocumentedStatus,
        Check::ContentType,
        Check::ResponseSchema,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Check::Transport => "transport",
            Check::NoServerError => "no-server-error",
            Check::DocumentedStatus => "documented-status",
            Check::ContentType => "content-type",
            Check::ResponseSchema => "response-schema",
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
}

impl Spec {
    /// Parses JSON or YAML.
    pub fn parse(text: &str) -> Result<Spec, String> {
        let root: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => serde_yaml::from_str(text).map_err(|e| format!("not JSON or YAML: {e}"))?,
        };
        match root.get("swagger").and_then(Value::as_str) {
            Some("2.0") => {}
            Some(other) => {
                return Err(format!(
                    "swagger version {other:?} is not supported, only \"2.0\""
                ))
            }
            None if root.get("openapi").is_some() => {
                return Err(
                    "this is an OpenAPI 3 description; only OpenAPI 2.0 is supported".into(),
                )
            }
            None => {
                return Err(
                    "no `swagger: \"2.0\"` field; is this an OpenAPI 2.0 description?".into(),
                )
            }
        }
        Ok(Spec {
            root: Rc::new(root),
        })
    }

    pub fn load(path: &str) -> Result<Spec, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        Spec::parse(&text).map_err(|e| format!("{path}: {e}"))
    }

    /// The base URL the description itself names, when it names a host.
    pub fn base_url(&self) -> Option<String> {
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
                    operations.push(Operation::new(&self.root, path, method, item, operation));
                }
            }
        }
        operations
    }
}

#[derive(Clone)]
struct Parameter {
    name: String,
    location: String,
    required: bool,
    /// The schema of a body, or the parameter object itself, which carries
    /// the same keywords for the other locations.
    schema: Value,
    collection_format: String,
}

/// One method on one path.
#[derive(Clone)]
pub struct Operation {
    root: Rc<Value>,
    pub method: String,
    pub path: String,
    pub operation_id: Option<String>,
    parameters: Vec<Parameter>,
    consumes: Vec<String>,
    produces: Vec<String>,
    responses: Value,
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
    let schema = schema::resolve(root, schema);
    match schema {
        Value::Object(map) => {
            map.contains_key("pattern")
                || map.iter().any(|(key, value)| match key.as_str() {
                    "items" => mentions_pattern(root, value, depth + 1),
                    "properties" => value
                        .as_object()
                        .is_some_and(|p| p.values().any(|v| mentions_pattern(root, v, depth + 1))),
                    "allOf" => value
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| mentions_pattern(root, v, depth + 1))),
                    _ => false,
                })
        }
        _ => false,
    }
}

impl Operation {
    fn new(
        root: &Rc<Value>,
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
            let schema = match location.as_str() {
                "body" => parameter.get("schema").cloned().unwrap_or(json!({})),
                _ => parameter.clone(),
            };
            if mentions_pattern(root, &schema, 0) {
                warnings.push(format!(
                    "`pattern` on parameter {name:?} is not enforced; generated values may not match it"
                ));
            }
            let parsed = Parameter {
                required: location == "path"
                    || parameter.get("required") == Some(&Value::Bool(true)),
                collection_format: match field("collectionFormat") {
                    "" => "csv".to_string(),
                    format => format.to_string(),
                },
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

        let inherited = |name: &str, default: &str| {
            strings(operation.get(name))
                .or_else(|| strings(root.get(name)))
                .unwrap_or_else(|| vec![default.to_string()])
        };
        let consumes = inherited("consumes", "application/json");
        let produces = inherited("produces", "application/json");

        let has = |location: &str| parameters.iter().any(|p| p.location == location);
        if has("body") && !consumes.iter().any(|m| m.contains("json")) {
            unsupported = Some(format!(
                "request bodies of type {} are not supported",
                consumes.join(", ")
            ));
        }
        if has("formData")
            && !consumes
                .iter()
                .any(|m| m == "application/x-www-form-urlencoded")
        {
            unsupported.get_or_insert("multipart form data is not supported".to_string());
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
            consumes,
            produces,
            responses: operation.get("responses").cloned().unwrap_or(json!({})),
            unsupported,
            warnings,
        }
    }

    /// True when `filter` is the operation id or occurs in `METHOD /path`.
    pub fn matches(&self, filter: &str) -> bool {
        self.operation_id.as_deref() == Some(filter)
            || format!("{} {}", self.method, self.path).contains(filter)
    }

    /// A generator of requests the description calls valid. Optional
    /// parameters come and go, and shrink to absent.
    pub fn requests(&self) -> Generator<HttpRequest> {
        let mut values: Generator<Vec<Option<Value>>> = gens::constant(Vec::new());
        for parameter in &self.parameters {
            let resolved = schema::resolve(&self.root, &parameter.schema);
            let plain_string = resolved.get("type").and_then(Value::as_str) == Some("string")
                && resolved.get("enum").is_none()
                && resolved.get("format").is_none();
            let value = if parameter.location == "path" && plain_string {
                // An empty segment or a slash would address another path.
                let field = |name| {
                    resolved
                        .get(name)
                        .and_then(Value::as_u64)
                        .map(|n| n as usize)
                };
                let min = field("minLength").unwrap_or(1).max(1);
                let max = field("maxLength").unwrap_or(min + 12);
                schema::text(schema::PATH_TEXT, min, max).map(Value::String)
            } else {
                schema::generator(&self.root, &parameter.schema)
            };
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

        let operation = self.clone();
        values.map(move |values| operation.build(&values))
    }

    fn build(&self, values: &[Option<Value>]) -> HttpRequest {
        let mut request = HttpRequest {
            method: self.method.clone(),
            path: self.path.clone(),
            query: Vec::new(),
            headers: vec![("accept".to_string(), self.produces.join(", "))],
            body: None,
        };
        let mut form = Vec::new();

        for (parameter, value) in self.parameters.iter().zip(values) {
            let Some(value) = value else { continue };
            let name = parameter.name.clone();
            match parameter.location.as_str() {
                "path" => {
                    let segment = percent_encode(&flat(value, &parameter.collection_format));
                    request.path = request.path.replace(&format!("{{{name}}}"), &segment);
                }
                "query" => spread(
                    &mut request.query,
                    name,
                    value,
                    &parameter.collection_format,
                ),
                "formData" => spread(&mut form, name, value, &parameter.collection_format),
                "header" => request
                    .headers
                    .push((name, flat(value, &parameter.collection_format))),
                "body" => {
                    let media = self.consumes.iter().find(|m| m.contains("json"));
                    let media = media.cloned().unwrap_or("application/json".to_string());
                    request.headers.push(("content-type".to_string(), media));
                    request.body = Some(Body::Json(value.clone()));
                }
                _ => {}
            }
        }
        if !form.is_empty() {
            request.body = Some(Body::Form(form));
        }
        request
    }

    /// The documented response for a status: the exact code, else `default`.
    fn documented(&self, status: u16) -> Option<&Value> {
        let response = self
            .responses
            .get(status.to_string())
            .or_else(|| self.responses.get("default"))?;
        Some(schema::resolve(&self.root, response))
    }

    /// The first enabled check the response fails, in the order of
    /// [`Check::ALL`].
    pub fn check(&self, response: &HttpResponse, enabled: &[Check]) -> Result<(), Violation> {
        let fail = |check, message| Err(Violation { check, message });
        let on = |check| enabled.contains(&check);
        let status = response.status;

        if on(Check::NoServerError) && status >= 500 {
            return fail(
                Check::NoServerError,
                format!("server error: status {status}"),
            );
        }
        let Some(documented) = self.documented(status) else {
            if on(Check::DocumentedStatus) {
                let mut known: Vec<&str> = self
                    .responses
                    .as_object()
                    .map(|r| r.keys().map(String::as_str).collect())
                    .unwrap_or_default();
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
        let Some(schema) = documented.get("schema") else {
            return Ok(());
        };

        let media = response.media_type().unwrap_or_default();
        if on(Check::ContentType) && !self.produces.iter().any(|m| m.eq_ignore_ascii_case(&media)) {
            return fail(
                Check::ContentType,
                format!(
                    "content type {:?} is not in produces ({})",
                    response.header("content-type").unwrap_or(""),
                    self.produces.join(", ")
                ),
            );
        }
        if on(Check::ResponseSchema) && (media.contains("json") || media.is_empty()) {
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

/// A parameter value as one string. Arrays are joined by the collection
/// format; `multi` is handled by the caller.
fn flat(value: &Value, collection_format: &str) -> String {
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
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tests: 100,
            seed: None,
            max_shrinks: 500,
            checks: Check::ALL.to_vec(),
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
    Passed { tests: u32 },
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

    let property = |request: HttpRequest| {
        let (violation, response) = match send(adapter, &request) {
            Err(message) if options.checks.contains(&Check::Transport) => {
                let check = Check::Transport;
                (Violation { check, message }, None)
            }
            Err(_) => return TestResult::Discard,
            Ok(response) => match operation.check(&response, &options.checks) {
                Ok(()) => return TestResult::Pass,
                Err(violation) => (violation, Some(response)),
            },
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
    match config.run_with(&operation.requests(), property) {
        Outcome::Passed { tests } => Verdict::Passed { tests },
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
