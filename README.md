# qcheck

QuickCheck for APIs. qcheck generates requests, sends them to a running
API, and checks each response. When a check fails, it shrinks the request
to the smallest one that still fails.

It tests from the outside, through an adapter, so the API can be written in
any language. Two adapters are built in: HTTP (versions 1.0, 1.1, 2 and 3)
and a helper process that speaks JSON lines. Given an OpenAPI description
(2.0, 3.0 or 3.1), qcheck derives the requests and the checks itself.

The repository holds two crates. `qcheck` is the property testing library,
with no dependencies. `qcheck-api` in `api/` adds the adapters, the OpenAPI
support, and the `qcheck` command.

## Install

Each release has a static binary for x86_64 Linux. It has no dependencies,
so the same two lines work on a laptop, in a container, and in CI:

```
curl -fsSL -o qcheck https://github.com/happi/qcheck/releases/latest/download/qcheck-x86_64-linux
chmod +x qcheck
```

Pin a version in CI by replacing `latest/download` with `download/v0.1.0`.
From source: `cargo build --release -p qcheck-api --bin qcheck` in a
checkout, which picks up the build flag in `.cargo/config.toml`.

## Checking an API from its description

`api/examples/buggy_pets.rs` is a small pet API with three planted bugs, and
`api/examples/pets.yaml` describes it. Start the server, then point qcheck at
the description:

```
cargo run -p qcheck-api --example buggy_pets &
cargo run -p qcheck-api --bin qcheck -- openapi api/examples/pets.yaml \
    --base-url http://127.0.0.1:8642 --http 2 --seed 1
```

Captured output:

```
api/examples/pets.yaml: 4 operations, 100 requests each, http://127.0.0.1:8642 over HTTP/2, seed 1

ok    GET /health (health): 100 requests
FAIL  GET /pets (listPets): after 1 passing request
      check:    no-server-error
      reason:   server error: status 500
      request:  GET /pets?limit=0
      response: HTTP/2.0 500 {"error":"attempt to divide by zero"}
      original: GET /pets?limit=0&tag=y%7D-%22hM8%2C99  (before 1 shrink)
      replay:   curl -i --http2-prior-knowledge -X GET 'http://127.0.0.1:8642/pets?limit=0' -H 'accept: application/json'
      rerun:    add --seed 1 --operation 'listPets'
FAIL  POST /pets (createPet): after 0 passing requests
      check:    documented-status
      reason:   status 422 is not documented (documented: 201, 400)
      request:  POST /pets
                {"age":0,"name":"aaaaaaaaaaa"}
      response: HTTP/2.0 422 {"error":"name too long"}
      original: POST /pets  (before 14 shrinks)
                {"age":14,"name":"NN:<Uz?J1OVMo5=8<fhS"}
      replay:   curl -i --http2-prior-knowledge -X POST 'http://127.0.0.1:8642/pets' -H 'accept: application/json' -H 'content-type: application/json' --data-raw '{"age":0,"name":"aaaaaaaaaaa"}'
      rerun:    add --seed 1 --operation 'createPet'
FAIL  GET /pets/{id} (getPet): after 3 passing requests
      check:    response-schema
      reason:   $.name: expected string, got null
      request:  GET /pets/1001
      response: HTTP/2.0 200 {"age":3,"id":1001,"name":null}
      original: GET /pets/9007199254740992  (before 51 shrinks)
      replay:   curl -i --http2-prior-knowledge -X GET 'http://127.0.0.1:8642/pets/1001' -H 'accept: application/json'
      rerun:    add --seed 1 --operation 'getPet'

1 passed, 3 failed, 0 skipped
```

Look at `createPet`. The first failing body had a twenty-character name and
an age of 14. The shrunk body has eleven `a`s and an age of 0. The
description allows twenty characters and the server rejects the eleventh, so
the report hands you the boundary. In `getPet` the id went from
9007199254740992 to 1001, the first id whose pet has no name.

The description can also be a URL. Many frameworks serve one, FastAPI at
`/openapi.json`, and then the API is assumed to run where the description
came from:

```
qcheck openapi http://127.0.0.1:8000/openapi.json
```

The command exits with 1 when a check fails, so it fits a CI step.
`qcheck openapi --help` lists the options. `--dry-run` prints one generated
request per operation and sends nothing.

Generated requests are real requests. A `POST` creates things. Run qcheck
against a test instance.

## What is generated and what is checked

Each operation becomes one property: for every valid request, the response
passes the checks. A request is built from the operation's parameters (path,
query, header, form and JSON body). Required parameters are always present.
Optional ones come and go, and shrink to absent.

Values follow the schema: `type`, `format` (int32, int64, date, date-time,
uuid, email, byte), `enum`, `minimum` and `maximum` with their exclusive
forms, `multipleOf` for integers, `minLength`, `maxLength`, `minItems`,
`maxItems`, `uniqueItems`, `required`, `allOf`, and `$ref` into the same
file, recursive references included. From OpenAPI 3.x: `requestBody` (JSON
or URL-encoded), `oneOf`, `anyOf`, `const`, `nullable`, type lists such as
`["string", "null"]`, numeric exclusive bounds, and cookie parameters.

| Check | A response fails when |
|---|---|
| `transport` | no response came back |
| `no-server-error` | the status is 500 or above |
| `documented-status` | the status is not listed and there is no `default` response |
| `content-type` | the body has a media type the description does not list for the status |
| `response-schema` | the JSON body does not conform to the schema documented for the status |

Shrinking keeps to the check that failed first. `--skip-check` turns one
off. A value passes `oneOf` when at least one alternative accepts it; a
second match is not reported, since overlapping alternatives are common.

Authentication is yours to supply: pass the header with `-H`, for example
`-H 'Authorization: Bearer ...'`. Security schemes in the description are
not read.

## HTTP versions

`--http` selects the protocol version: `1.0`, `1.1`, `2`, `3`, or `auto`.

| Value | On the wire |
|---|---|
| `auto` | HTTP/2 when a TLS server offers it, otherwise HTTP/1.1 |
| `1.0`, `1.1` | that version |
| `2` | HTTP/2 without negotiation: h2c on `http`, `h2` only on `https` |
| `3` | HTTP/3 over QUIC; needs an `https` URL |

The tests confirm 1.0, 1.1 and 2 against a server that echoes the version it
saw. HTTP/3 has no local test server yet; it was verified by hand against a
public HTTP/3 endpoint. HTTP/3 support in the client library is still marked
unstable, which is why `.cargo/config.toml` sets a build flag.

## Other protocols and languages: adapters

An adapter carries a call to the system under test and returns the reply.
It is one trait:

```rust
pub trait Adapter {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, String>;
    fn describe(&self) -> String;
}
```

`--exec` runs a helper process as the adapter. The helper reads one JSON
object per line on standard input and writes one reply per line:

```
-> {"operation": "http", "input": {"method": "GET", "path": "/pets", "query": [], "headers": [], "body": null}}
<- {"output": {"status": 200, "headers": [["content-type", "application/json"]], "body": "[]"}}
```

A helper that fails to complete a call answers `{"error": "why"}`. The
helper decides how a request reaches the code. It can call a handler
in-process with no network, or translate to a protocol qcheck does not
speak. `api/examples/pets_helper.py` is a complete one in Python:

```
qcheck openapi api/examples/pets.yaml --exec 'python3 api/examples/pets_helper.py'
```

For a protocol with no HTTP shape, implement `Adapter` in Rust, or use
`ExecAdapter` with your own operation names, and write the property with
`forall`:

```rust
let adapter = ExecAdapter::new("./my-helper")?;
forall(gens::range(0i64, 1000), |n| {
    let reply = adapter.call("double", &json!(n)).unwrap();
    reply == json!(n * 2)
});
```

## Not covered yet

- `$ref` to other files, and OpenAPI 3 `servers` with variables.
- `pattern`: generated strings ignore it, and the report says so per
  operation.
- File uploads, multipart forms, and request bodies other than JSON and
  URL-encoded forms. Such operations are reported as skipped.
- Invalid requests that should be rejected with a 4xx.
- Sequences of calls checked against a model (state machine testing), and
  test case statistics. QuviQ QuickCheck and PropEr have both.
- In the library: floats, `Result`, collections beyond `Vec`, and a test
  attribute, which the Rust `quickcheck` crate has.

## The property testing library

The `qcheck` crate is the engine underneath, and works on its own for
testing Rust code.

### A property

A property is a function from one generated value to `bool`. Use a tuple
when it needs several inputs.

```rust
use qcheck::quickcheck;

#[test]
fn reverse_twice_is_identity() {
    quickcheck(|v: Vec<i32>| {
        let mut w = v.clone();
        w.reverse();
        w.reverse();
        w == v
    });
}
```

A property may also return `()` and use `assert!`, return a `Result`, or
return `TestResult` to discard inputs that miss a precondition.

### A failure

`examples/dedup.rs` tests a `dedup` that only compares neighbours. This is
the captured output of `QC_SEED=1 cargo run --example dedup`:

```
property failed after 8 passing tests
  counterexample: [2, 0, 2]
  before 3 shrinks: [2, 7, 2, 228, 2, 83, 3]
  reason: property returned false
  rerun with QC_SEED=1
```

The seven-element list is what the generator produced. The three-element
list is what shrinking left: two equal values with something between them,
which is the bug.

Shrinking is greedy. It moves to the first simpler input that still fails
and stops when no single step does. `[1, 0, 1]` is simpler than `[2, 0, 2]`,
but reaching it means changing two elements at once, so qcheck stops here.

### Generators

`quickcheck` picks its inputs from the argument's type. When the type says
too little, build a generator and hand it to `forall`. This follows
`?FORALL(X, Generator, Prop)` in QuviQ QuickCheck and PropEr.

`examples/orders.rs` prices an order with a discount from ten items up. An
order is a unit price between 1 and 500 kr, in öre, and a quantity:

```rust
let order = gens::zip(gens::range(100u64, 50_000), gens::range(1u64, 100));

forall(order, |(price, quantity)| {
    total(price, quantity) >= total(price, quantity - 1)
});
```

Captured output of `QC_SEED=1 cargo run --example orders`:

```
property failed after 47 passing tests
  counterexample: (100, 10)
  before 1 shrinks: (29887, 10)
  reason: property returned false
  rerun with QC_SEED=1
```

The price shrank to 100, the lower bound of its range, and went no further.
A generator carries its own shrinking, so a counterexample is always a value
the generator could have produced.

| qcheck | QuviQ / PropEr | Shrinks toward |
|---|---|---|
| `gens::any::<T>()` | a type as generator | the type's `Arbitrary::shrink` |
| `gens::constant(v)` | `return`, `exactly` | nothing |
| `gens::range(lo, hi)` | `choose`, `range` | zero, or the bound nearest zero |
| `gens::elements(vec)` | `elements` | the front of the list |
| `gens::one_of(vec)` | `oneof`, `union` | the first generator |
| `gens::frequency(vec)` | `frequency` | the first generator with a weight |
| `gens::vec(g)`, `gens::vec_len(g, min, max)` | `list`, `vector` | fewer, then simpler elements |
| `gens::option(g)` | `default` | `None` |
| `gens::zip(a, b)`, `gens::zip3` | a tuple of generators | each component in turn |
| `gens::sized(f)` | `?SIZED` | |
| `gens::lazy(f)` | `?LAZY` | |
| `g.map(f)` | `?LET` | the input of `f` |
| `g.and_then(f)` | `?LET` returning a generator | the first value, then the second |
| `g.such_that(p)` | `?SUCHTHAT` | values that satisfy `p` |
| `g.no_shrink()` | `noshrink` | nothing |
| `g.resize(n)` | `resize` | |

`such_that` panics after 50 rejected draws in a row, PropEr's default for
`constraint_tries`. When the constraint is that strict, construct valid
values with `map` instead.

A recursive generator calls itself through `gens::lazy` and spends a budget
from `gens::sized`, so it ends. `tests/generators.rs` has one for nested
lists.

### Replaying and tuning

Each run picks a fresh seed and prints it on failure. `QC_SEED` replays a
run and `QC_TESTS` changes the test count (default 100). The same settings
are available in code:

```rust
let outcome = QuickCheck::new().seed(42).tests(1000).run(|x: u32| x < 500);
```

`run` returns an `Outcome` to inspect. `check` panics with the report, which
is the form a `#[test]` wants. `run_with` and `check_with` take a generator.

### Your own types

Implement `Arbitrary`. `arbitrary` builds a value from the `Gen`, and
`shrink` lists strictly simpler candidates, simplest first. `shrink` is
optional; without it failures are reported as generated.

```rust
impl Arbitrary for Even {
    fn arbitrary(g: &mut Gen) -> Self {
        Even(g.below(g.size() as u64 + 1) as u32 * 2)
    }

    fn shrink(&self) -> Shrinker<Self> {
        Box::new((self.0 / 2).shrink().map(|half| Even(half * 2)))
    }
}
```

Built in: `bool`, the integer types up to 64 bits, `char`, `String`,
`Vec<T>`, `Option<T>`, and tuples of two or three.

## Running the tests

```
cargo test --workspace
scripts/dtest.sh --workspace    # same, in a Rust container, for hosts without cargo
```

The API tests start the example server on a free port. The helper test
needs `python3` and is skipped without it.

`api/tests/dogfood.rs` is qcheck testing itself: percent-encoding round
trips, a request arrives at the server as written on HTTP/1.0, 1.1 and 2,
and no description, however malformed, makes the reader panic.

## License

Licensed under either the MIT license (`LICENSE-MIT`) or the Apache License,
Version 2.0 (`LICENSE-APACHE`), at your option.
