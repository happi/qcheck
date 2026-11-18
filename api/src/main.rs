//! The `qcheck` command.

use qcheck::{Gen, QuickCheck};
use qcheck_api::openapi::{check_operation, Check, Failure, Operation, Options, Spec, Verdict};
use qcheck_api::sequence::{check_sequences, SequenceVerdict};
use qcheck_api::{Adapter, ExecAdapter, HttpAdapter, HttpOptions, HttpRequest, HttpVersion};
use std::process::ExitCode;
use std::time::Duration;

/// Prints a line, and exits quietly when the reader has gone away, as it
/// has when the output is piped to `head`.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        if writeln!(std::io::stdout(), $($arg)*).is_err() {
            std::process::exit(0);
        }
    }};
}

const USAGE: &str = "\
qcheck: QuickCheck for APIs

Usage:
  qcheck openapi <spec> [options]
  qcheck --version

<spec> is a file or an http(s) URL, such as a service's /openapi.json.

Reads an OpenAPI description (2.0, 3.0 or 3.1; JSON or YAML), generates valid requests
for each operation, and checks every response against the description.
A failing request is shrunk to the smallest one that still fails.

Target:
  --base-url <url>      Where the API runs. Default: the server named in the
                        description, else the host the description was
                        fetched from.
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
  --sequences <n>       Sequences of calls to run after the single requests,
                        where later calls use ids from earlier responses.
                        Default: 50. 0 turns them off.
  --sequence-length <n> The most calls in one sequence. Default: 8.
  --reset <command>     Shell command that restores the server's state. It
                        runs before each sequence.
  --set <name=value>    A value known to exist on the server, for the
                        parameter or body property of that name. Half the
                        requests use it. May be repeated.
  --operation <text>    Only operations with this operationId, or with this
                        text in 'METHOD /path'. With a '*' in it, a pattern
                        for the operationId, as in '*Unsupported*'. May be
                        repeated.
  --exclude <text>      Leave out operations that match, same matching.
                        May be repeated.
  --exclude-from <file> Exclusions from a file, one per line, as
                        'text  # reason'. The reason is printed with the
                        skipped operation, which keeps known failures
                        visible while the rest of the API is checked.
                        'text @check,check  # reason' leaves out only the
                        named checks for the matching operations.
  --skip-check <name>   Disable a check: transport, no-server-error,
                        documented-status, content-type, response-schema,
                        and for sequences deleted-stays-deleted,
                        created-is-readable.
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
    operations: Vec<String>,
    /// A filter and the reason for it.
    excluded: Vec<(String, String)>,
    /// A filter, the checks to leave out for the operations it matches,
    /// and the reason.
    relaxed: Vec<(String, Vec<Check>, String)>,
    dry_run: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args {
        spec: String::new(),
        base_url: None,
        exec: None,
        http: HttpOptions::default(),
        options: Options::default(),
        operations: Vec::new(),
        excluded: Vec::new(),
        relaxed: Vec::new(),
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
            "--sequences" => parsed.options.sequences = number(value("a count")?)? as u32,
            "--sequence-length" => {
                parsed.options.sequence_length = number(value("a count")?)? as usize
            }
            "--reset" => parsed.options.reset = Some(value("a command")?),
            "--set" => {
                let text = value("name=value")?;
                let (name, fixed) = text
                    .split_once('=')
                    .ok_or(format!("--set: {text:?} is not name=value"))?;
                // A value that reads as JSON is sent as that type.
                let fixed = serde_json::from_str(fixed)
                    .unwrap_or(serde_json::Value::String(fixed.to_string()));
                parsed.options.fixtures.push((name.to_string(), fixed));
            }
            "--operation" => parsed.operations.push(value("a filter")?),
            "--exclude" => parsed
                .excluded
                .push((value("a filter")?, "excluded".to_string())),
            "--exclude-from" => {
                let path = value("a file")?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                for line in text.lines() {
                    let (entry, reason) = line.split_once('#').unwrap_or((line, "excluded"));
                    let (entry, reason) = (entry.trim(), reason.trim().to_string());
                    if entry.is_empty() {
                        continue;
                    }
                    // `filter @check,check` leaves out only those checks.
                    match entry.split_once(" @") {
                        None => parsed.excluded.push((entry.to_string(), reason)),
                        Some((filter, names)) => {
                            let mut checks = Vec::new();
                            for name in names.split(',') {
                                let name = name.trim();
                                checks.push(
                                    Check::parse(name)
                                        .ok_or(format!("{path}: no check named {name:?}"))?,
                                );
                            }
                            parsed
                                .relaxed
                                .push((filter.trim().to_string(), checks, reason));
                        }
                    }
                }
            }
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
fn curl(
    request: &HttpRequest,
    base_url: &str,
    version: HttpVersion,
    extra_headers: &[(String, String)],
) -> String {
    let mut parts = vec!["curl -i".to_string()];
    parts.extend(version.curl_flag().map(str::to_string));
    parts.push(format!("-X {}", request.method));
    parts.push(quote(&format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        request.target()
    )));
    // Headers from the command line are often credentials, and reports end
    // up in CI logs, so only their names are shown.
    for (name, _) in extra_headers {
        parts.push(format!("-H {}", quote(&format!("{name}: ..."))));
    }
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
    say!(
        "      {label:<9} {} {}{note}",
        request.method,
        request.target()
    );
    if let Some(body) = request.body_text() {
        say!("      {:<9} {}", "", clip(&body, 300));
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
    say!("      check:    {}", failure.violation.check.name());
    say!("      reason:   {}", clip(&failure.violation.message, 400));
    print_request("request:", &failure.request, "");
    if let Some(response) = &failure.response {
        let line = format!(
            "{} {} {}",
            response.version,
            response.status,
            clip(&response.body, 200)
        );
        say!("      response: {}", line.trim_start());
    }
    if failure.shrinks > 0 {
        let note = format!("  (before {})", plural(failure.shrinks, "shrink"));
        print_request("original:", &failure.original, &note);
    }
    if let Some(base_url) = base_url {
        say!(
            "      replay:   {}",
            curl(
                &failure.request,
                base_url,
                args.http.version,
                &args.http.headers
            )
        );
    }
    let filter = operation
        .operation_id
        .clone()
        .unwrap_or(format!("{} {}", operation.method, operation.path));
    say!(
        "      rerun:    add --seed {seed} --operation {}",
        quote(&filter)
    );
}

fn run(args: &Args) -> Result<bool, String> {
    let spec = Spec::load(&args.spec)?;
    let operations: Vec<Operation> = spec
        .operations()
        .into_iter()
        .filter(|o| args.operations.is_empty() || args.operations.iter().any(|f| o.matches(f)))
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
            say!("{operation}");
            let excluded = args.excluded.iter().find(|(f, _)| operation.matches(f));
            match (&operation.unsupported, excluded) {
                (Some(reason), _) | (None, Some((_, reason))) => say!("      skipped: {reason}"),
                (None, None) => {
                    let requests = operation.requests_with(&args.options.fixtures);
                    print_request("sample:", &requests.sample(&mut g), "")
                }
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

    say!(
        "{}: {}, {} each, {}, seed {seed}\n",
        args.spec,
        plural(operations.len() as u32, "operation"),
        plural(options.tests, "request"),
        adapter.describe()
    );

    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    // Every operation that ran goes on to the sequences, with the checks
    // that apply to it. One that failed alone has been reported, so it
    // takes part without the check it failed: a create that returns an
    // undocumented status still creates what later calls need.
    let mut sound: Vec<(Operation, Vec<Check>)> = Vec::new();
    for operation in &operations {
        if let Some((_, reason)) = args.excluded.iter().find(|(f, _)| operation.matches(f)) {
            skipped += 1;
            say!("skip  {operation}: {reason}");
            continue;
        }
        let mut options = options.clone();
        let mut relaxations = Vec::new();
        for (filter, checks, reason) in &args.relaxed {
            if operation.matches(filter) {
                options.checks.retain(|check| !checks.contains(check));
                let names: Vec<&str> = checks.iter().map(|check| check.name()).collect();
                relaxations.push(format!("{} not checked: {reason}", names.join(", ")));
            }
        }
        let verdict = check_operation(operation, adapter.as_ref(), &options);
        match verdict {
            Verdict::Passed { tests, statuses } => {
                passed += 1;
                sound.push((operation.clone(), options.checks.clone()));
                let seen: Vec<String> = statuses
                    .iter()
                    .map(|(status, count)| format!("{status} x{count}"))
                    .collect();
                say!(
                    "ok    {operation}: {} ({})",
                    plural(tests, "request"),
                    seen.join(", ")
                );
                // Passing on rejections alone checks the error path only.
                if !statuses
                    .iter()
                    .any(|(status, _)| (200..300).contains(status))
                {
                    say!("      note: no request got a 2xx response, so the success path was not checked");
                }
            }
            Verdict::Skipped(reason) => {
                skipped += 1;
                say!("skip  {operation}: {reason}");
            }
            Verdict::Failed(failure) => {
                failed += 1;
                let mut checks = options.checks.clone();
                checks.retain(|check| *check != failure.violation.check);
                sound.push((operation.clone(), checks));
                say!(
                    "FAIL  {operation}: after {}",
                    plural(failure.passed, "passing request")
                );
                print_failure(&failure, operation, args, base_url.as_deref(), seed);
            }
        }
        for note in &relaxations {
            say!("      note: {note}");
        }
        for warning in &operation.warnings {
            say!("      note: {warning}");
        }
    }
    match check_sequences(&sound, adapter.as_ref(), &options) {
        SequenceVerdict::Skipped(_) => {}
        SequenceVerdict::Passed {
            sequences,
            calls,
            reused,
            statuses,
        } => {
            passed += 1;
            let seen: Vec<String> = statuses
                .iter()
                .map(|(status, count)| format!("{status} x{count}"))
                .collect();
            say!(
                "ok    sequences: {}, {} ({})",
                plural(sequences, "sequence"),
                plural(calls, "call"),
                seen.join(", ")
            );
            say!(
                "      {} used a value from an earlier response",
                plural(reused, "call")
            );
        }
        SequenceVerdict::Failed(failure) => {
            failed += 1;
            say!(
                "FAIL  sequences: after {}",
                plural(failure.passed, "passing sequence")
            );
            say!("      check:    {}", failure.violation.check.name());
            say!("      reason:   {}", clip(&failure.violation.message, 400));
            for (index, call) in failure.calls.iter().enumerate() {
                let label = if index == 0 { "calls:" } else { "" };
                let status = match call.status {
                    Some(status) => status.to_string(),
                    None => "no response".to_string(),
                };
                say!(
                    "      {label:<9} {}. {} {}  -> {status}",
                    index + 1,
                    call.request.method,
                    call.request.target()
                );
                if let Some(body) = call.request.body_text() {
                    say!("      {:<9}    {}", "", clip(&body, 300));
                }
            }
            say!("      rerun:    add --seed {seed}");
        }
    }
    say!("\n{passed} passed, {failed} failed, {skipped} skipped");
    Ok(failed == 0)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("openapi") => {}
        Some("-V" | "--version" | "version") => {
            say!("qcheck {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("-h" | "--help" | "help") | None => {
            say!("{}", USAGE.trim_end());
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("qcheck: unknown command {other:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        say!("{}", USAGE.trim_end());
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
