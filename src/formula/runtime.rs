use super::{Definition, manager::Run};
mod files;
pub(crate) mod helper;
use crate::{
    ipc::{self, Request},
    journal::{Page, private_file},
    session::Status,
};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use regex::bytes::{Regex, RegexBuilder};
use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult, INT, Scope};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    net::IpAddr,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const BUFFER_LIMIT: usize = 1024 * 1024;
type ScriptResult<T> = std::result::Result<T, Box<EvalAltResult>>;
fn script_error(e: anyhow::Error) -> Box<EvalAltResult> {
    format!("{e:#}").into()
}
fn dynamic(v: Value) -> Result<Dynamic> {
    Ok(rhai::serde::to_dynamic(v)?)
}
fn regex(pattern: &str) -> Result<Regex> {
    if pattern.len() > 4096 {
        bail!("regex pattern exceeds 4096 bytes");
    }
    Ok(RegexBuilder::new(pattern)
        .size_limit(2 * 1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .nest_limit(64)
        .build()?)
}
fn engine() -> Engine {
    let mut e = Engine::new();
    e.set_optimization_level(rhai::OptimizationLevel::None)
        .set_max_operations(50_000_000)
        .set_max_call_levels(32)
        .set_max_expr_depths(64, 64)
        .set_max_variables(256)
        .set_max_functions(128)
        .set_max_string_size(BUFFER_LIMIT)
        .set_max_array_size(65536)
        .set_max_map_size(1024)
        .set_fail_on_invalid_map_property(true)
        .disable_symbol("eval");
    e
}
pub(super) fn compile(d: &Definition) -> Result<()> {
    let _ = engine().compile(d.source.as_deref().context("missing formula source")?)?;
    Ok(())
}

struct Artifact {
    file: File,
    path: PathBuf,
    size: u64,
    hash: Sha256,
    closed: bool,
    require_close: bool,
}
struct Host {
    run: Arc<Run>,
    cursor: u64,
    buffer: Vec<u8>,
    claimed: bool,
    sent: bool,
    partial: bool,
    artifacts: BTreeMap<String, Artifact>,
    output: Option<PathBuf>,
}
impl Host {
    fn status(&self) -> Result<Status> {
        Ok(serde_json::from_value(ipc::call(
            &self.run.session,
            &Request::Status,
        )?)?)
    }
    fn claim(&mut self) -> Result<()> {
        self.run.check()?;
        if !self.run.definition.interactive {
            bail!("formula needs interactive=true to claim or send UART input");
        }
        ipc::call(
            &self.run.session,
            &Request::Claim {
                actor: self.run.actor.clone(),
                client_id: self.run.client.clone(),
                takeover: false,
            },
        )?;
        self.claimed = true;
        Ok(())
    }
    fn release(&mut self) -> Result<()> {
        if self.claimed {
            ipc::call(
                &self.run.session,
                &Request::Release {
                    client_id: self.run.client.clone(),
                },
            )?;
            self.claimed = false;
            self.sent = false;
            self.partial = false;
        }
        Ok(())
    }
    fn mark(&mut self) -> Result<INT> {
        self.cursor = self.status()?.latest_cursor;
        self.buffer.clear();
        Ok(self.cursor as INT)
    }
    fn send(&mut self, data: Blob, boundary: bool) -> Result<INT> {
        if !self.run.definition.interactive || !self.claimed {
            bail!("claim interactive input before sending");
        }
        if data.is_empty() || data.len() > 4096 {
            bail!("send requires 1..4096 bytes");
        }
        self.run.check()?;
        // A transport failure can mean the write happened. Preserve ownership.
        self.sent = true;
        self.partial = true;
        let v = ipc::call(
            &self.run.session,
            &Request::Send {
                data_base64: STANDARD.encode(&data),
                actor: self.run.actor.clone(),
                client_id: self.run.client.clone(),
                release: false,
            },
        )?;
        self.partial = !boundary;
        v["written"].as_i64().context("invalid send response")
    }
    fn receive(&mut self, wait_ms: u64) -> Result<()> {
        self.run.check()?;
        let page: Page = serde_json::from_value(ipc::call(
            &self.run.session,
            &Request::Read {
                after: self.cursor,
                limit: 32,
                wait_ms: wait_ms.min(50),
            },
        )?)?;
        if page.history_gap {
            bail!(
                "RX history gap: unread data was evicted; refusing to continue an incomplete exchange"
            );
        }
        for event in page.events {
            if event.kind == "rx"
                && let Some(bytes) = event.data_base64
            {
                let bytes = STANDARD.decode(bytes)?;
                if self.buffer.len() + bytes.len() > BUFFER_LIMIT {
                    bail!(
                        "unconsumed RX exceeds 1 MiB; read smaller chunks or narrow the expected response"
                    );
                }
                self.buffer.extend(bytes);
            }
        }
        self.cursor = page.next_cursor;
        self.run.check()?;
        Ok(())
    }
    fn expect(&mut self, pattern: &Regex, timeout_ms: INT) -> Result<Dynamic> {
        let deadline = wait_deadline(timeout_ms)?;
        loop {
            self.run.check()?;
            if let Some(captures) = pattern.captures(&self.buffer) {
                let m = captures.get(0).unwrap();
                if m.is_empty() {
                    bail!("expect must consume at least one byte");
                }
                let end = m.end();
                let before = self.buffer[..m.start()].to_vec();
                let matched = m.as_bytes().to_vec();
                let consumed = self.buffer[..end].to_vec();
                let groups: Vec<_> = captures
                    .iter()
                    .map(|g| {
                        g.map_or(String::new(), |m| {
                            String::from_utf8_lossy(m.as_bytes()).into_owned()
                        })
                    })
                    .collect();
                self.buffer.drain(..end);
                let mut value = dynamic(
                    json!({"matched":true,"text":String::from_utf8_lossy(&consumed),
                    "groups":groups,"cursor":self.cursor}),
                )?
                .cast::<rhai::Map>();
                value.insert("bytes".into(), Dynamic::from_blob(consumed));
                value.insert("before".into(), Dynamic::from_blob(before));
                value.insert("match_bytes".into(), Dynamic::from_blob(matched));
                return Ok(value.into());
            }
            if Instant::now() >= deadline {
                return dynamic(
                    json!({"matched":false,"cursor":self.cursor,"buffered":self.buffer.len()}),
                );
            }
            self.receive(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(50) as u64,
            )?;
        }
    }
    fn read_rx(&mut self, max: INT, timeout_ms: INT) -> Result<Blob> {
        if !(1..=65536).contains(&max) {
            bail!("read_bytes size must be 1..65536");
        }
        let deadline = wait_deadline(timeout_ms)?;
        while self.buffer.is_empty() {
            self.receive(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(50) as u64,
            )?;
            if Instant::now() >= deadline {
                break;
            }
        }
        self.run.check()?;
        Ok(self
            .buffer
            .drain(..self.buffer.len().min(max as usize))
            .collect())
    }
    fn history(&self, after: INT) -> Result<Dynamic> {
        if after < 0 {
            bail!("history cursor must be nonnegative");
        }
        let mut page: Page = serde_json::from_value(ipc::call(
            &self.run.session,
            &Request::Read {
                after: after as u64,
                limit: 32,
                wait_ms: 0,
            },
        )?)?;
        page.events.retain(|e| e.seq <= self.run.history_end);
        page.next_cursor = page.next_cursor.min(self.run.history_end);
        page.has_more = page.next_cursor < self.run.history_end;
        let mut value = dynamic(serde_json::to_value(&page)?)?.cast::<rhai::Map>();
        let mut events = Array::new();
        for event in page.events {
            let bytes = STANDARD.decode(event.data_base64.as_deref().unwrap_or(""))?;
            let mut row = dynamic(serde_json::to_value(event)?)?.cast::<rhai::Map>();
            row.insert("bytes".into(), Dynamic::from_blob(bytes));
            events.push(row.into());
        }
        value.insert("events".into(), events.into());
        Ok(value.into())
    }
    fn artifact_open(&mut self, name: &str) -> Result<()> {
        if name.is_empty()
            || name.len() > 128
            || name == "."
            || name == ".."
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            bail!("artifact name must be a plain filename (letters, digits, '.', '_', '-')");
        }
        if self.artifacts.len() >= 8 || self.artifacts.contains_key(name) {
            bail!("artifact exists or eight-file limit reached");
        }
        if self.output.is_none() {
            let parent = self
                .run
                .output_dir
                .as_ref()
                .context("artifact output requires --output-dir (or MCP output_dir)")?;
            fs::create_dir_all(parent)?;
            let id = self.run.data.lock().unwrap().id.clone();
            let dir = parent.join(format!("formula-{id}"));
            fs::DirBuilder::new().mode(0o700).create(&dir)?;
            self.output = Some(dir);
        }
        let path = self.output.as_ref().unwrap().join(name);
        let file = private_file(&path)?;
        self.artifacts.insert(
            name.into(),
            Artifact {
                file,
                path,
                size: 0,
                hash: Sha256::new(),
                closed: false,
                require_close: false,
            },
        );
        self.artifact_status(false);
        Ok(())
    }
    fn artifact_write(&mut self, name: &str, bytes: Blob) -> Result<INT> {
        if bytes.len() > BUFFER_LIMIT {
            bail!("artifact chunk exceeds 1 MiB");
        }
        let a = self
            .artifacts
            .get_mut(name)
            .context("open artifact before writing")?;
        if a.closed {
            bail!("artifact is already closed");
        }
        if a.size + bytes.len() as u64 > 64 * 1024 * 1024 * 1024 {
            bail!("artifact exceeds 64 GiB");
        }
        let mut written = 0;
        let result = (|| -> Result<()> {
            while written < bytes.len() {
                self.run.check()?;
                let n = a.file.write(&bytes[written..])?;
                if n == 0 {
                    bail!("artifact write returned zero");
                }
                a.hash.update(&bytes[written..written + n]);
                a.size += n as u64;
                written += n;
            }
            Ok(())
        })();
        self.artifact_status(false);
        result?;
        Ok(written as INT)
    }
    fn artifact_close(&mut self, name: &str) -> Result<()> {
        let a = self.artifacts.get_mut(name).context("unknown artifact")?;
        a.file.sync_all()?;
        a.closed = true;
        self.artifact_status(false);
        Ok(())
    }
    fn artifact_status(&self, success: bool) {
        self.run.data.lock().unwrap().artifacts = self.artifacts.iter().map(|(name,a)|
            json!({"name":name,"path":a.path,"bytes":a.size,"sha256":a.hash.clone().finalize().iter().map(|b|format!("{b:02x}")).collect::<String>(),
                "complete":a.closed || (success && !a.require_close)})).collect();
    }
    fn finish(&mut self, success: bool) -> Result<()> {
        let mut sync_error = None;
        for a in self.artifacts.values_mut() {
            if let Err(e) = a.file.sync_all() {
                a.closed = false;
                sync_error = Some(anyhow::anyhow!("artifact sync failed: {e}"));
            }
        }
        self.artifact_status(success && sync_error.is_none());
        if self.claimed && ((!self.sent) || (success && !self.partial)) {
            let _ = self.release();
        }
        let retained = self
            .status()
            .ok()
            .is_some_and(|s| s.writer.is_some_and(|w| w.client_id == self.run.client));
        self.run.data.lock().unwrap().writer_retained = retained;
        if retained {
            self.run.warn("Input remains reserved after partial or failed interaction; use Ctrl-] t to recover it, or explicitly release from the formula on a known boundary.");
        }
        if let Some(error) = sync_error {
            return Err(error);
        }
        Ok(())
    }
}
fn wait_deadline(ms: INT) -> Result<Instant> {
    if !(0..=3_600_000).contains(&ms) {
        bail!("wait must be 0..3600000 milliseconds");
    }
    Ok(Instant::now() + Duration::from_millis(ms as u64))
}

macro_rules! host_fn {
    ($e:ident, $ctx:ident, $name:literal, |$h:ident $(, $arg:ident : $ty:ty)*| -> $ret:ty $body:block) => {{
        let context = $ctx.clone();
        $e.register_fn($name, move |$($arg:$ty),*| -> ScriptResult<$ret> {
            let mut guard = context.lock().unwrap();
            #[allow(unused_mut)]
            let $h = &mut *guard;
            $h.run.check().map_err(script_error)?;
            #[allow(unused_mut)]
            let mut operation = || -> Result<$ret> { $body };
            operation().map_err(script_error)
        });
    }};
}
pub(super) fn execute(run: Arc<Run>) -> Result<()> {
    let ctx = Arc::new(Mutex::new(Host {
        cursor: run.history_end,
        run: run.clone(),
        buffer: Vec::new(),
        claimed: false,
        sent: false,
        partial: false,
        artifacts: BTreeMap::new(),
        output: None,
    }));
    let result = (|| -> Result<()> {
        let mut e = engine();
        let cancel = run.clone();
        let output_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let output_failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failed = output_failed.clone();
        e.on_progress(move |_| {
            if failed.load(std::sync::atomic::Ordering::Relaxed) {
                Some(Dynamic::from("formula output limit exceeded"))
            } else {
                cancel.check().err().map(|e| Dynamic::from(e.to_string()))
            }
        });
        let output = run.clone();
        let errors = output_error.clone();
        let failed = output_failed.clone();
        e.on_print(move |s| {
            if let Err(e) = output.emit(json!({"message":s})) {
                *errors.lock().unwrap() = Some(e.to_string());
                failed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        let output = run.clone();
        let errors = output_error.clone();
        let failed = output_failed.clone();
        e.on_debug(move |s, _, _| {
            if let Err(e) = output.emit(json!({"debug":s})) {
                *errors.lock().unwrap() = Some(e.to_string());
                failed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        host_fn!(e, ctx, "claim", |h| -> () { h.claim() });
        host_fn!(e, ctx, "release", |h| -> () { h.release() });
        host_fn!(e, ctx, "mark", |h| -> INT { h.mark() });
        host_fn!(e, ctx, "linux_probe", |h| -> Dynamic {
            dynamic(h.linux_operation("probe", "", "")?)
        });
        host_fn!(e, ctx, "linux_upload", |h,
                                          source: &str,
                                          path: &str,
                                          helper: &str,
                                          executable: bool|
         -> Dynamic {
            dynamic(h.linux_upload(source, path, helper, executable)?)
        });
        host_fn!(e, ctx, "linux_inspect", |h, helper: &str| -> Dynamic {
            dynamic(h.linux_inspect(helper, false)?)
        });
        host_fn!(e, ctx, "linux_collect_overview", |h,
                                                    helper: &str|
         -> Dynamic {
            dynamic(h.linux_inspect(helper, true)?)
        });
        host_fn!(e, ctx, "linux_list", |h, path: &str| -> Dynamic {
            dynamic(h.linux_operation("list", path, "")?)
        });
        host_fn!(e, ctx, "linux_download", |h,
                                            path: &str,
                                            name: &str|
         -> Dynamic {
            dynamic(h.linux_operation("download", path, name)?)
        });
        host_fn!(e, ctx, "linux_helper_install", |h,
                                                  arch: &str,
                                                  directory: &str|
         -> Dynamic {
            dynamic(h.helper_install(arch, directory)?)
        });
        host_fn!(e, ctx, "linux_probe", |h, helper: &str| -> Dynamic {
            dynamic(h.linux_with_helper("probe", "", "", helper)?)
        });
        host_fn!(e, ctx, "linux_list", |h,
                                        path: &str,
                                        helper: &str|
         -> Dynamic {
            dynamic(h.linux_with_helper("list", path, "", helper)?)
        });
        host_fn!(e, ctx, "linux_download", |h,
                                            path: &str,
                                            name: &str,
                                            helper: &str|
         -> Dynamic {
            dynamic(h.linux_with_helper("download", path, name, helper)?)
        });
        host_fn!(e, ctx, "send_bytes", |h, bytes: Blob| -> INT {
            h.send(bytes, false)
        });
        host_fn!(e, ctx, "send", |h, text: &str| -> INT {
            h.send(text.as_bytes().to_vec(), false)
        });
        host_fn!(e, ctx, "send_line", |h, text: &str| -> INT {
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(b'\r');
            h.send(bytes, true)
        });
        host_fn!(e, ctx, "read_bytes", |h,
                                        max: INT,
                                        timeout_ms: INT|
         -> Blob {
            h.read_rx(max, timeout_ms)
        });
        host_fn!(e, ctx, "expect", |h,
                                    pattern: &str,
                                    timeout_ms: INT|
         -> Dynamic {
            h.expect(&regex(pattern)?, timeout_ms)
        });
        host_fn!(e, ctx, "expect_text", |h,
                                         text: &str,
                                         timeout_ms: INT|
         -> Dynamic {
            h.expect(&regex(&regex::escape(text))?, timeout_ms)
        });
        host_fn!(e, ctx, "expect_bytes", |h,
                                          bytes: Blob,
                                          timeout_ms: INT|
         -> Dynamic {
            if bytes.is_empty() || bytes.len() > 512 {
                bail!("expect_bytes needs 1..512 bytes");
            }
            let pattern = format!(
                "(?-u:{})",
                bytes
                    .iter()
                    .map(|b| format!("\\x{b:02x}"))
                    .collect::<String>()
            );
            h.expect(&regex(&pattern)?, timeout_ms)
        });
        host_fn!(e, ctx, "sleep_ms", |h, ms: INT| -> () {
            let deadline = wait_deadline(ms)?;
            while Instant::now() < deadline {
                h.run.check()?;
                thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(20)),
                );
            }
            Ok(())
        });
        host_fn!(e, ctx, "history", |h, after: INT| -> Dynamic {
            h.history(after)
        });
        host_fn!(e, ctx, "emit", |h, value: Dynamic| -> () {
            h.run.emit(rhai::serde::from_dynamic(&value)?)
        });
        host_fn!(e, ctx, "progress", |h,
                                      done: INT,
                                      total: INT,
                                      message: &str|
         -> () {
            if message.len() > 2048 || done < 0 || total < 0 {
                bail!("invalid progress");
            }
            h.run.data.lock().unwrap().progress =
                json!({"done":done,"total":total,"message":message});
            Ok(())
        });
        host_fn!(e, ctx, "artifact_open", |h, name: &str| -> () {
            h.artifact_open(name)
        });
        host_fn!(e, ctx, "artifact_write", |h,
                                            name: &str,
                                            bytes: Blob|
         -> INT {
            h.artifact_write(name, bytes)
        });
        host_fn!(e, ctx, "artifact_write", |h,
                                            name: &str,
                                            text: &str|
         -> INT {
            h.artifact_write(name, text.as_bytes().to_vec())
        });
        host_fn!(e, ctx, "artifact_close", |h, name: &str| -> () {
            h.artifact_close(name)
        });
        host_fn!(e, ctx, "scan", |h, kind: &str| -> () {
            scan(&h.run, kind, None)
        });
        host_fn!(e, ctx, "scan_regex", |h,
                                        pattern: &str,
                                        kind: &str,
                                        capture: INT|
         -> () {
            if !(0..=64).contains(&capture) || kind.len() > 128 {
                bail!("invalid scan kind/capture");
            }
            scan(&h.run, kind, Some((regex(pattern)?, capture as usize)))
        });
        e.register_fn("hex_decode", |s: &str| -> ScriptResult<Blob> {
            decode_hex(s).map_err(script_error)
        });
        e.register_fn("hex_encode", |bytes: Blob| -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        });
        e.register_fn("utf8", |bytes: Blob| -> ScriptResult<String> {
            String::from_utf8(bytes).map_err(|e| e.to_string().into())
        });
        e.register_fn("bytes", |s: &str| -> Blob { s.as_bytes().to_vec() });
        e.register_fn(
            "matches",
            |pattern: &str, text: &str| -> ScriptResult<Dynamic> {
                matches(pattern, text).map_err(script_error)
            },
        );
        let mut scope = Scope::new();
        scope.push_constant("params", dynamic(run.params.clone())?);
        scope.push_constant("session_id", run.session.clone());
        scope.push_constant("run_id", run.data.lock().unwrap().id.clone());
        scope.push_constant("history_end", run.history_end as INT);
        let ast = e.compile_with_scope(&scope, run.definition.source.as_deref().unwrap())?;
        if run.definition.interactive {
            ctx.lock().unwrap().claim()?;
        }
        e.run_ast_with_scope(&mut scope, &ast)?;
        if let Some(error) = output_error.lock().unwrap().as_ref() {
            bail!("{error}");
        }
        run.check()?;
        Ok(())
    })();
    let cleanup = ctx.lock().unwrap().finish(result.is_ok());
    result.and(cleanup)
}
fn decode_hex(s: &str) -> Result<Blob> {
    if s.len() > BUFFER_LIMIT {
        bail!("hex input exceeds 1 MiB");
    }
    let digits: Vec<_> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        bail!("hex requires pairs of digits");
    }
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let hi = (pair[0] as char)
                .to_digit(16)
                .context("invalid hex digit")?;
            let lo = (pair[1] as char)
                .to_digit(16)
                .context("invalid hex digit")?;
            Ok((hi * 16 + lo) as u8)
        })
        .collect()
}
fn matches(pattern: &str, text: &str) -> Result<Dynamic> {
    let pattern = regex(pattern)?;
    let mut found = Vec::new();
    let mut size = 0;
    for c in pattern.captures_iter(text.as_bytes()) {
        let m = c.get(0).unwrap();
        let groups: Vec<_> = c
            .iter()
            .map(|g| {
                g.map_or(String::new(), |m| {
                    String::from_utf8_lossy(m.as_bytes()).into_owned()
                })
            })
            .collect();
        size += groups.iter().map(String::len).sum::<usize>();
        if found.len() >= 1024 || size > BUFFER_LIMIT {
            bail!("matches result exceeds 1024 matches / 1 MiB");
        }
        found.push(json!({"start":m.start(),"end":m.end(),"text":String::from_utf8_lossy(m.as_bytes()),"groups":groups}));
    }
    dynamic(json!(found))
}

#[derive(Default)]
struct Line {
    filter: crate::search::TextFilter,
    bytes: Vec<u8>,
    seq: u64,
    time: String,
}
struct Finding {
    kind: String,
    value: String,
    count: u64,
    locations: Vec<Value>,
}
fn scan(run: &Run, kind: &str, custom: Option<(Regex, usize)>) -> Result<()> {
    let password = regex(
        r#"(?i)\b(?:password|passwd|pwd|passphrase|psk|wifi[_ -]?(?:password|key))\b["']?\s*[:=]\s*(?:"([^"\r\n]+)"|'([^'\r\n]+)'|([^\s,;"']+))"#,
    )?;
    let urls = regex(r#"(?i)\b[a-z][a-z0-9+.-]{1,20}://[^\s<>"']+"#)?;
    let ips = regex(
        r"(?:\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b|[0-9A-Fa-f:]*:[0-9A-Fa-f:.]+(?:%[A-Za-z0-9_.-]+)?)",
    )?;
    if custom.is_none() && !["passwords", "endpoints"].contains(&kind) {
        bail!("unknown scan; use scan_regex for custom patterns");
    }
    let mut lines: BTreeMap<String, Line> = BTreeMap::new();
    let mut findings: BTreeMap<(String, String), Finding> = BTreeMap::new();
    let mut location_bytes = 0;
    let mut accept = |line: &Line, direction: &str| -> Result<()> {
        run.check()?;
        let mut values: Vec<(String, String, usize)> = Vec::new();
        if let Some((pattern, capture)) = &custom {
            if *capture >= pattern.captures_len() {
                bail!("scan capture does not exist in pattern");
            }
            for captures in pattern.captures_iter(&line.bytes) {
                if let Some(m) = captures.get(*capture)
                    && !m.is_empty()
                {
                    values.push((
                        kind.into(),
                        String::from_utf8_lossy(m.as_bytes()).into_owned(),
                        m.start(),
                    ));
                }
                if values.len() > 5000 {
                    bail!("too many matches in one line");
                }
            }
        } else if kind == "passwords" {
            for c in password.captures_iter(&line.bytes) {
                if let Some(m) = (1..=3).find_map(|i| c.get(i)) {
                    let value = String::from_utf8_lossy(m.as_bytes()).to_string();
                    if ![
                        "",
                        "none",
                        "null",
                        "<redacted>",
                        "redacted",
                        "***",
                        "****",
                        "*****",
                        "******",
                        "********",
                    ]
                    .contains(&value.to_lowercase().as_str())
                        && !value.chars().all(|c| c == '*')
                    {
                        values.push(("possible-password".into(), value, m.start()));
                    }
                }
            }
        } else {
            for m in urls.find_iter(&line.bytes) {
                let value = trim_url(&String::from_utf8_lossy(m.as_bytes()));
                if url::Url::parse(&value).is_ok_and(|url| url.host_str().is_some()) {
                    values.push(("url".into(), value, m.start()));
                }
            }
            for m in ips.find_iter(&line.bytes) {
                let value = String::from_utf8_lossy(m.as_bytes())
                    .trim_end_matches('.')
                    .to_owned();
                let address = value.split('%').next().unwrap();
                if address.parse::<IpAddr>().is_ok() {
                    values.push(("ip-candidate".into(), value, m.start()));
                }
            }
        }
        if values.len() > 5000 {
            bail!("too many matches in one line");
        }
        for (kind, value, offset) in values {
            run.check()?;
            if value.len() > 4096 {
                bail!("extracted value exceeds 4096 bytes");
            }
            let key = (kind.clone(), value.clone());
            if !findings.contains_key(&key) && findings.len() >= 5000 {
                bail!("scan exceeds 5000 unique findings");
            }
            let f = findings.entry(key).or_insert(Finding {
                kind,
                value,
                count: 0,
                locations: Vec::new(),
            });
            f.count += 1;
            if f.locations.len() < 8 {
                let start = offset.saturating_sub(128);
                let end = (offset + 512).min(line.bytes.len());
                let location = json!({"cursor":line.seq,"time":line.time,"direction":direction,"line_byte_offset":offset,
                    "context":String::from_utf8_lossy(&line.bytes[start..end])});
                location_bytes += serde_json::to_vec(&location)?.len();
                if location_bytes > 1024 * 1024 {
                    bail!("scan context exceeds 1 MiB; narrow the pattern");
                }
                f.locations.push(location);
            }
        }
        Ok(())
    };
    let mut cursor = 0;
    while cursor < run.history_end {
        run.check()?;
        let page: Page = serde_json::from_value(ipc::call(
            &run.session,
            &Request::Read {
                after: cursor,
                limit: 128,
                wait_ms: 0,
            },
        )?)?;
        if page.history_gap {
            run.warn("Earlier session history was evicted; this scan covers retained data only.");
            lines.clear();
        }
        for event in page.events {
            if event.seq > run.history_end {
                break;
            }
            if !["rx", "tx"].contains(&event.kind.as_str()) {
                continue;
            }
            let key = if event.kind == "rx" {
                "rx".into()
            } else {
                format!("tx:{}", event.actor)
            };
            if !lines.contains_key(&key) && lines.len() >= 64 {
                bail!("scan exceeds 64 traffic sources");
            }
            let line = lines.entry(key.clone()).or_default();
            let raw = STANDARD.decode(event.data_base64.as_deref().unwrap_or(""))?;
            let mut normalized = Vec::new();
            // TX usually ends in CR, while RX commonly uses CRLF.
            let raw = if event.kind == "tx" {
                raw.into_iter()
                    .map(|b| if b == b'\r' { b'\n' } else { b })
                    .collect::<Vec<_>>()
            } else {
                raw
            };
            line.filter.write(&raw, &mut normalized)?;
            for byte in normalized {
                if line.bytes.is_empty() {
                    line.seq = event.seq;
                    line.time = event.time.clone();
                }
                if byte == b'\n' {
                    accept(line, &key)?;
                    line.bytes.clear();
                } else {
                    if line.bytes.len() >= BUFFER_LIMIT {
                        bail!("formula scan line exceeds 1 MiB");
                    }
                    line.bytes.push(byte);
                }
            }
        }
        if page.next_cursor <= cursor {
            bail!("history stopped advancing");
        }
        cursor = page.next_cursor.min(run.history_end);
        run.data.lock().unwrap().progress =
            json!({"done":cursor,"total":run.history_end,"message":"Scanning retained history"});
    }
    for (key, line) in &lines {
        if !line.bytes.is_empty() {
            accept(line, key)?;
        }
    }
    for f in findings.into_values() {
        run.emit(
            json!({"kind":f.kind,"value":f.value,"count":f.count,"locations":f.locations,
            "locations_truncated":f.count > f.locations.len() as u64}),
        )?;
    }
    Ok(())
}

pub fn reference() -> &'static str {
    include_str!("../../docs/formula-api.txt")
}

fn trim_url(text: &str) -> String {
    let mut text = text.trim_end_matches(['.', ',', ';']);
    loop {
        let pair = match text.as_bytes().last() {
            Some(b')') => Some(('(', ')')),
            Some(b']') => Some(('[', ']')),
            Some(b'}') => Some(('{', '}')),
            _ => None,
        };
        if let Some((open, close)) = pair
            && text.matches(close).count() > text.matches(open).count()
        {
            text = &text[..text.len() - 1];
        } else {
            break;
        }
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_delimiters_preserve_ipv6_hosts_and_balanced_paths() {
        assert_eq!(trim_url("http://[2001:db8::1]"), "http://[2001:db8::1]");
        assert_eq!(
            trim_url("https://example.test/a(b))."),
            "https://example.test/a(b)"
        );
    }
    #[test]
    fn binary_and_regex_helpers_reject_ambiguous_or_unbounded_inputs() {
        assert_eq!(decode_hex("00 ff 01\n").unwrap(), [0, 255, 1]);
        assert!(decode_hex("a").is_err());
        assert!(decode_hex("gg").is_err());
        assert!(regex("[").is_err());
        assert!(regex(r"a{100000000}").is_err());
        assert!(
            engine()
                .compile("eval(\"send_line(\\\"bad\\\")\")")
                .is_err()
        );
    }
}
