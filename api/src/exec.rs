//! An adapter that hands each call to a helper process.
//!
//! The helper can be written in any language. It reads one JSON object per
//! line on standard input and answers each with one line on standard output:
//!
//! ```text
//! -> {"operation": "http", "input": {"method": "GET", "path": "/pets"}}
//! <- {"output": {"status": 200, "body": "[]"}}
//! ```
//!
//! A helper that cannot complete a call answers `{"error": "why"}`. The
//! helper is free to call the code under test in-process, so an API can be
//! checked without a network, or over a protocol qcheck does not speak.

use crate::adapter::Adapter;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

pub struct ExecAdapter {
    command: String,
    process: Mutex<Process>,
}

struct Process {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ExecAdapter {
    /// Starts `command` through `sh -c`.
    pub fn new(command: &str) -> Result<Self, String> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start {command:?}: {e}"))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        Ok(ExecAdapter {
            command: command.to_string(),
            process: Mutex::new(Process {
                child,
                stdin,
                stdout,
            }),
        })
    }
}

impl Adapter for ExecAdapter {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, String> {
        let mut process = self.process.lock().map_err(|_| "adapter lock poisoned")?;
        let line = json!({ "operation": operation, "input": input }).to_string();
        writeln!(process.stdin, "{line}")
            .and_then(|_| process.stdin.flush())
            .map_err(|e| format!("helper {:?} stopped reading: {e}", self.command))?;

        let mut reply = String::new();
        let read = process
            .stdout
            .read_line(&mut reply)
            .map_err(|e| format!("helper {:?}: {e}", self.command))?;
        if read == 0 {
            return Err(format!("helper {:?} exited without a reply", self.command));
        }

        let mut reply: Value = serde_json::from_str(&reply)
            .map_err(|e| format!("helper reply is not JSON ({e}): {}", reply.trim_end()))?;
        if let Some(error) = reply.get("error") {
            return Err(error
                .as_str()
                .map(str::to_string)
                .unwrap_or(error.to_string()));
        }
        match reply.get_mut("output") {
            Some(output) => Ok(output.take()),
            None => Err("helper reply has neither \"output\" nor \"error\"".into()),
        }
    }

    fn describe(&self) -> String {
        format!("helper `{}`", self.command)
    }
}

impl Drop for ExecAdapter {
    fn drop(&mut self) {
        if let Ok(mut process) = self.process.lock() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
    }
}
