//! Framed shell exchanges. The broker still owns every UART byte and job.
use super::*;
mod extended;

const CHUNK: u64 = 1024;
const MAX_FILE: u64 = 64 * 1024 * 1024;
const ATTEMPTS: usize = 3;

#[derive(Clone, Serialize)]
struct Capabilities {
    helper: Option<String>,
    features: Vec<String>,
    encoder: String,
    encoding: String,
    reader: String,
    checksum: String,
    checksum_kind: String,
    size: Option<String>,
    printf: Option<String>,
}
use serde::Serialize;

#[derive(PartialEq)]
struct Fingerprint {
    value: String,
    len: u64,
}
impl Capabilities {
    fn parse(&self, bytes: &[u8]) -> Result<Fingerprint> {
        if self.checksum_kind == "cksum" {
            let (crc, len) = parse_checksum(bytes)?;
            return Ok(Fingerprint {
                value: crc.to_string(),
                len,
            });
        }
        let parts: Vec<_> = std::str::from_utf8(bytes)?
            .split_ascii_whitespace()
            .collect();
        if parts.len() != 3 || !valid_sha256(parts[0]) || parts[1] != "-" {
            bail!("invalid SHA-256/length response (console noise or unsupported utility)");
        }
        Ok(Fingerprint {
            value: parts[0].to_ascii_lowercase(),
            len: parts[2].parse()?,
        })
    }
    fn matches(&self, bytes: &[u8], expected: &Fingerprint) -> bool {
        let value = if self.checksum_kind == "cksum" {
            checksum(bytes).0.to_string()
        } else {
            hex(&Sha256::digest(bytes))
        };
        value == expected.value && bytes.len() as u64 == expected.len
    }
    fn check_command(&self, producer: &str) -> String {
        let mut command = format!("{producer} | {}", self.checksum);
        if let Some(size) = &self.size {
            command.push_str(&format!("; {producer} | {size}"));
        }
        command
    }
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// POSIX cksum includes the little-endian length after the file bytes, then
// complements the CRC. It is not the reflected CRC used by ZIP/zlib.
#[derive(Default)]
struct Cksum {
    crc: u32,
    len: u64,
}
impl Cksum {
    fn byte(&mut self, b: u8) {
        self.crc ^= (b as u32) << 24;
        for _ in 0..8 {
            self.crc = (self.crc << 1)
                ^ if self.crc & 0x8000_0000 != 0 {
                    0x04c1_1db7
                } else {
                    0
                };
        }
    }
    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
        self.len += bytes.len() as u64;
    }
    fn finish(mut self) -> (u32, u64) {
        let mut n = self.len;
        while n != 0 {
            self.byte(n as u8);
            n >>= 8;
        }
        (!self.crc, self.len)
    }
}
fn checksum(bytes: &[u8]) -> (u32, u64) {
    let mut c = Cksum::default();
    c.update(bytes);
    c.finish()
}
pub(super) fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
// Small BusyBox line editors can truncate at 128 bytes. Continue commands in
// short physical lines without changing their tokens. In single quotes, close
// and reopen the quote around the continuation; a bare backslash there would
// instead become part of the filename. Fixed double-quoted expressions remain
// intact, so variable names cannot be split by added quote boundaries.
fn shell_lines(command: &str) -> String {
    let mut output = String::new();
    let mut column = 0;
    let mut quote = None;
    let mut escaped = false;
    for ch in command.chars() {
        if column >= 80 && quote != Some('"') && !escaped && ch != '\r' {
            if quote == Some('\'') {
                output.push_str("'\\\r'");
                column = 1;
            } else {
                output.push_str("\\\r");
                column = 0;
            }
        }
        output.push(ch);
        column += ch.len_utf8();
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (None, '\'' | '"') => quote = Some(ch),
            (None | Some('"'), '\\') => escaped = true,
            _ => (),
        }
    }
    output
}
pub(super) fn path(s: &str) -> Result<String> {
    if !s.starts_with('/') || s.len() > 512 || s.chars().any(char::is_control) {
        bail!("DUT path must be absolute, at most 512 bytes, with no control characters");
    }
    Ok(quote(s))
}
fn parse_checksum(bytes: &[u8]) -> Result<(u32, u64)> {
    let text = std::str::from_utf8(bytes)?.trim();
    let words: Vec<_> = text.split_ascii_whitespace().collect();
    if words.len() != 2 {
        bail!("invalid checksum response (console noise or unsupported cksum)");
    }
    Ok((words[0].parse()?, words[1].parse()?))
}
fn decode(bytes: &[u8], encoding: &str) -> Result<Vec<u8>> {
    // Only transport whitespace is dispensable. In particular, do not filter
    // arbitrary log characters: valid-alphabet corruption needs the CRC too.
    let compact: Vec<_> = bytes
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if encoding == "base64" {
        return Ok(STANDARD.decode(compact)?);
    }
    if compact.len() % 2 != 0 {
        bail!("odd hex payload length");
    }
    compact
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| Ok(u8::from_str_radix(std::str::from_utf8(p)?, 16)?))
        .collect()
}

impl Host {
    // A completed end marker establishes a shell command boundary, even when
    // payload validation fails. Cancellation/timeouts during an exchange retain
    // input: never inject Ctrl-C after a different client has taken over.
    pub(super) fn linux_operation(&mut self, op: &str, remote: &str, name: &str) -> Result<Value> {
        self.linux_with_helper(op, remote, name, "")
    }
    pub(super) fn linux_with_helper(
        &mut self,
        op: &str,
        remote: &str,
        name: &str,
        helper: &str,
    ) -> Result<Value> {
        self.claim()?;
        let result = (|| {
            if op != "probe" {
                path(remote)?;
            }
            if op == "download" {
                self.run
                    .output_dir
                    .as_ref()
                    .context("download requires an explicit host output directory")?;
            }
            let caps = if helper.is_empty() {
                self.file_probe()?
            } else {
                self.helper_probe(helper)?
            };
            match op {
                "probe" => Ok(
                    json!({"supported":true,"capabilities":caps,"chunk_bytes":CHUNK,"max_bytes":MAX_FILE}),
                ),
                "list" => self.file_list(remote, &caps),
                "download" => self.file_download(remote, name, &caps),
                _ => bail!("unknown file operation"),
            }
        })();
        if !self.partial {
            self.release()?;
        }
        result
    }

    pub(super) fn exchange(&mut self, body: &str) -> Result<(Vec<u8>, String)> {
        self.exchange_timeout(body, Duration::from_secs(10))
    }
    pub(super) fn exchange_timeout(
        &mut self,
        body: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, String)> {
        self.run.check()?;
        let tag = format!("SC{}", uuid::Uuid::new_v4().simple());
        // Markers are expanded from a variable so echoed input cannot match.
        // All shell state is scoped to a subshell; no stty, files, or persistent
        // shell options are changed on the DUT.
        let command = format!(
            "(t={}; echo; echo \"$t:B\"; {}; s=$?; echo; echo \"$t:E:$s\"; echo \"$t:E:$s\")\r",
            quote(&tag),
            body
        );
        let command = shell_lines(&command);
        if command.len() > 4096 {
            bail!("generated shell command exceeds 4096 bytes");
        }
        self.mark()?;
        self.send(command.into_bytes(), false)?;
        let begin = format!("{tag}:B\r\n");
        let end = regex(&format!(r"(?:\r?\n){}:E:([0-9]+)\r?\n", tag))?;
        let deadline = Instant::now() + timeout;
        loop {
            self.run.check()?;
            if let Some(c) = end.captures(&self.buffer) {
                let stop = c.get(0).unwrap().start();
                let consumed = c.get(0).unwrap().end();
                let code = std::str::from_utf8(c.get(1).unwrap().as_bytes())?.parse::<u32>()?;
                let start = self
                    .buffer
                    .windows(begin.len())
                    .position(|w| w == begin.as_bytes())
                    .map(|n| n + begin.len())
                    .or_else(|| {
                        let b = format!("{tag}:B\n");
                        self.buffer
                            .windows(b.len())
                            .position(|w| w == b.as_bytes())
                            .map(|n| n + b.len())
                    });
                let payload = start
                    .filter(|s| *s <= stop)
                    .map(|s| self.buffer[s..stop].to_vec());
                self.buffer.drain(..consumed);
                self.partial = false;
                if code != 0 {
                    let detail = payload
                        .as_deref()
                        .map(|p| {
                            String::from_utf8_lossy(&p[p.len().saturating_sub(512)..])
                                .trim()
                                .to_owned()
                        })
                        .unwrap_or_default();
                    bail!(
                        "DUT command failed (exit {code}); check path, permissions and shell tools: {detail}"
                    );
                }
                return Ok((
                    payload.context("response start marker corrupted by console noise")?,
                    tag,
                ));
            }
            if Instant::now() >= deadline {
                bail!(
                    "no shell completion marker within {} seconds; input retained for recovery (Ctrl-] t). Check shell/login, noise or a reboot",
                    timeout.as_secs()
                );
            }
            self.receive(50)?;
        }
    }
    fn probe_one(
        &mut self,
        candidates: &[&str],
        expected: impl Fn(&[u8]) -> bool,
    ) -> Result<Option<String>> {
        for candidate in candidates {
            for _ in 0..ATTEMPTS {
                match self.exchange(&format!("echo sericon | {candidate} 2>/dev/null")) {
                    Ok((bytes, _)) if expected(&bytes) => return Ok(Some((*candidate).into())),
                    _ if self.partial => {
                        bail!("shell probe did not complete; input retained for recovery")
                    }
                    _ => {
                        self.run.check()?;
                    }
                }
            }
        }
        Ok(None)
    }
    fn file_probe(&mut self) -> Result<Capabilities> {
        self.run.data.lock().unwrap().progress =
            json!({"stage":"probing","message":"Checking existing Linux shell tools"});
        let crc = self.probe_one(&["cksum", "busybox cksum"], |b| {
            parse_checksum(b).ok() == Some(checksum(b"sericon\n"))
        })?;
        let (c, checksum_kind, size) = if let Some(c) = crc {
            (c, "cksum".to_owned(), None)
        } else {
            let expected = hex(&Sha256::digest(b"sericon\n"));
            let c = self.probe_one(&["sha256sum", "busybox sha256sum"], |b| {
                let text = String::from_utf8_lossy(b);
                let words: Vec<_> = text.split_ascii_whitespace().collect();
                words == [expected.as_str(), "-"]
            })?.context("Linux Files needs an existing cksum utility, or sha256sum plus wc (standalone or BusyBox). No software was installed on the DUT")?;
            let size = self
                .probe_one(&["wc -c", "busybox wc -c"], |b| b.trim_ascii() == b"8")?
                .context("SHA-256 downloads also need wc -c for byte counts")?;
            (c, "sha256".to_owned(), Some(size))
        };
        let reader = self
            .probe_one(&["dd bs=8 count=1", "busybox dd bs=8 count=1"], |b| {
                b.trim_ascii() == b"sericon"
            })?
            .context("Linux Files needs dd on the DUT")?;
        let reader = reader.split(" bs=").next().unwrap().to_owned();
        let mut encoder = None;
        for (encoding, commands) in [
            ("base64", vec!["base64", "busybox base64"]),
            (
                "hex",
                vec![
                    "od -An -v -tx1",
                    "busybox od -An -v -tx1",
                    "hexdump -v -e '1/1 \"%02x\"'",
                    "busybox hexdump -v -e '1/1 \"%02x\"'",
                ],
            ),
        ] {
            if let Some(command) = self.probe_one(&commands, |b| {
                decode(b, encoding).is_ok_and(|v| v == b"sericon\n")
            })? {
                encoder = Some((command, encoding.to_owned()));
                break;
            }
        }
        let (encoder, encoding) =
            encoder.context("Linux Files needs base64, od or hexdump on the DUT")?;
        let mut printf = None;
        for p in ["printf", "busybox printf"] {
            if self
                .exchange(&format!("{p} '%s' sericon 2>/dev/null"))
                .is_ok_and(|(b, _)| b.trim_ascii() == b"sericon")
            {
                printf = Some(p.into());
                break;
            }
            if self.partial {
                bail!("printf probe did not complete; input retained");
            }
        }
        self.run.check()?;
        Ok(Capabilities {
            helper: None,
            features: vec![],
            encoder,
            encoding,
            reader,
            checksum: c,
            checksum_kind,
            size,
            printf,
        })
    }
    fn checked_payload(
        &mut self,
        command: &str,
        caps: &Capabilities,
        expected_len: Option<u64>,
    ) -> Result<Vec<u8>> {
        self.checked_payload_timeout(command, caps, expected_len, Duration::from_secs(10))
    }
    fn checked_payload_timeout(
        &mut self,
        command: &str,
        caps: &Capabilities,
        expected_len: Option<u64>,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let mut error = String::new();
        for attempt in 1..=ATTEMPTS {
            let received = self
                .exchange_timeout(command, timeout)
                .and_then(|(body, tag)| {
                    let divider = format!("{tag}:C");
                    let at = body
                        .windows(divider.len())
                        .position(|w| w == divider.as_bytes())
                        .context("missing chunk checksum marker")?;
                    let bytes = decode(&body[..at], &caps.encoding)?;
                    let expected = caps.parse(&body[at + divider.len()..])?;
                    if !caps.matches(&bytes, &expected)
                        || expected_len.is_some_and(|len| len != bytes.len() as u64)
                    {
                        bail!("chunk length/checksum mismatch (console noise or changing file)");
                    }
                    Ok(bytes)
                });
            match received {
                Ok(bytes) => return Ok(bytes),
                Err(e) => {
                    self.run.check()?;
                    if self.partial {
                        return Err(e);
                    }
                    error = format!("{e:#}");
                    self.run.warn("A response failed validation; bounded retries were used. Only validated chunks are saved.");
                    self.run.data.lock().unwrap().progress["retry"] = json!(attempt);
                    self.run.data.lock().unwrap().progress["message"] = json!(&error);
                }
            }
        }
        bail!("transfer failed after {ATTEMPTS} attempts: {error}");
    }
    fn file_list(&mut self, remote: &str, caps: &Capabilities) -> Result<Value> {
        let q = path(remote)?;
        let command = if let Some(helper) = &caps.helper {
            format!("{} \"$t\" list {q}", path(helper)?)
        } else {
            let printf = caps.printf.as_ref().context(
                "directory browsing needs printf; direct file downloads are still available",
            )?;
            // NUL-delimited type/path records preserve spaces, backslashes and
            // newlines. No parsing of ls columns or shell evaluation of filenames.
            let list = format!(
                "l() {{ n=0; for p in {q}/* {q}/.[!.]* {q}/..?*; do [ -e \"$p\" ] || [ -L \"$p\" ] || continue; n=$((n+1)); [ \"$n\" -le 512 ] || return 1; k=other; if [ -L \"$p\" ]; then k=link; elif [ -d \"$p\" ]; then k=directory; elif [ -f \"$p\" ]; then k=file; fi; {printf} '%s\\000%s\\000' \"$k\" \"$p\"; done; }}"
            );
            format!(
                "[ -d {q} ] && [ -r {q} ] && [ -x {q} ] && {{ {list}; l >/dev/null && {{ l | {}; echo; echo \"$t:C\"; {}; }}; }}",
                caps.encoder,
                caps.check_command("l")
            )
        };
        self.run.data.lock().unwrap().progress = json!({"stage":"listing","path":remote});
        let bytes = self.checked_payload(&command, caps, None)?;
        let parts: Vec<_> = bytes.split(|b| *b == 0).collect();
        if parts.last() != Some(&b"".as_slice()) || parts.len() % 2 != 1 {
            bail!("invalid directory records");
        }
        let mut entries = Vec::new();
        for pair in parts[..parts.len() - 1].as_chunks::<2>().0 {
            let p = std::str::from_utf8(pair[1])
                .context("non-UTF-8 filenames are not supported yet")?;
            entries.push(json!({"kind":std::str::from_utf8(pair[0])?,"path":p,"name":p.rsplit('/').next().unwrap_or(p)}));
        }
        entries.sort_by_key(|e| {
            (
                e["kind"] != "directory",
                e["path"].as_str().unwrap_or("").to_owned(),
            )
        });
        // Entries are separate results to respect formula result paging/limits.
        for entry in &entries {
            self.run.emit(entry.clone())?;
        }
        Ok(json!({"path":remote,"entries":entries.len(),"capabilities":caps}))
    }
    fn file_checksum(&mut self, q: &str, caps: &Capabilities) -> Result<Fingerprint> {
        if let Some(helper) = &caps.helper {
            let bytes = self.checked_payload_timeout(
                &format!("{} \"$t\" check {q}", path(helper)?),
                caps,
                None,
                Duration::from_secs(120),
            )?;
            return caps.parse(&bytes);
        }
        let mut command = format!("{} < {q}", caps.checksum);
        if let Some(size) = &caps.size {
            command.push_str(&format!("; {size} < {q}"));
        }
        let mut last = String::new();
        for _ in 0..ATTEMPTS {
            match self.exchange(&command).and_then(|(b, _)| caps.parse(&b)) {
                Ok(c) => return Ok(c),
                Err(e) => {
                    if self.partial {
                        return Err(e);
                    }
                    last = e.to_string();
                    self.run.check()?;
                }
            }
        }
        bail!("cannot read file checksum: {last}")
    }
    fn file_download(&mut self, remote: &str, name: &str, caps: &Capabilities) -> Result<Value> {
        let q = path(remote)?;
        // Reject known pseudo-filesystems, symlinks and nonregular files. A
        // canonical parent catches aliases such as /tmp/../proc and dir links.
        let (parent, _leaf) = remote.rsplit_once('/').context("invalid absolute path")?;
        let parent = quote(if parent.is_empty() { "/" } else { parent });
        if caps.helper.is_none() {
            self.exchange(&format!("p=$(cd -P {parent} && pwd) && case \"$p/\" in /proc/*|/sys/*|/dev/*) false;; *) [ -f {q} ] && [ ! -L {q} ] && [ -r {q} ];; esac"))?;
        }
        let expected = self.file_checksum(&q, caps)?;
        if expected.len > MAX_FILE {
            bail!("file exceeds the first-release 64 MiB limit");
        }
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.len() > 120
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            bail!(
                "host filename must be 1..120 letters, digits, dot, underscore or hyphen, excluding . and .."
            );
        }
        let partial = format!("{name}.partial");
        self.artifact_open(&partial)?;
        self.artifacts.get_mut(&partial).unwrap().require_close = true;
        let started = Instant::now();
        let mut whole = Cksum::default();
        let mut offset = 0;
        while offset < expected.len {
            self.run.check()?;
            let bytes_per_second = offset as f64 / started.elapsed().as_secs_f64().max(0.001);
            self.run.data.lock().unwrap().progress = json!({"stage":"downloading","path":remote,"bytes":offset,"total":expected.len,
                "bytes_per_second":bytes_per_second,"eta_seconds":if bytes_per_second > 0.0 { Some((expected.len-offset) as f64 / bytes_per_second) } else { None },"retry":0});
            let read = format!(
                "{} bs={CHUNK} skip={} count=1 < {q} 2>/dev/null",
                caps.reader,
                offset / CHUNK
            );
            let command = if let Some(helper) = &caps.helper {
                format!("{} \"$t\" read {q} {offset}", path(helper)?)
            } else {
                format!(
                    "r() {{ {read}; }}; r | {}; echo; echo \"$t:C\"; {}",
                    caps.encoder,
                    caps.check_command("r")
                )
            };
            let bytes =
                self.checked_payload(&command, caps, Some(CHUNK.min(expected.len - offset)))?;
            if caps.checksum_kind == "cksum" {
                whole.update(&bytes);
            }
            offset += self.artifact_write(&partial, bytes)? as u64;
        }
        self.run.data.lock().unwrap().progress =
            json!({"stage":"verifying","bytes":offset,"total":expected.len});
        let actual = if caps.checksum_kind == "cksum" {
            let (crc, len) = whole.finish();
            Fingerprint {
                value: crc.to_string(),
                len,
            }
        } else {
            let a = &self.artifacts[&partial];
            Fingerprint {
                value: hex(&a.hash.clone().finalize()),
                len: a.size,
            }
        };
        if self.file_checksum(&q, caps)? != expected || actual != expected {
            bail!("whole-file checksum changed or differs from DUT; partial download retained");
        }
        self.run.check()?;
        let a = self.artifacts.get_mut(&partial).unwrap();
        a.file.sync_all()?;
        let final_path = a.path.with_file_name(name);
        // Link without replacement, then unlink the temporary name. This is
        // atomic publication and never overwrites an existing destination.
        fs::hard_link(&a.path, &final_path)?;
        fs::remove_file(&a.path)?;
        a.path = final_path.clone();
        a.closed = true;
        let a = self.artifacts.remove(&partial).unwrap();
        self.artifacts.insert(name.into(), a);
        self.artifact_status(false);
        self.run.data.lock().unwrap().progress =
            json!({"stage":"completed","bytes":offset,"total":expected.len});
        Ok(
            json!({"path":remote,"destination":final_path,"bytes":offset,"verified":true,"verification":"DUT checksum before/after and per chunk","checksum_kind":caps.checksum_kind,"checksum":expected.value,"capabilities":caps}),
        )
    }

    fn helper_probe(&mut self, helper: &str) -> Result<Capabilities> {
        let mut caps = Capabilities {
            helper: Some(helper.into()),
            features: vec![],
            encoding: "hex".into(),
            encoder: "sericon-helper/1".into(),
            reader: "sericon-helper/1".into(),
            checksum: "sericon-helper/1".into(),
            checksum_kind: "cksum".into(),
            size: None,
            printf: None,
        };
        let info = self.checked_payload(&format!("{} \"$t\" info", path(helper)?), &caps, None)?;
        let info = std::str::from_utf8(&info)?;
        let fields: Vec<_> = info.split_whitespace().collect();
        if fields.len() < 5
            || !matches!(fields[0], "sericon-helper/1" | "sericon-helper/2")
            || fields[2..5] != ["check", "read", "list"]
        {
            bail!("unsupported Sericon helper version/capabilities");
        }
        caps.features = fields[2..].iter().map(|s| (*s).into()).collect();
        caps.encoder = fields[0].into();
        caps.reader = fields[0].into();
        caps.checksum = fields[0].into();
        Ok(caps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn posix_crc_vectors_and_binary() {
        assert_eq!(checksum(b""), (4294967295, 0));
        assert_eq!(checksum(b"123456789"), (930766865, 9));
        assert_eq!(checksum(b"sericon\n"), (1643962224, 8));
    }
    #[test]
    fn shell_paths_are_literal_and_constrained() {
        assert_eq!(path("/a'$(id)").unwrap(), "'/a'\\''$(id)'");
        assert!(path("relative").is_err());
        assert!(path("/a\ncommand").is_err());
    }
    #[test]
    fn short_physical_lines_preserve_shell_quoting() {
        let value = format!("{}' $(echo BAD) \\", "é".repeat(200));
        let command = format!("echo {}\r", quote(&value));
        let wrapped = shell_lines(&command);
        assert!(wrapped.split('\r').all(|line| line.len() < 100));
        let result = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(wrapped.replace('\r', "\n"))
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            format!("{value}\n")
        );
    }
}
