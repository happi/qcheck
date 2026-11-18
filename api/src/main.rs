//! The `qcheck` command.

use qcheck::{Gen, QuickCheck};
use qcheck_api::openapi::{check_operation, Check, Failure, Operation, Options, Spec, Verdict};
use qcheck_api::{Adapter, ExecAdapter, HttpAdapter, HttpOptions, HttpRequest, HttpVersion};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "\
qcheck: QuickCheck for APIs

Usage:
  qcheck openapi <spec> [options]

Reads an OpenAPI 2.0 description (JSON or YAML), generates valid requests
for each operation, and checks every response against the description.
A failing request is shrunk to the smallest one that still fails.

Target:
  --base-url <url>      Where the API runs. Default: host and basePath from
                        the description.
  --http <version>      auto, 1.0, 1.1, 2 or 3. Default: auto. `2` skips
                        negotiation (h2c on plain http). `3` needs https.
  --exec <command>      Send requests to a helper process instead of over
                        HTTP. See the README for the JSON lines protocol.
  -H, --header <h>      Header for every request, as 'Name: value'.
  --insecure            Accept any TLS certificate.
  --timeout <seconds>   Per request. Default: 30.

Run:
  --tests <n>           Requests per operation. Default: 100.
  --seed <n>            Replay a run.
  --max-shrinks <n>     Requests spent shrinking one failure. Default: 500.
  --operation <text>    Only operations with this operationId, or with this
                        text in 'METHOD /path'.
  --skip-check <name>   Disable a check: transport, no-server-error,
                        documented-status, content-type, response-schema.
  --dry-run             Print one generated request per operation and send
                        nothing.

Exit status: 0 when every operation passes, 1 on a failed check, 2 on a
usage or setup error.
";

struct Args {
    spec: String,
    base_url: Option<String>,
    exec: Option<String>,
    http: HttpOptions,
    options: Options,
    operation: Option<String>,
    dry_run: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args {
        spec: String::new(),
        base_url: None,
        exec: None,
        http: HttpOptions::default(),
        options: Options::default(),
        operation: None,
        dry_run: false,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = |what: &str| rest.next().cloned().ok_or(format!("{arg} needs {what}"));
        let number = |text: String| {
            text.parse::<u64>()
                .map_err(|_| format!("{arg}: {text:?} is not a number"))
        };
        match arg.as_str() {
            "--base-url" => parsed.base_url = Some(value("a URL")?),
            "--exec" => parsed.exec = Some(value("a command")?),
            "--http" => {
                let text = value("a version")?;
                parsed.http.version = HttpVersion::parse(&text).ok_or(format!(
                    "--http: {text:?} is not one of auto, 1.0, 1.1, 2, 3"
                ))?;
            }
            "-H" | "--header" => {
                let text = value("a header")?;
                let (name, content) = text
                    .split_once(':')
                    .ok_or(format!("{arg}: {text:?} is not 'Name: value'"))?;
                parsed
                    .http
                    .headers
                    .push((name.trim().to_string(), content.trim().to_string()));
            }
            "--insecure" => parsed.http.insecure = true,
            "--timeout" => parsed.http.timeout = Duration::from_secs(number(value("seconds")?)?),
            "--tests" => parsed.options.tests = number(value("a count")?)? as u32,
            "--seed" => parsed.options.seed = Some(number(value("a seed")?)?),
            "--max-shrinks" => parsed.options.max_shrinks = number(value("a count")?)? as u32,
            "--operation" => parsed.operation = Some(value("a filter")?),
            "--skip-check" => {
                let name = value("a check name")?;
                let check =
                    Check::parse(&name).ok_or(format!("--skip-check: no check named {name:?}"))?;
                parsed.options.checks.retain(|c| *c != check);
            }
            "--dry-run" => parsed.dry_run = true,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            path if parsed.spec.is_empty() => parsed.spec = path.to_string(),
            extra => return Err(format!("unexpected argument {extra:?}")),
        }
    }
    if parsed.spec.is_empty() {
        return Err("no description file given".into());
    }
    Ok(parsed)
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The request as a curl command, so a failure can be replayed by hand.
fn curl(request: &HttpRequest, base_url: &str, version: HttpVersion) -> String {
    let mut parts = vec!["curl -i".to_string()];
    parts.extend(version.curl_flag().map(str::to_string));
    parts.push(format!("-X {}", request.method));
    parts.push(quote(&format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        request.target()
    )));
    for (name, value) in &request.headers {
        parts.push(format!("-H {}", quote(&format!("{name}: {value}"))));
    }
    if let Some(body) = request.body_text() {
        parts.push(format!("--data-raw {}", quote(&body)));
    }
    parts.join(" ")
}

fn clip(text: &str, limit: usize) -> String {
    let flat = text.replace('\n', " ");
    match flat.char_indices().nth(limit) {
        Some((end, _)) => format!("{}...", &flat[..end]),
        None => flat,
    }
}

fn print_request(label: &str, request: &HttpRequest, note: &str) {
    println!(
        "      {label:<9} {} {}{note}",
        request.method,
        request.target()
    );
    if let Some(body) = request.body_text() {
        println!("      {:<9} {}", "", clip(&body, 300));
    }
}

fn plural(count: u32, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

fn print_failure(
    failure: &Failure,
    operation: &Operation,
    args: &Args,
    base_url: Option<&str>,
    seed: u64,
) {
    println!("      check:    {}", failure.violation.check.name());
    println!("      reason:   {}", clip(&failure.violation.message, 400));
    print_request("request:", &failure.request, "");
    if let Some(response) = &failure.response {
        let line = format!(
            "{} {} {}",
            response.version,
            response.status,
            clip(&response.body, 200)
        );
        println!("      response: {}", line.trim_start());
    }
    if failure.shrinks > 0 {
        let note = format!("  (before {})", plural(failure.shrinks, "shrink"));
        print_request("original:", &failure.original, &note);
    }
    if let Some(base_url) = base_url {
        println!(
            "      replay:   {}",
            curl(&failure.request, base_url, args.http.version)
        );
    }
    let filter = operation
        .operation_id
        .clone()
        .unwrap_or(format!("{} {}", operation.method, operation.path));
    println!(
        "      rerun:    add --seed {seed} --operation {}",
        quote(&filter)
    );
}

fn run(args: &Args) -> Result<bool, String> {
    let spec = Spec::load(&args.spec)?;
    let operations: Vec<Operation> = spec
        .operations()
        .into_iter()
        .filter(|o| args.operation.as_deref().is_none_or(|f| o.matches(f)))
        .collect();
    if operations.is_empty() {
        return Err("no operation to check".into());
    }
    let seed = args
        .options
        .seed
        .unwrap_or_else(|| QuickCheck::new().current_seed());
    let options = Options {
        seed: Some(seed),
        ..args.options.clone()
    };

    if args.dry_run {
        let mut g = Gen::new(seed, 30);
        for operation in &operations {
            println!("{operation}");
            match &operation.unsupported {
                Some(reason) => println!("      skipped: {reason}"),
                None => print_request("sample:", &operation.requests().sample(&mut g), ""),
            }
        }
        return Ok(true);
    }

    let base_url = match &args.exec {
        Some(_) => None,
        None => Some(
            args.base_url
                .clone()
                .or_else(|| spec.base_url())
                .ok_or("the description names no host; pass --base-url")?,
        ),
    };
    let adapter: Box<dyn Adapter> = match (&args.exec, &base_url) {
        (Some(command), _) => Box::new(ExecAdapter::new(command)?),
        (None, Some(url)) => Box::new(HttpAdapter::new(url, args.http.clone())?),
        (None, None) => unreachable!("a base URL is required without --exec"),
    };

    println!(
        "{}: {}, {} each, {}, seed {seed}\n",
        args.spec,
        plural(operations.len() as u32, "operation"),
        plural(options.tests, "request"),
        adapter.describe()
    );

    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    for operation in &operations {
        match check_operation(operation, adapter.as_ref(), &options) {
            Verdict::Passed { tests } => {
                passed += 1;
                println!("ok    {operation}: {}", plural(tests, "request"));
            }
            Verdict::Skipped(reason) => {
                skipped += 1;
                println!("skip  {operation}: {reason}");
            }
            Verdict::Failed(failure) => {
                failed += 1;
                println!(
                    "FAIL  {operation}: after {}",
                    plural(failure.passed, "passing request")
                );
                print_failure(&failure, operation, args, base_url.as_deref(), seed);
            }
        }
        for warning in &operation.warnings {
            println!("      note: {warning}");
        }
    }
    println!("\n{passed} passed, {failed} failed, {skipped} skipped");
    Ok(failed == 0)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("openapi") => {}
        Some("-h" | "--help" | "help") | None => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("qcheck: unknown command {other:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match parse_args(&args[1..]).and_then(|args| run(&args)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("qcheck: {message}");
            ExitCode::from(2)
        }
    }
}
