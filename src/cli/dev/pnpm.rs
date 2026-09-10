use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::Value;

use super::{SourceWatchError, SourceWatchResult};

const RECORD_PREFIX: &[u8] = b"{\"";
const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;
const MAX_ACTIVE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ACTIVE_LIFECYCLES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OutputChannel {
    Stdout,
    Stderr,
}

#[derive(Clone, Default)]
pub(super) struct InstallObserver {
    state: Arc<Mutex<InstallState>>,
}

#[derive(Default)]
struct InstallState {
    active: BTreeMap<LifecycleKey, bool>,
    active_bytes: usize,
    problem: Option<&'static str>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct LifecycleKey {
    pid: u64,
    dep_path: String,
    stage: String,
    wd: String,
}

impl LifecycleKey {
    fn from_record(record: &Value) -> Option<Self> {
        let pid = record.get("pid")?.as_u64().filter(|pid| *pid > 0)?;
        Some(Self {
            pid,
            dep_path: record.get("depPath")?.as_str()?.to_string(),
            stage: record.get("stage")?.as_str()?.to_string(),
            wd: record.get("wd")?.as_str()?.to_string(),
        })
    }

    fn bytes(&self) -> usize {
        self.dep_path.len() + self.stage.len() + self.wd.len()
    }
}

impl InstallState {
    fn fail(&mut self, problem: &'static str) {
        self.problem.get_or_insert(problem);
        self.active.clear();
        self.active_bytes = 0;
    }
}

pub(super) fn reserved_signal_exit(code: i32) -> bool {
    cfg!(unix) && (129..=192).contains(&code)
}

impl InstallObserver {
    fn fail(&self, problem: &'static str) {
        if let Ok(mut state) = self.state.lock() {
            state.fail(problem);
        }
    }

    fn observe(&self, record: &Value) {
        let Some(name) = record.get("name").and_then(Value::as_str) else {
            self.fail("pnpm emitted an invalid reporter envelope");
            return;
        };
        if name != "pnpm:lifecycle" {
            if name.starts_with("pnpm:lifecycle") {
                self.fail("pnpm emitted an unsupported lifecycle record");
            }
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.problem.is_some() {
            return;
        }
        let Some(key) = LifecycleKey::from_record(record) else {
            state.fail("pnpm emitted an invalid lifecycle identity");
            return;
        };
        let variants = ["script", "line", "exitCode"]
            .into_iter()
            .filter(|field| record.get(*field).is_some())
            .count();
        if variants != 1 {
            state.fail("pnpm emitted an ambiguous lifecycle record");
            return;
        }
        if let Some(script) = record.get("script") {
            let Some(optional) = record.get("optional").and_then(Value::as_bool) else {
                state.fail("pnpm omitted lifecycle start metadata");
                return;
            };
            if !script.is_string() || state.active.contains_key(&key) {
                state.fail("pnpm emitted an invalid or duplicate lifecycle start");
            } else if state.active.len() >= MAX_ACTIVE_LIFECYCLES
                || key.bytes() > MAX_ACTIVE_BYTES.saturating_sub(state.active_bytes)
            {
                state.fail("pnpm active lifecycle observation exceeded its memory bound");
            } else {
                state.active_bytes += key.bytes();
                state.active.insert(key, optional);
            }
        } else if let Some(line) = record.get("line") {
            if !line.is_string()
                || !matches!(
                    record.get("stdio").and_then(Value::as_str),
                    Some("stdout" | "stderr")
                )
                || !state.active.contains_key(&key)
            {
                state.fail("pnpm emitted lifecycle output without a matching start");
            }
        } else {
            let code = record
                .get("exitCode")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok());
            let optional = record.get("optional").and_then(Value::as_bool);
            let active_optional = state.active.remove(&key);
            if code.is_none() || optional.is_none() || active_optional != optional {
                state.fail("pnpm emitted an invalid or unmatched lifecycle exit");
                return;
            }
            state.active_bytes -= key.bytes();
            let code = code.unwrap();
            if code < 0 || reserved_signal_exit(code) {
                state.fail("pnpm reported a lifecycle terminated by a signal");
            }
        }
    }

    pub(super) fn finish(&self) -> SourceWatchResult<()> {
        match self.problem() {
            Err(problem) | Ok(Some(problem)) => Err(SourceWatchError::unverified(format!(
                "{problem}; dependency cleanup is unverified and session ownership was retained"
            ))),
            Ok(None) => Ok(()),
        }
    }

    fn problem(&self) -> Result<Option<&'static str>, &'static str> {
        let state = self
            .state
            .lock()
            .map_err(|_| "pnpm lifecycle observation failed")?;
        Ok(state.problem.or_else(|| {
            (!state.active.is_empty()).then_some("pnpm ended with an incomplete lifecycle")
        }))
    }

    // The caller must already have verified the native Windows Job and both
    // output streams. Killing that Job deliberately prevents future exit logs.
    #[cfg(any(windows, test))]
    pub(super) fn finish_after_job_cleanup(&self, cancelled: bool) -> SourceWatchResult<()> {
        if cancelled {
            return Ok(());
        }
        match self.problem() {
            Err(problem) | Ok(Some(problem)) => Err(SourceWatchError::from(format!(
                "{problem}; the dependency process job was stopped and its cleanup verified"
            ))),
            Ok(None) => Ok(()),
        }
    }

    pub(super) fn spawn_reader<R: Read + Send + 'static>(
        &self,
        mut reader: R,
        channel: OutputChannel,
    ) -> JoinHandle<SourceWatchResult<Vec<u8>>> {
        let observer = self.clone();
        thread::spawn(move || {
            let mut decoder = InstallStream::new(observer, channel, write_human);
            let mut buffer = [0_u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => decoder.consume(&buffer[..read]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        return Err(SourceWatchError::unverified(format!(
                            "failed reading dependency install output before EOF: {error}"
                        )));
                    }
                }
            }
            decoder.finish();
            if let Some(error) = decoder.output_error {
                Err(SourceWatchError::from(error))
            } else {
                // Output is rendered incrementally. A long successful install
                // does not accumulate its entire debug stream in memory.
                Ok(Vec::new())
            }
        })
    }
}

fn write_human(channel: OutputChannel, bytes: &[u8]) -> io::Result<()> {
    match channel {
        OutputChannel::Stdout => write_and_flush(&mut io::stdout().lock(), bytes),
        OutputChannel::Stderr => write_and_flush(&mut io::stderr().lock(), bytes),
    }
}

fn write_and_flush(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    output.write_all(bytes)?;
    output.flush()
}

struct InstallStream<W> {
    observer: InstallObserver,
    channel: OutputChannel,
    write: W,
    prefix: usize,
    line_start: bool,
    in_record: bool,
    depth: usize,
    quoted: bool,
    escaped: bool,
    framed: bool,
    discard: bool,
    record: Vec<u8>,
    human: Vec<u8>,
    output_error: Option<String>,
}

impl<W: FnMut(OutputChannel, &[u8]) -> io::Result<()>> InstallStream<W> {
    fn new(observer: InstallObserver, channel: OutputChannel, write: W) -> Self {
        Self {
            observer,
            channel,
            write,
            prefix: 0,
            line_start: true,
            in_record: false,
            depth: 0,
            quoted: false,
            escaped: false,
            framed: false,
            discard: false,
            record: Vec::new(),
            human: Vec::new(),
            output_error: None,
        }
    }

    fn emit(&mut self, channel: OutputChannel, bytes: &[u8]) {
        if self.output_error.is_none()
            && let Err(error) = (self.write)(channel, bytes)
        {
            self.output_error = Some(format!(
                "failed forwarding dependency install output: {error}"
            ));
        }
    }

    fn flush_human(&mut self) {
        if self.human.is_empty() {
            return;
        }
        let mut bytes = std::mem::take(&mut self.human);
        self.emit(self.channel, &bytes);
        bytes.clear();
        self.human = bytes;
    }

    fn begin_record(&mut self, prefix: &[u8]) {
        self.in_record = true;
        self.depth = 0;
        self.quoted = false;
        self.escaped = false;
        self.framed = false;
        for &byte in prefix {
            self.push_record_byte(byte);
        }
    }

    fn push_record_byte(&mut self, byte: u8) {
        self.record.push(byte);
        if self.framed {
            return;
        }
        if self.quoted {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.quoted = false;
            }
            return;
        }
        match byte {
            b'"' => self.quoted = true,
            b'{' | b'[' => self.depth += 1,
            b'}' | b']' => {
                self.depth = self.depth.saturating_sub(1);
                if self.depth != 0 {
                    return;
                }
                self.framed = true;
                // Only find a structural boundary here; Serde still validates
                // JSON. Genuine reports wait for their NDJSON delimiter.
                let reporter = match serde_json::from_slice::<Value>(&self.record) {
                    Ok(value) => is_reporter_record(&value),
                    Err(_) => looks_like_envelope(&self.record),
                };
                if !reporter {
                    self.emit_plain_record(false);
                    self.record.clear();
                    self.in_record = false;
                    self.line_start = false;
                }
            }
            _ => {}
        }
    }

    fn consume(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.in_record {
                if byte == b'\n' {
                    if !self.discard {
                        self.complete_record(true);
                    }
                    self.record.clear();
                    self.in_record = false;
                    self.discard = false;
                    self.line_start = true;
                    continue;
                }
                if self.discard {
                    continue;
                }
                if self.record.len() < MAX_RECORD_BYTES {
                    self.push_record_byte(byte);
                    continue;
                }
                if looks_like_envelope(&self.record) {
                    self.observer
                        .fail("pnpm reporter record exceeded its memory bound");
                    self.record.clear();
                    self.discard = true;
                    continue;
                }
                // Native pnpm and the JS Bole reporter emit the namespace
                // before variable payload. A large script line without that
                // header is ordinary output.
                let pending = if self.record.ends_with(RECORD_PREFIX) {
                    RECORD_PREFIX.len()
                } else {
                    usize::from(self.record.ends_with(&RECORD_PREFIX[..1]))
                };
                self.record.truncate(self.record.len() - pending);
                self.emit_plain_record(false);
                self.record.clear();
                self.line_start = false;
                if pending == RECORD_PREFIX.len() {
                    self.begin_record(RECORD_PREFIX);
                    self.push_record_byte(byte);
                    continue;
                }
                self.in_record = false;
                self.prefix = pending;
                // Process the overflow byte normally, including a new reporter
                // prefix after unterminated output.
            }
            // Within the record bound, accept any object field order. The
            // compact prefix also recognizes reports after a human prompt.
            if self.line_start && byte == b'{' {
                self.flush_human();
                self.begin_record(&[byte]);
                self.line_start = false;
                continue;
            }
            if byte == RECORD_PREFIX[self.prefix] {
                self.prefix += 1;
                if self.prefix == RECORD_PREFIX.len() {
                    self.flush_human();
                    self.prefix = 0;
                    self.begin_record(RECORD_PREFIX);
                }
            } else {
                self.human.extend_from_slice(&RECORD_PREFIX[..self.prefix]);
                self.prefix = 0;
                if byte == RECORD_PREFIX[0] {
                    self.prefix = 1;
                } else {
                    self.human.push(byte);
                }
            }
            if byte == b'\n' {
                self.line_start = true;
            } else if !byte.is_ascii_whitespace() {
                self.line_start = false;
            }
        }
        // Prompts and ordinary diagnostics need not contain a newline. Only
        // the native reporter envelope waits for its record delimiter.
        self.flush_human();
    }

    fn complete_record(&mut self, terminated: bool) {
        let Ok(record) = serde_json::from_slice::<Value>(&self.record) else {
            if looks_like_envelope(&self.record) {
                self.observer.fail(if terminated {
                    "pnpm emitted unreadable lifecycle telemetry"
                } else {
                    "pnpm ended with incomplete lifecycle telemetry"
                });
            } else {
                self.emit_plain_record(terminated);
            }
            return;
        };
        let name = record.get("name").and_then(Value::as_str);
        // Lifecycle scripts can write ordinary JSON on the reporter's channel.
        // A valid object needs pnpm's namespace before it is treated as telemetry.
        if !is_reporter_record(&record) {
            self.emit_plain_record(terminated);
            return;
        }
        if !terminated {
            self.observer
                .fail("pnpm ended with incomplete lifecycle telemetry");
            return;
        }
        self.observer.observe(&record);
        if name == Some("pnpm:lifecycle") {
            if let Some(line) = record.get("line").and_then(Value::as_str) {
                let channel = match record.get("stdio").and_then(Value::as_str) {
                    Some("stdout") => OutputChannel::Stdout,
                    _ => OutputChannel::Stderr,
                };
                self.emit_line(channel, line);
            } else if let (Some(stage), Some(script)) = (
                record.get("stage").and_then(Value::as_str),
                record.get("script").and_then(Value::as_str),
            ) {
                self.emit_line(OutputChannel::Stderr, &format!("{stage}: {script}"));
            }
        } else if matches!(name, Some("pnpm" | "pnpm:global" | "pnpm:pnpmfile")) {
            if let Some(message) = record.get("message").and_then(Value::as_str) {
                self.emit_line(OutputChannel::Stderr, message);
            }
        } else if name == Some("pnpm:ignored-scripts") {
            if let Some(names) = record.get("packageNames").and_then(Value::as_array) {
                let names = names.iter().filter_map(Value::as_str).collect::<Vec<_>>();
                if !names.is_empty() {
                    self.emit_line(
                        OutputChannel::Stderr,
                        &format!("Skipped dependency build scripts: {}", names.join(", ")),
                    );
                }
            }
        }
    }

    fn emit_plain_record(&mut self, terminated: bool) {
        let mut bytes = self.record.clone();
        if terminated {
            bytes.push(b'\n');
        }
        self.emit(self.channel, &bytes);
    }

    fn emit_line(&mut self, channel: OutputChannel, line: &str) {
        self.emit(channel, line.as_bytes());
        if !line.ends_with('\n') {
            self.emit(channel, b"\n");
        }
    }

    fn finish(&mut self) {
        if self.in_record && !self.discard {
            self.complete_record(false);
        }
        if self.prefix > 0 {
            self.human.extend_from_slice(&RECORD_PREFIX[..self.prefix]);
            self.prefix = 0;
        }
        self.flush_human();
    }
}

fn is_reporter_record(record: &Value) -> bool {
    record
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| {
            name.starts_with("pnpm:")
                || (name == "pnpm" && record.get("time").is_some() && record.get("pid").is_some())
        })
}

fn skip_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[1..];
    }
    bytes
}

fn has_field_prefix(bytes: &[u8], field: &[u8], value: Option<&[u8]>) -> bool {
    bytes
        .windows(field.len())
        .enumerate()
        .any(|(offset, part)| {
            if part != field {
                return false;
            }
            let tail = skip_whitespace(&bytes[offset + field.len()..]);
            let Some(tail) = tail.strip_prefix(b":") else {
                return false;
            };
            value.is_none_or(|value| skip_whitespace(tail).starts_with(value))
        })
}

fn looks_like_envelope(bytes: &[u8]) -> bool {
    has_field_prefix(bytes, b"\"name\"", Some(b"\"pnpm:"))
        || (has_field_prefix(bytes, b"\"name\"", Some(b"\"pnpm\""))
            && has_field_prefix(bytes, b"\"time\"", None)
            && has_field_prefix(bytes, b"\"pid\"", None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn lifecycle(field: &str, value: Value) -> Vec<u8> {
        let mut event = json!({
            "time": 1, "hostname": "private-host", "pid": 42,
            "name": "pnpm:lifecycle", "depPath": "fixture@1", "stage": "postinstall",
            "wd": "/private/fixture", "optional": false,
        });
        event[field] = value;
        if field == "line" {
            event["stdio"] = "stdout".into();
        }
        let mut bytes = serde_json::to_vec(&event).unwrap();
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn pnpm_reporter_preserves_normal_failures_and_private_human_output() {
        let observer = InstallObserver::default();
        let output = Rc::new(RefCell::new(Vec::<(OutputChannel, Vec<u8>)>::new()));
        let captured = output.clone();
        let mut decoder = InstallStream::new(
            observer.clone(),
            OutputChannel::Stderr,
            move |channel, bytes: &[u8]| {
                captured.borrow_mut().push((channel, bytes.to_vec()));
                Ok(())
            },
        );
        decoder.consume(b"Proceed? ");
        assert_eq!(output.borrow().concat_bytes().as_slice(), b"Proceed? ");
        // The test serializer deliberately puts depPath before time/name. The
        // same object must work after a partial prompt and across byte chunks.
        for record in [
            lifecycle("script", "node build.mjs".into()),
            lifecycle("line", "build failed".into()),
            lifecycle("exitCode", 1.into()),
        ] {
            for chunk in record.chunks(3) {
                decoder.consume(chunk);
            }
        }
        decoder.consume(b"Error: ordinary install failure\n");
        decoder.consume(b"{\"time\":1,\"pid\":42,\"name\":\"pnpm:context\",\"privateMetadata\":\"do-not-render\"}\n");
        decoder.finish();
        assert!(observer.finish().is_ok());
        let rendered = String::from_utf8(output.borrow().concat_bytes()).unwrap();
        assert!(rendered.contains("postinstall: node build.mjs\n"));
        assert!(rendered.contains("build failed\n"));
        assert!(rendered.contains("Error: ordinary install failure\n"));
        assert!(!rendered.contains("private-host"));
        assert!(!rendered.contains("do-not-render"));
        assert!(!rendered.contains("depPath"));
        assert!(
            output
                .borrow()
                .iter()
                .any(|(channel, bytes)| *channel == OutputChannel::Stdout
                    && bytes.as_slice() == b"build failed")
        );
    }

    #[test]
    fn pnpm_reporter_preserves_ordinary_braces_and_json() {
        let large = format!("{{\"data\":\"{}\"}}", "x".repeat(MAX_RECORD_BYTES + 17));
        let large_named = format!(
            "{{\"name\":\"fixture\",\"data\":\"{}\"}}\n",
            "x".repeat(MAX_RECORD_BYTES + 17)
        );
        for human in [
            "{\n  \"name\": \"fixture\",\n  \"pid\": 42,\n  \"time\": 1\n}\n",
            "{ build output }\n",
            "{\"time\":1,\"pid\":42,\"state\":\"done\"}\n",
            "{\"time\":1,\"pid\":42,\"state\":\"done\"}",
            "{\"name\":\"fixture\",\"time\":1,\"pid\":42}\n",
            "{\"name\":\"fixture\",\"time\":1,\"pid\":42,\n \"state\":\"done\"}\n",
            "{\"time\":1,\"pid\":42,\n \"state\":\"done\"}\n",
            "[{\"time\":1,\"pid\":42,\"state\":\"done\"}]\n",
            "{\"name\":\"pnpm\",\"version\":\"12.3.4\"}\n",
            "{\"name\":\"pnpm\",\"version\":\"12.3.4\",\n \"state\":\"done\"}\n",
            "{",
            &large,
            &large_named,
        ] {
            let observer = InstallObserver::default();
            let output = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&output);
            let mut decoder = InstallStream::new(
                observer.clone(),
                OutputChannel::Stdout,
                move |_, bytes: &[u8]| {
                    captured.borrow_mut().extend_from_slice(bytes);
                    Ok(())
                },
            );
            for chunk in human.as_bytes().chunks(2) {
                decoder.consume(chunk);
            }
            decoder.finish();
            assert!(observer.finish().is_ok(), "{human:?}");
            assert_eq!(output.borrow().as_slice(), human.as_bytes());
        }

        // A reporter can follow an unterminated ordinary line with its compact
        // prefix on either side of the buffer boundary.
        for prefix_bytes in 0..=RECORD_PREFIX.len() {
            let human = format!("{{{}", "x".repeat(MAX_RECORD_BYTES - prefix_bytes - 1));
            let observer = InstallObserver::default();
            let output = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&output);
            let mut decoder = InstallStream::new(
                observer.clone(),
                OutputChannel::Stderr,
                move |_, bytes: &[u8]| {
                    captured.borrow_mut().extend_from_slice(bytes);
                    Ok(())
                },
            );
            for chunk in human.as_bytes().chunks(8192) {
                decoder.consume(chunk);
            }
            decoder.consume(&lifecycle("script", "build".into()));
            decoder.consume(&lifecycle("exitCode", 0.into()));
            decoder.finish();
            assert!(observer.finish().is_ok());
            let expected = format!("{human}postinstall: build\n");
            assert_eq!(output.borrow().as_slice(), expected.as_bytes());
        }
    }

    #[test]
    fn pnpm_reporter_separates_adjacent_ordinary_json_and_reports() {
        for human in [
            r#"{"state":"done"}"#,
            r#"{"nested":[{"brace":"} ] {","quote":"\\\""}],"done":true}"#,
        ] {
            let observer = InstallObserver::default();
            let output = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&output);
            let mut decoder = InstallStream::new(
                observer.clone(),
                OutputChannel::Stderr,
                move |_, bytes: &[u8]| {
                    captured.borrow_mut().extend_from_slice(bytes);
                    Ok(())
                },
            );
            let mut input = human.as_bytes().to_vec();
            input.extend_from_slice(b"{\"time\":1,\"pid\":42,\"name\":\"pnpm:context\"}\n");
            input.extend(lifecycle("script", "build".into()));
            input.extend(lifecycle("exitCode", 0.into()));
            for chunk in input.chunks(2) {
                decoder.consume(chunk);
            }
            decoder.finish();
            assert!(observer.finish().is_ok(), "{human}");
            let expected = format!("{human}postinstall: build\n");
            assert_eq!(output.borrow().as_slice(), expected.as_bytes());
        }
    }

    trait OutputBytes {
        fn concat_bytes(&self) -> Vec<u8>;
    }
    impl OutputBytes for Vec<(OutputChannel, Vec<u8>)> {
        fn concat_bytes(&self) -> Vec<u8> {
            self.iter()
                .flat_map(|(_, bytes)| bytes.iter().copied())
                .collect()
        }
    }

    #[test]
    fn pnpm_reporter_keeps_signal_and_incomplete_observations_unsafe() {
        let mut cases = vec![
            vec![lifecycle("script", "node build.mjs".into())],
            vec![lifecycle("exitCode", 0.into())],
            vec![
                lifecycle("script", "a".into()),
                lifecycle("script", "b".into()),
            ],
            vec![b"{\"time\":1,\"pid\":42,\"name\":\"pnpm:lifecycle\",broken}\n".to_vec()],
            vec![b"{\"name\":\"pnpm:lifecycle\",\"time\":1,\"pid\":42".to_vec()],
            vec![
                b"{ \"name\" : \"pnpm:lifecycle\", \"time\" : 1, \"pid\" : 42, broken }\n".to_vec(),
            ],
            vec![b"{ \"name\" : \"pnpm:lifecycle\", \"time\" : 1, \"pid\" : 42".to_vec()],
            vec![b"{\"name\":\"pnpm:context\",\"time\":1,\"pid\":42}".to_vec()],
            vec![b"{\"name\":\"pnpm:context\"}{\"name\":\"pnpm:context\"}\n".to_vec()],
        ];
        for code in [-1, 137, 143] {
            if code < 0 || reserved_signal_exit(code) {
                cases.push(vec![
                    lifecycle("script", "node build.mjs".into()),
                    lifecycle("exitCode", code.into()),
                ]);
            }
        }
        for records in cases {
            let observer = InstallObserver::default();
            let mut decoder = InstallStream::new(
                observer.clone(),
                OutputChannel::Stderr,
                |_, _: &[u8]| Ok(()),
            );
            for record in records {
                decoder.consume(&record);
            }
            decoder.finish();
            assert!(!observer.finish().unwrap_err().cleanup_verified);
        }
    }

    #[test]
    fn pnpm_reporter_tracks_concurrent_lifecycles_and_optional_signals() {
        let observer = InstallObserver::default();
        let mut decoder = InstallStream::new(
            observer.clone(),
            OutputChannel::Stderr,
            |_, _: &[u8]| Ok(()),
        );
        let mut second: Value =
            serde_json::from_slice(&lifecycle("script", "second".into())).unwrap();
        second["pid"] = 43.into();
        second["optional"] = true.into();
        decoder.consume(&lifecycle("script", "first".into()));
        let mut bytes = serde_json::to_vec(&second).unwrap();
        bytes.push(b'\n');
        decoder.consume(&bytes);
        decoder.consume(&lifecycle("exitCode", 1.into()));
        second.as_object_mut().unwrap().remove("script");
        second["exitCode"] = (-1).into();
        let mut bytes = serde_json::to_vec(&second).unwrap();
        bytes.push(b'\n');
        decoder.consume(&bytes);
        decoder.finish();
        // An optional script's signal cannot be erased by pnpm returning zero.
        assert!(!observer.finish().unwrap_err().cleanup_verified);
    }

    #[test]
    fn pnpm_reporter_bounds_records_and_active_state_without_a_lifetime_output_cap() {
        let observer = InstallObserver::default();
        let mut decoder = InstallStream::new(
            observer.clone(),
            OutputChannel::Stderr,
            |_, _: &[u8]| Ok(()),
        );
        let progress = format!(
            "{{\"time\":1,\"pid\":42,\"name\":\"pnpm:progress\",\"data\":\"{}\"}}\n",
            "x".repeat(2048)
        );
        assert!(progress.len() * 3000 > MAX_RECORD_BYTES);
        for _ in 0..3000 {
            decoder.consume(progress.as_bytes());
        }
        decoder.finish();
        assert!(observer.finish().is_ok());

        let oversized = InstallObserver::default();
        let mut decoder =
            InstallStream::new(oversized.clone(), OutputChannel::Stderr, |_, _: &[u8]| {
                Ok(())
            });
        decoder.consume(b"{\"time\":1,\"pid\":42,\"name\":\"pnpm:progress\",");
        decoder.consume(&vec![b'x'; MAX_RECORD_BYTES]);
        decoder.consume(b"}\n");
        decoder.finish();
        assert!(!oversized.finish().unwrap_err().cleanup_verified);

        let active = InstallObserver::default();
        for pid in 1..=(MAX_ACTIVE_LIFECYCLES + 1) {
            let mut record: Value =
                serde_json::from_slice(&lifecycle("script", "build".into())).unwrap();
            record["pid"] = (pid as u64).into();
            active.observe(&record);
        }
        assert!(!active.finish().unwrap_err().cleanup_verified);
    }

    #[test]
    fn pnpm_reporter_continues_observing_after_a_human_output_failure() {
        let observer = InstallObserver::default();
        let mut decoder =
            InstallStream::new(observer.clone(), OutputChannel::Stderr, |_, _: &[u8]| {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "consumer closed"))
            });
        decoder.consume(&lifecycle("script", "build".into()));
        decoder.consume(&lifecycle("line", "output".into()));
        decoder.consume(&lifecycle("exitCode", 1.into()));
        decoder.finish();
        assert!(decoder.output_error.is_some());
        assert!(observer.finish().is_ok());
    }

    #[test]
    fn pnpm_reporter_flushes_partial_human_prompts() {
        #[derive(Default)]
        struct BufferedOutput {
            bytes: Vec<u8>,
            flushed: bool,
        }
        impl Write for BufferedOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushed = true;
                Ok(())
            }
        }
        let mut output = BufferedOutput::default();
        write_and_flush(&mut output, b"Proceed? ").unwrap();
        assert!(output.flushed);
        assert_eq!(output.bytes.as_slice(), b"Proceed? ");
    }

    #[test]
    fn pnpm_reporter_respects_verified_native_job_cancellation() {
        let observer = InstallObserver::default();
        let start: Value = serde_json::from_slice(&lifecycle("script", "build".into())).unwrap();
        observer.observe(&start);
        assert!(!observer.finish().unwrap_err().cleanup_verified);
        assert!(observer.finish_after_job_cleanup(true).is_ok());
        let diagnostic = observer.finish_after_job_cleanup(false).unwrap_err();
        assert!(diagnostic.cleanup_verified);
        assert!(!diagnostic.message.contains("ownership was retained"));
    }
}
