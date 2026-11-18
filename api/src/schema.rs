//! OpenAPI 2.0 schemas in both directions: a generator of values that
//! satisfy a schema, and a validator that says how a value breaks one.

use qcheck::{gens, Generator};
use serde_json::{json, Map, Value};
use std::rc::Rc;

/// `$ref` chains longer than this are treated as a cycle.
const MAX_REF_DEPTH: usize = 12;
/// Past this many `$ref` hops, optional parts of a value are left out, so
/// recursive schemas produce finite values.
const SHALLOW_AFTER: usize = 3;

const TEXT: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -_.,:;/?&=%+#'\"<>{}[]\\\u{e9}\u{df}\u{4e2d}\u{1f600}";
/// Characters that survive in a URL path segment on every server we know of.
pub const PATH_TEXT: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.~";

/// Follows a local reference such as `#/definitions/Pet`.
pub fn lookup<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    root.pointer(reference.strip_prefix('#')?)
}

/// The schema a `$ref` chain ends at, or the schema itself.
pub fn resolve<'a>(root: &'a Value, mut schema: &'a Value) -> &'a Value {
    for _ in 0..MAX_REF_DEPTH {
        match schema.get("$ref").and_then(Value::as_str) {
            Some(reference) => match lookup(root, reference) {
                Some(target) => schema = target,
                None => return &Value::Null,
            },
            None => break,
        }
    }
    schema
}

/// Folds `allOf` into one schema: properties and required lists are joined,
/// and other keywords are taken from the first schema that has them.
fn flatten(root: &Value, schema: &Value) -> Value {
    let schema = resolve(root, schema);
    let Some(parts) = schema.get("allOf").and_then(Value::as_array) else {
        return schema.clone();
    };
    let mut merged = schema.as_object().cloned().unwrap_or_default();
    merged.remove("allOf");
    for part in parts {
        let Value::Object(part) = flatten(root, part) else {
            continue;
        };
        for (key, value) in part {
            match (key.as_str(), merged.get_mut(&key)) {
                ("properties", Some(Value::Object(mine))) => {
                    if let Value::Object(theirs) = value {
                        for (name, property) in theirs {
                            mine.entry(name).or_insert(property);
                        }
                    }
                }
                ("required", Some(Value::Array(mine))) => {
                    if let Value::Array(theirs) = value {
                        for name in theirs {
                            if !mine.contains(&name) {
                                mine.push(name);
                            }
                        }
                    }
                }
                (_, Some(_)) => {}
                (_, None) => {
                    merged.insert(key, value);
                }
            }
        }
    }
    Value::Object(merged)
}

/// The types a schema allows. OpenAPI 3.1 may list several, and `null`
/// among them.
fn types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ if schema.get("properties").is_some() => vec!["object"],
        _ if schema.get("items").is_some() => vec!["array"],
        _ => vec!["any"],
    }
}

/// Null is allowed by a `null` type (3.1), `nullable` (3.0), or the
/// `x-nullable` extension (2.0).
fn nullable(schema: &Value) -> bool {
    let flag = |name| schema.get(name) == Some(&Value::Bool(true));
    types(schema).contains(&"null") || flag("nullable") || flag("x-nullable")
}

/// A generator of JSON values that satisfy `schema`.
///
/// Not enforced: `pattern`, and `uniqueItems` when removing duplicates
/// would go below `minItems`.
pub fn generator(root: &Rc<Value>, schema: &Value) -> Generator<Value> {
    generator_at(root, schema, 0)
}

fn generator_at(root: &Rc<Value>, schema: &Value, depth: usize) -> Generator<Value> {
    if depth > MAX_REF_DEPTH {
        return gens::constant(Value::Null);
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        // Built on demand, so a schema that refers to itself costs nothing
        // until a value needs that branch.
        let target = lookup(root, reference).cloned().unwrap_or(Value::Null);
        let root = root.clone();
        return gens::lazy(move || generator_at(&root, &target, depth + 1));
    }

    let schema = flatten(root, schema);
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.is_empty() {
            return gens::elements(values.clone());
        }
    }

    if let Some(value) = schema.get("const") {
        return gens::constant(value.clone());
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(alternatives) = schema.get(key).and_then(Value::as_array) {
            if !alternatives.is_empty() {
                let choices = alternatives.iter().map(|a| generator_at(root, a, depth));
                return gens::one_of(choices.collect());
            }
        }
    }

    let mut choices: Vec<Generator<Value>> = types(&schema)
        .into_iter()
        .filter(|name| *name != "null")
        .map(|name| typed(root, &schema, name, depth))
        .collect();
    let value = match choices.len() {
        0 => return gens::constant(Value::Null),
        1 => choices.remove(0),
        _ => gens::one_of(choices),
    };
    if nullable(&schema) {
        // The value comes first, so a nullable field shrinks to a simple
        // value and reports null only when null is what fails.
        return gens::frequency(vec![(4, value), (1, gens::constant(Value::Null))]);
    }
    value
}

fn typed(root: &Rc<Value>, schema: &Value, name: &str, depth: usize) -> Generator<Value> {
    match name {
        "integer" => integer(schema),
        "number" => number(schema),
        "string" => string(schema),
        "boolean" => gens::any::<bool>().map(Value::Bool),
        "array" => array(root, schema, depth),
        "object" => object(root, schema, depth),
        _ => gens::one_of(vec![
            gens::constant(Value::Null),
            gens::any::<bool>().map(Value::Bool),
            gens::range(-100i64, 100).map(|n| json!(n)),
            text(TEXT, 0, 8).map(Value::String),
        ]),
    }
}

/// Mostly small values, sometimes anything in range, sometimes a bound.
fn spread(lo: i64, hi: i64) -> Generator<i64> {
    let near = gens::range((-100).clamp(lo, hi), 100.clamp(lo, hi));
    gens::frequency(vec![
        (4, near),
        (1, gens::range(lo, hi)),
        (1, gens::elements(vec![lo, hi])),
    ])
}

/// The inclusive bounds of a numeric schema in units of `1 / scale`.
fn bounds(schema: &Value, scale: f64, default: (i64, i64)) -> (i64, i64) {
    let number = |name| schema.get(name).and_then(Value::as_f64);
    let flag = |name| schema.get(name) == Some(&Value::Bool(true));
    let (mut lo, mut hi) = default;

    if let Some(min) = number("minimum") {
        let scaled = (min * scale).ceil();
        lo = lo.max(scaled as i64);
        // OpenAPI 2.0 and 3.0: a flag beside `minimum`.
        if flag("exclusiveMinimum") && scaled == min * scale {
            lo += 1;
        }
    }
    // OpenAPI 3.1: the bound itself.
    if let Some(min) = number("exclusiveMinimum") {
        lo = lo.max((min * scale).floor() as i64 + 1);
    }
    if let Some(max) = number("maximum") {
        let scaled = (max * scale).floor();
        hi = hi.min(scaled as i64);
        if flag("exclusiveMaximum") && scaled == max * scale {
            hi -= 1;
        }
    }
    if let Some(max) = number("exclusiveMaximum") {
        hi = hi.min((max * scale).ceil() as i64 - 1);
    }
    (lo, hi.max(lo))
}

fn integer(schema: &Value) -> Generator<Value> {
    let default = match schema.get("format").and_then(Value::as_str) {
        Some("int32") => (i32::MIN as i64, i32::MAX as i64),
        // The integers every JSON implementation represents exactly.
        _ => (-(1 << 53), 1 << 53),
    };
    let (lo, hi) = bounds(schema, 1.0, default);
    let step = match schema.get("multipleOf").and_then(Value::as_i64) {
        Some(step) if step > 0 => step,
        _ => 1,
    };
    let (first, last) = (-(-lo).div_euclid(step), hi.div_euclid(step));
    if first > last {
        return gens::constant(json!(lo));
    }
    spread(first, last).map(move |k| json!(k * step))
}

fn number(schema: &Value) -> Generator<Value> {
    // Hundredths give fractions without the noise of full float precision.
    let (lo, hi) = bounds(schema, 100.0, (-100_000_000, 100_000_000));
    spread(lo, hi).map(|k| json!(k as f64 / 100.0))
}

/// A string of `min..=max` characters drawn from `alphabet`. Shrinks toward
/// the front of the alphabet.
pub fn text(alphabet: &str, min: usize, max: usize) -> Generator<String> {
    let letters: Vec<char> = alphabet.chars().collect();
    gens::vec_len(gens::elements(letters), min, max.max(min)).map(|cs| cs.into_iter().collect())
}

fn length_bounds(schema: &Value, at_least: usize) -> (usize, usize) {
    let field = |name| schema.get(name).and_then(Value::as_u64).map(|n| n as usize);
    let min = field("minLength").unwrap_or(0).max(at_least);
    let max = field("maxLength").unwrap_or(min + 12).max(min);
    (min, max)
}

fn string(schema: &Value) -> Generator<Value> {
    let two = |n: u32| format!("{n:02}");
    let date = || {
        gens::zip3(
            gens::range(1970u32, 2099),
            gens::range(1u32, 12),
            gens::range(1u32, 28),
        )
        .map(move |(y, m, d)| format!("{y}-{}-{}", two(m), two(d)))
    };
    let generated = match schema.get("format").and_then(Value::as_str) {
        Some("date") => date(),
        Some("date-time") => gens::zip(
            date(),
            gens::zip3(
                gens::range(0u32, 23),
                gens::range(0u32, 59),
                gens::range(0u32, 59),
            ),
        )
        .map(move |(day, (h, m, s))| format!("{day}T{}:{}:{}Z", two(h), two(m), two(s))),
        Some("uuid") => gens::zip(gens::any::<u64>(), gens::any::<u64>()).map(|(a, b)| {
            let hex = format!("{a:016x}{b:016x}");
            format!(
                "{}-{}-4{}-a{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[13..16],
                &hex[17..20],
                &hex[20..]
            )
        }),
        Some("email") => text("abcdefghijklmnopqrstuvwxyz0123456789", 1, 10)
            .map(|local| format!("{local}@example.com")),
        Some("byte") => {
            let groups = ["QUJD", "ZGVm", "MTIz", "AAAA", "/+8="];
            gens::vec_len(gens::elements(groups.to_vec()), 0, 4).map(|g| g.concat())
        }
        _ => {
            let (min, max) = length_bounds(schema, 0);
            text(TEXT, min, max)
        }
    };
    generated.map(Value::String)
}

fn array(root: &Rc<Value>, schema: &Value, depth: usize) -> Generator<Value> {
    let field = |name| schema.get(name).and_then(Value::as_u64).map(|n| n as usize);
    let min = field("minItems").unwrap_or(0);
    let room = if depth > SHALLOW_AFTER { 0 } else { 5 };
    let max = field("maxItems").unwrap_or(min + room).max(min);
    let unique = schema.get("uniqueItems") == Some(&Value::Bool(true));

    let items = schema.get("items").cloned().unwrap_or(json!({}));
    gens::vec_len(generator_at(root, &items, depth), min, max).map(move |mut values| {
        if unique {
            let mut seen: Vec<Value> = Vec::new();
            for value in &values {
                if !seen.contains(value) {
                    seen.push(value.clone());
                }
            }
            if seen.len() >= min {
                values = seen;
            }
        }
        Value::Array(values)
    })
}

fn object(root: &Rc<Value>, schema: &Value, depth: usize) -> Generator<Value> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut fields: Generator<Map<String, Value>> = gens::constant(Map::new());
    let properties = schema.get("properties").and_then(Value::as_object);
    for (name, property) in properties.into_iter().flatten() {
        let value = generator_at(root, property, depth);
        let value = if required.contains(&name.as_str()) {
            value.map(Some)
        } else if depth > SHALLOW_AFTER {
            continue;
        } else {
            // An optional property shrinks to absent.
            gens::option(value)
        };
        let name = name.clone();
        fields = gens::zip(fields, value).map(move |(mut map, value)| {
            if let Some(value) = value {
                map.insert(name.clone(), value);
            }
            map
        });
    }
    fields.map(Value::Object)
}

/// Every way `value` breaks `schema`, each with the JSON path it applies
/// to. An empty list means the value conforms.
pub fn validate(root: &Value, schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    check(root, schema, value, "$", 0, &mut errors);
    errors
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn check(
    root: &Value,
    schema: &Value,
    value: &Value,
    at: &str,
    depth: usize,
    errors: &mut Vec<String>,
) {
    // Values are finite, so the depth only guards against reference loops
    // that never consume any of the value.
    if depth > 200 {
        return;
    }
    if schema.get("$ref").is_some() {
        let target = resolve(root, schema);
        if target.is_null() {
            errors.push(format!("{at}: the schema has an unresolved $ref"));
            return;
        }
        return check(root, target, value, at, depth + 1, errors);
    }
    for part in schema
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        check(root, part, value, at, depth + 1, errors);
    }

    // At least one alternative must accept the value. `oneOf` asks for
    // exactly one, which descriptions with overlapping alternatives rarely
    // mean, so a second match is not reported.
    for key in ["oneOf", "anyOf"] {
        if let Some(alternatives) = schema.get(key).and_then(Value::as_array) {
            let accepted = alternatives.iter().any(|alternative| {
                let mut found = Vec::new();
                check(root, alternative, value, at, depth + 1, &mut found);
                found.is_empty()
            });
            if !accepted && !alternatives.is_empty() {
                errors.push(format!("{at}: {value} matches none of the {key} schemas"));
            }
        }
    }
    if let Some(constant) = schema.get("const") {
        if constant != value {
            errors.push(format!(
                "{at}: expected the constant {constant}, got {value}"
            ));
        }
        return;
    }

    let allowed = types(schema);
    let expected = allowed.join(" or ");
    if value.is_null() {
        if !nullable(schema) && !allowed.contains(&"any") {
            errors.push(format!("{at}: expected {expected}, got null"));
        }
        return;
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            errors.push(format!("{at}: {value} is not one of the enum values"));
        }
    }

    let actual = type_name(value);
    let fits = allowed.iter().any(|name| match *name {
        "any" | "file" => true,
        "integer" => actual == "integer" || value.as_f64().is_some_and(|f| f.fract() == 0.0),
        "number" => actual == "integer" || actual == "number",
        name => name == actual,
    });
    if !fits {
        errors.push(format!("{at}: expected {expected}, got {actual}"));
        return;
    }

    let limit = |name: &str| schema.get(name).and_then(Value::as_f64);
    let flag = |name: &str| schema.get(name) == Some(&Value::Bool(true));
    match value {
        Value::Number(n) => {
            let n = n.as_f64().unwrap_or(0.0);
            if let Some(min) = limit("minimum") {
                if n < min || (flag("exclusiveMinimum") && n == min) {
                    errors.push(format!("{at}: {n} is below the minimum {min}"));
                }
            }
            if let Some(max) = limit("maximum") {
                if n > max || (flag("exclusiveMaximum") && n == max) {
                    errors.push(format!("{at}: {n} is above the maximum {max}"));
                }
            }
            if limit("exclusiveMinimum").is_some_and(|min| n <= min) {
                errors.push(format!("{at}: {n} is not above the exclusive minimum"));
            }
            if limit("exclusiveMaximum").is_some_and(|max| n >= max) {
                errors.push(format!("{at}: {n} is not below the exclusive maximum"));
            }
        }
        Value::String(s) => {
            let length = s.chars().count() as f64;
            if limit("minLength").is_some_and(|min| length < min) {
                errors.push(format!("{at}: string is shorter than minLength"));
            }
            if limit("maxLength").is_some_and(|max| length > max) {
                errors.push(format!("{at}: string is longer than maxLength"));
            }
        }
        Value::Array(items) => {
            let length = items.len() as f64;
            if limit("minItems").is_some_and(|min| length < min) {
                errors.push(format!("{at}: array has fewer items than minItems"));
            }
            if limit("maxItems").is_some_and(|max| length > max) {
                errors.push(format!("{at}: array has more items than maxItems"));
            }
            if let Some(item_schema) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    check(
                        root,
                        item_schema,
                        item,
                        &format!("{at}[{i}]"),
                        depth + 1,
                        errors,
                    );
                }
            }
        }
        Value::Object(map) => {
            for name in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(name) = name.as_str() {
                    if !map.contains_key(name) {
                        errors.push(format!("{at}: required property {name:?} is missing"));
                    }
                }
            }
            let properties = schema.get("properties").and_then(Value::as_object);
            for (name, member) in map {
                let at = format!("{at}.{name}");
                match properties.and_then(|p| p.get(name)) {
                    Some(property) => check(root, property, member, &at, depth + 1, errors),
                    None => match schema.get("additionalProperties") {
                        Some(Value::Bool(false)) => {
                            errors.push(format!("{at}: property is not in the schema"))
                        }
                        Some(extra @ Value::Object(_)) => {
                            check(root, extra, member, &at, depth + 1, errors)
                        }
                        _ => {}
                    },
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qcheck::forall;

    fn root() -> Rc<Value> {
        Rc::new(json!({
            "definitions": {
                "Pet": {
                    "type": "object",
                    "required": ["id", "name"],
                    "properties": {
                        "id": {"type": "integer", "format": "int64", "minimum": 1},
                        "name": {"type": "string", "minLength": 1, "maxLength": 20},
                        "age": {"type": "number", "minimum": 0, "maximum": 30, "exclusiveMaximum": true},
                        "status": {"type": "string", "enum": ["available", "sold"]},
                        "born": {"type": "string", "format": "date"},
                        "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 3, "uniqueItems": true},
                        "even": {"type": "integer", "minimum": -7, "maximum": 9, "multipleOf": 2}
                    },
                    "additionalProperties": false
                },
                "Tree": {
                    "type": "object",
                    "required": ["label"],
                    "properties": {
                        "label": {"type": "string"},
                        "children": {"type": "array", "items": {"$ref": "#/definitions/Tree"}}
                    }
                },
                "Event": {
                    "type": "object",
                    "required": ["object", "note", "size", "payload"],
                    "properties": {
                        "object": {"const": "event"},
                        "note": {"type": ["string", "null"], "maxLength": 5},
                        "size": {"type": "integer", "exclusiveMinimum": 0, "exclusiveMaximum": 10},
                        "payload": {"oneOf": [
                            {"type": "string", "enum": ["none"]},
                            {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}}}
                        ]}
                    }
                },
                "Named": {"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}},
                "Dog": {
                    "allOf": [
                        {"$ref": "#/definitions/Named"},
                        {"type": "object", "required": ["breed"], "properties": {"breed": {"type": "string"}}}
                    ]
                }
            }
        }))
    }

    /// The generator and the validator are written separately, so each
    /// checks the other: every generated value must validate.
    fn generated_values_validate(name: &str) {
        let root = root();
        let schema = json!({ "$ref": format!("#/definitions/{name}") });
        let doc = root.clone();
        forall(generator(&root, &schema), move |value| {
            let errors = validate(&doc, &schema, &value);
            assert!(errors.is_empty(), "{value} breaks the schema: {errors:?}");
        });
    }

    #[test]
    fn generated_pets_validate() {
        generated_values_validate("Pet");
    }

    #[test]
    fn recursive_schemas_generate_finite_values() {
        generated_values_validate("Tree");
    }

    #[test]
    fn openapi_3_1_keywords_generate_and_validate() {
        generated_values_validate("Event");
        let root = root();
        let event = json!({"$ref": "#/definitions/Event"});
        let value = json!({"object": "other", "note": 7, "size": 10, "payload": {"id": "x"}});
        let mut errors = validate(&root, &event, &value);
        errors.sort();
        assert_eq!(
            errors,
            [
                "$.note: expected string or null, got integer",
                "$.object: expected the constant \"event\", got \"other\"",
                "$.payload: {\"id\":\"x\"} matches none of the oneOf schemas",
                "$.size: 10 is not below the exclusive maximum",
            ]
        );
        let fine = json!({"object": "event", "note": null, "size": 9, "payload": "none"});
        assert_eq!(validate(&root, &event, &fine), Vec::<String>::new());
    }

    #[test]
    fn all_of_joins_properties_and_required() {
        generated_values_validate("Dog");
        let root = root();
        let dog = json!({"$ref": "#/definitions/Dog"});
        let errors = validate(&root, &dog, &json!({"name": "Rex"}));
        assert_eq!(errors, ["$: required property \"breed\" is missing"]);
    }

    #[test]
    fn validation_names_the_path_and_the_problem() {
        let root = root();
        let pet = json!({"$ref": "#/definitions/Pet"});
        let value =
            json!({"id": 0, "name": null, "age": 30, "status": "lost", "tags": [1], "x": 1});
        let mut errors = validate(&root, &pet, &value);
        errors.sort();
        assert_eq!(
            errors,
            [
                "$.age: 30 is above the maximum 30",
                "$.id: 0 is below the minimum 1",
                "$.name: expected string, got null",
                "$.status: \"lost\" is not one of the enum values",
                "$.tags[0]: expected string, got integer",
                "$.x: property is not in the schema",
            ]
        );
    }
}
