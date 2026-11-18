//! Sequences of calls, where a later request uses what an earlier response
//! returned.
//!
//! A single generated request can only ask for ids that do not exist. A
//! sequence creates a resource, reads the id from the response, and uses it
//! in the calls that follow. That reaches the code behind "found", and it
//! allows checks that need more than one call: a deleted resource stays
//! deleted, a created one can be read.

use crate::adapter::{percent_encode, send, Adapter, HttpRequest};
use crate::openapi::{flat, Check, Operation, Options, Parameter, Violation};
use qcheck::{gens, Generator, Outcome, QuickCheck, TestResult};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The most values remembered per name.
const POOL_LIMIT: usize = 50;

/// One planned call: an operation, a generated value per parameter, and
/// per parameter whether to use a value from an earlier response instead.
#[derive(Clone, Debug)]
struct Step {
    operation: usize,
    values: Vec<Option<Value>>,
    /// `Some(k)`: take the k-th remembered value for this parameter, when
    /// there is one. The generated value stays as the fallback, so a plan
    /// means something whatever the server returns.
    reuse: Vec<Option<usize>>,
}

/// One call as it was made.
#[derive(Clone, Debug)]
pub struct Call {
    pub request: HttpRequest,
    /// `None` when no response came back.
    pub status: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct SequenceFailure {
    pub violation: Violation,
    /// The shortest sequence found that fails the same check. The last
    /// call is the one that failed.
    pub calls: Vec<Call>,
    pub shrinks: u32,
    /// Sequences that passed before the failure.
    pub passed: u32,
}

#[derive(Clone, Debug)]
pub enum SequenceVerdict {
    Passed {
        sequences: u32,
        calls: u32,
        /// Calls that used a value from an earlier response.
        reused: u32,
        statuses: Vec<(u16, u32)>,
    },
    Failed(Box<SequenceFailure>),
    Skipped(String),
}

/// The path up to its first parameter: `/pets/{id}/toys` gives `/pets`.
/// Operations that share it work on the same kind of resource.
fn resource(path: &str) -> &str {
    match path.find("/{") {
        Some(end) => &path[..end],
        None => path,
    }
}

/// The path without trailing parameters: `/pets/{id}` gives `/pets`, and
/// `/pets/{id}/toys/{toy}` gives `/pets/{id}/toys`. It names the collection
/// an operation's response describes members of.
fn collection(path: &str) -> &str {
    let mut path = path;
    while path.ends_with('}') {
        match path.rfind("/{") {
            Some(cut) => path = &path[..cut],
            None => break,
        }
    }
    path
}

/// Values seen in responses, for later requests to use.
#[derive(Default)]
struct Pool {
    by_collection: HashMap<(String, String), Vec<Value>>,
    by_name: HashMap<String, Vec<Value>>,
}

impl Pool {
    fn add(&mut self, collection: &str, name: &str, value: &Value) {
        let scoped = self
            .by_collection
            .entry((collection.to_string(), name.to_string()))
            .or_default();
        let global = self.by_name.entry(name.to_string()).or_default();
        for list in [scoped, global] {
            if !list.contains(value) && list.len() < POOL_LIMIT {
                list.push(value.clone());
            }
        }
    }

    /// Remembers the strings and numbers in a response body, by property
    /// name, down to two levels of nesting.
    fn harvest(&mut self, collection: &str, body: &Value, depth: usize) {
        match body {
            Value::Object(map) => {
                for (name, member) in map {
                    match member {
                        Value::Number(_) => self.add(collection, name, member),
                        Value::String(text) if !text.is_empty() && text.len() <= 200 => {
                            self.add(collection, name, member)
                        }
                        Value::Object(_) | Value::Array(_) if depth < 2 => {
                            self.harvest(collection, member, depth + 1)
                        }
                        _ => {}
                    }
                }
            }
            Value::Array(items) => {
                for item in items.iter().take(10) {
                    self.harvest(collection, item, depth);
                }
            }
            _ => {}
        }
    }

    /// Remembered values that fit a parameter, best match first.
    ///
    /// A path parameter `{id}` in `/pets/{id}` is looked up among the
    /// values that responses on `/pets` returned, under its own name and
    /// under `id`. Other parameters match by name; `pet_id` and `petId`
    /// also match the `id` of a collection called `pet` or `pets`.
    fn candidates(&self, operation: &Operation, parameter: &Parameter) -> &[Value] {
        let scoped = |collection: &str, name: &str| {
            self.by_collection
                .get(&(collection.to_string(), name.to_string()))
        };
        let name = parameter.name.as_str();

        let found = if parameter.location == "path" {
            let marker = format!("/{{{name}}}");
            let prefix = operation.path.split(&marker).next().unwrap_or("");
            scoped(prefix, name)
                .or_else(|| scoped(prefix, "id"))
                .or_else(|| self.by_name.get(name))
        } else {
            self.by_name.get(name).or_else(|| {
                let base = ["_id", "Id", "ID"]
                    .iter()
                    .find_map(|suffix| name.strip_suffix(suffix))?
                    .to_lowercase();
                self.by_collection.iter().find_map(|((path, key), values)| {
                    let last = path.rsplit('/').next()?.to_lowercase();
                    (key == "id" && (last == base || last == format!("{base}s"))).then_some(values)
                })
            })
        };
        found.map(Vec::as_slice).unwrap_or(&[])
    }
}

fn step(index: usize, operation: &Operation, fixtures: &[(String, Value)]) -> Generator<Step> {
    let count = operation.parameters.len();
    // Mostly reuse: a random id almost never exists. `None` comes first, so
    // shrinking drops a reuse that the failure does not need.
    let reuse = gens::frequency(vec![
        (1, gens::constant(None)),
        (3, gens::range(0usize, 7).map(Some)),
    ]);
    gens::zip(
        operation.values(fixtures),
        gens::vec_len(reuse, count, count),
    )
    .map(move |(values, reuse)| Step {
        operation: index,
        values,
        reuse,
    })
}

/// Sequences stay within one kind of resource, since a call on `/pets`
/// has nothing to hand to a call on `/orders`. Operations that are alone
/// with their resource share one group.
fn plans(operations: &[(Operation, Vec<Check>)], options: &Options) -> Generator<Vec<Step>> {
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, (operation, _)) in operations.iter().enumerate() {
        groups
            .entry(resource(&operation.path))
            .or_default()
            .push(index);
    }
    let (mut related, mut alone): (Vec<Vec<usize>>, Vec<usize>) = (Vec::new(), Vec::new());
    for members in groups.into_values() {
        match members.len() {
            1 => alone.extend(members),
            _ => related.push(members),
        }
    }
    if !alone.is_empty() {
        related.push(alone);
    }

    let length = options.sequence_length.max(1);
    let choices = related.into_iter().map(|members| {
        let steps = members
            .iter()
            .map(|&index| step(index, &operations[index].0, &options.fixtures));
        let weight = members.len() as u32;
        (
            weight,
            gens::vec_len(gens::one_of(steps.collect()), 1, length),
        )
    });
    gens::frequency(choices.collect())
}

/// What one execution of a plan saw.
#[derive(Default)]
struct Tally {
    calls: u32,
    reused: u32,
    statuses: BTreeMap<u16, u32>,
}

/// Where a POST on a collection puts the resource it created, when the
/// description has an operation to read one: `/pets` and `GET /pets/{id}`.
fn created_path(
    operations: &[(Operation, Vec<Check>)],
    operation: &Operation,
    request: &HttpRequest,
    location: Option<&str>,
    body: Option<&Value>,
) -> Option<String> {
    let prefix = format!("{}/{{", operation.path);
    let reader = operations.iter().map(|(o, _)| o).find(|o| {
        o.method == "GET"
            && o.path.starts_with(&prefix)
            && o.path.ends_with('}')
            && !o.path[prefix.len()..].contains('/')
    })?;
    if let Some(location) = location {
        let id = location.trim_end_matches('/').rsplit('/').next()?;
        return Some(format!("{}/{id}", request.path));
    }
    let name = &reader.path[prefix.len()..reader.path.len() - 1];
    let id = body?.get(name).or_else(|| body?.get("id"))?;
    (id.is_string() || id.is_number())
        .then(|| format!("{}/{}", request.path, percent_encode(&flat(id, "csv"))))
}

/// Runs one plan against the server. The trace holds every call made, so
/// it ends with the call that failed.
fn execute(
    plan: &[Step],
    operations: &[(Operation, Vec<Check>)],
    adapter: &dyn Adapter,
    options: &Options,
    trace: &mut Vec<Call>,
    tally: &mut Tally,
) -> Result<(), Violation> {
    let fail = |check, message| Err(Violation { check, message });

    if let Some(command) = &options.reset {
        let done = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .status();
        if !done.is_ok_and(|status| status.success()) {
            return fail(
                Check::Transport,
                format!("the reset command failed: {command}"),
            );
        }
    }

    let mut pool = Pool::default();
    let (mut deleted, mut created) = (BTreeSet::new(), BTreeSet::new());

    for step in plan {
        let (operation, checks) = &operations[step.operation];
        let on = |check| checks.contains(&check);

        let mut values = step.values.clone();
        let mut reused = false;
        for (i, parameter) in operation.parameters.iter().enumerate() {
            let (Some(Some(pick)), Some(Some(value))) = (step.reuse.get(i), values.get_mut(i))
            else {
                continue;
            };
            if matches!(parameter.location.as_str(), "body" | "form") {
                // Properties of a body are matched one by one, by name.
                for (name, member) in value.as_object_mut().into_iter().flatten() {
                    if let Some(known) = pool.by_name.get(name) {
                        *member = known[pick % known.len()].clone();
                        reused = true;
                    }
                }
            } else {
                let known = pool.candidates(operation, parameter);
                if !known.is_empty() {
                    *value = known[pick % known.len()].clone();
                    reused = true;
                }
            }
        }

        let request = operation.build(&values);
        tally.calls += 1;
        tally.reused += reused as u32;
        let response = match send(adapter, &request) {
            Ok(response) => response,
            Err(message) => {
                trace.push(Call {
                    request,
                    status: None,
                });
                if on(Check::Transport) {
                    return fail(Check::Transport, message);
                }
                continue;
            }
        };
        let status = response.status;
        *tally.statuses.entry(status).or_default() += 1;
        trace.push(Call {
            request: request.clone(),
            status: Some(status),
        });
        operation.check(&response, checks)?;

        let succeeded = (200..300).contains(&status);
        let path = request.path.clone();
        // Only a path that ends in a parameter names one resource. A GET
        // on a collection may well succeed after a DELETE emptied it.
        let single = operation.path.ends_with('}');
        let body: Option<Value> = serde_json::from_str(&response.body).ok();

        match request.method.as_str() {
            "GET" if single => {
                if succeeded && deleted.contains(&path) && on(Check::DeletedStaysDeleted) {
                    return fail(
                        Check::DeletedStaysDeleted,
                        format!("GET {path} answered {status} after DELETE {path} succeeded"),
                    );
                }
                let missing = status == 404 && created.contains(&path) && !deleted.contains(&path);
                if missing && on(Check::CreatedIsReadable) {
                    return fail(
                        Check::CreatedIsReadable,
                        format!("GET {path} answered 404 after a POST created it"),
                    );
                }
            }
            "DELETE" if single && succeeded => {
                deleted.insert(path);
            }
            "PUT" if succeeded => {
                deleted.remove(&path);
            }
            "POST" if succeeded => {
                let location = response.header("location");
                let made = created_path(operations, operation, &request, location, body.as_ref());
                if let Some(made) = made {
                    deleted.remove(&made);
                    created.insert(made);
                }
            }
            _ => {}
        }

        if succeeded {
            let collection = collection(&operation.path);
            if let Some(body) = &body {
                pool.harvest(collection, body, 0);
            }
            let id = response
                .header("location")
                .and_then(|location| location.trim_end_matches('/').rsplit('/').next());
            if let Some(id) = id {
                pool.add(collection, "id", &Value::String(id.to_string()));
            }
        }
    }
    Ok(())
}

/// Runs sequences of calls over the given operations, each with the
/// checks that apply to it.
pub fn check_sequences(
    operations: &[(Operation, Vec<Check>)],
    adapter: &dyn Adapter,
    options: &Options,
) -> SequenceVerdict {
    if options.sequences == 0 {
        return SequenceVerdict::Skipped("sequences are turned off".to_string());
    }
    if operations.is_empty() {
        return SequenceVerdict::Skipped("no operation to call".to_string());
    }

    let tally = RefCell::new(Tally::default());
    // As for a single operation: the latest failing run, and the check
    // that failed first, which shrinking keeps to.
    let last: RefCell<Option<(Violation, Vec<Call>)>> = RefCell::new(None);
    let pinned: RefCell<Option<Check>> = RefCell::new(None);

    let property = |plan: Vec<Step>| {
        let mut trace = Vec::new();
        let outcome = execute(
            &plan,
            operations,
            adapter,
            options,
            &mut trace,
            &mut tally.borrow_mut(),
        );
        let Err(violation) = outcome else {
            return TestResult::Pass;
        };
        let target = *pinned.borrow_mut().get_or_insert(violation.check);
        if violation.check != target {
            return TestResult::Pass;
        }
        let message = violation.message.clone();
        *last.borrow_mut() = Some((violation, trace));
        TestResult::Fail(message)
    };

    let mut config = QuickCheck::new()
        .tests(options.sequences)
        .max_shrinks(options.max_shrinks);
    if let Some(seed) = options.seed {
        config = config.seed(seed);
    }
    match config.run_with(&plans(operations, options), property) {
        Outcome::Passed { tests } => {
            let tally = tally.into_inner();
            SequenceVerdict::Passed {
                sequences: tests,
                calls: tally.calls,
                reused: tally.reused,
                statuses: tally.statuses.into_iter().collect(),
            }
        }
        Outcome::GaveUp { .. } => SequenceVerdict::Skipped("no sequence completed".to_string()),
        Outcome::Failed(failure) => {
            let (violation, calls) = last.into_inner().expect("a failure was recorded");
            SequenceVerdict::Failed(Box::new(SequenceFailure {
                violation,
                calls,
                shrinks: failure.shrinks,
                passed: failure.passed,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_reduce_to_their_resource_and_collection() {
        assert_eq!(resource("/pets"), "/pets");
        assert_eq!(resource("/pets/{id}/toys/{toy}"), "/pets");
        assert_eq!(collection("/pets"), "/pets");
        assert_eq!(collection("/pets/{id}"), "/pets");
        assert_eq!(collection("/pets/{id}/toys"), "/pets/{id}/toys");
        assert_eq!(collection("/pets/{id}/toys/{toy}"), "/pets/{id}/toys");
        assert_eq!(collection("/{tenant}/{id}"), "");
    }

    #[test]
    fn the_pool_keeps_scalars_by_name_and_skips_the_rest() {
        let mut pool = Pool::default();
        let body = serde_json::json!({
            "id": 7, "name": "rex", "empty": "", "ok": true,
            "owner": {"id": 3},
            "toys": [{"toy_id": "t1"}, {"toy_id": "t2"}]
        });
        pool.harvest("/pets", &body, 0);
        assert_eq!(pool.by_name["id"], [7, 3]);
        assert_eq!(pool.by_name["name"], ["rex"]);
        assert_eq!(pool.by_name["toy_id"], ["t1", "t2"]);
        assert!(!pool.by_name.contains_key("empty") && !pool.by_name.contains_key("ok"));
    }
}
