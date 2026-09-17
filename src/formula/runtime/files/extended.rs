//! Explicit helper uploads and bounded system inspection; no shell-tool fallback.
use super::*;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

fn snapshot(source: &str) -> Result<(File, u64, u32, String)> {
    if !std::path::Path::new(source).is_absolute() {
        bail!("upload source must be an absolute host path");
    }
    let mut input = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)?;
    let before = input.metadata()?;
    if !before.is_file() || before.len() > MAX_FILE {
        bail!("upload source must be a regular file at most 64 MiB (no symlinks)");
    }
    let mut copy = tempfile::tempfile()?;
    let mut crc = Cksum::default();
    let mut sha = Sha256::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if crc.len + n as u64 > MAX_FILE {
            bail!("upload source grew beyond 64 MiB");
        }
        copy.write_all(&buffer[..n])?;
        crc.update(&buffer[..n]);
        sha.update(&buffer[..n]);
    }
    let after = input.metadata()?;
    let identity = |m: &fs::Metadata| {
        (
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    if identity(&before) != identity(&after) || crc.len != before.len() {
        bail!("host source changed while preparing upload; no UART writes made");
    }
    copy.seek(SeekFrom::Start(0))?;
    let (crc, size) = crc.finish();
    Ok((copy, size, crc, hex(&sha.finalize())))
}

impl Host {
    fn extended_caps(&mut self, helper: &str, feature: &str) -> Result<Capabilities> {
        if helper.is_empty() {
            bail!("{feature} requires a selected Sericon helper; use Files h or --helper PATH");
        }
        let caps = self.helper_probe(helper)?;
        if !caps.features.iter().any(|f| f == feature) {
            bail!(
                "installed helper lacks {feature}; explicitly install a current helper and select its new path"
            );
        }
        Ok(caps)
    }
    pub(crate) fn linux_upload(
        &mut self,
        source: &str,
        remote: &str,
        helper: &str,
        executable: bool,
    ) -> Result<Value> {
        self.run.check()?;
        if !self.run.definition.interactive {
            bail!("upload requires interactive=true before accessing a host source file");
        }
        // Prepare a stable, private, unnamed copy before any UART interaction.
        path(remote)?;
        path(helper)?;
        let (mut source_file, size, crc, sha) = snapshot(source)?;
        let (parent, leaf) = remote.rsplit_once('/').context("invalid DUT path")?;
        if matches!(leaf, "" | "." | "..") {
            bail!("upload destination needs a filename");
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let stage = format!("{parent}/.sericon-upload-{token}.partial");
        path(&stage)?;
        self.claim()?;
        let result = (|| {
            let caps = self.extended_caps(helper, "upload")?;
            let h = path(helper)?;
            self.run.emit(json!({"operation":"upload","source":source,"destination":remote,"partial_path":stage,
                "bytes":size,"sha256":sha,"executable":executable,"message":"New destination only; partial file retained on failure"}))?;
            self.run.data.lock().unwrap().progress = json!({"stage":"preparing upload","path":remote,"partial_path":stage,"bytes":0,"total":size});
            let ready = self.checked_payload(
                &format!(
                    "{h} \"$t\" upload-begin {} {} {size}",
                    path(remote)?,
                    quote(&token)
                ),
                &caps,
                Some(5),
            )?;
            if ready != b"ready" {
                bail!("invalid upload initialization response");
            }
            let started = Instant::now();
            let mut offset = 0u64;
            let mut bytes = [0u8; CHUNK as usize];
            while offset < size {
                self.run.check()?;
                let count = (size - offset).min(CHUNK) as usize;
                source_file.read_exact(&mut bytes[..count])?;
                let data = &bytes[..count];
                let command = format!(
                    "{h} \"$t\" upload-write {} {offset} {} {}",
                    path(&stage)?,
                    quote(&hex(data)),
                    checksum(data).0
                );
                let echoed = self.checked_payload(&command, &caps, Some(count as u64))?;
                if echoed != data {
                    bail!("upload readback differs from source; partial file retained");
                }
                offset += count as u64;
                let speed = offset as f64 / started.elapsed().as_secs_f64().max(0.001);
                self.run.data.lock().unwrap().progress = json!({"stage":"uploading","path":remote,"partial_path":stage,"bytes":offset,"total":size,
                    "bytes_per_second":speed,"eta_seconds":(size-offset) as f64/speed,"retry":0});
            }
            self.run.data.lock().unwrap().progress["stage"] =
                json!("verifying upload before publication");
            let done = self.checked_payload_timeout(
                &format!(
                    "{h} \"$t\" upload-commit {} {} {size} {crc} {}",
                    path(&stage)?,
                    path(remote)?,
                    if executable { "700" } else { "600" }
                ),
                &caps,
                Some(9),
                Duration::from_secs(120),
            )?;
            if done != b"committed" {
                bail!("invalid upload completion response; check destination and partial path");
            }
            self.run.data.lock().unwrap().progress =
                json!({"stage":"upload verified","path":remote,"bytes":size,"total":size});
            Ok(
                json!({"operation":"upload","source":source,"destination":remote,"bytes":size,"sha256":sha,"verified":true,
                "checksum_kind":"cksum","checksum":crc,"mode":if executable {"0700"} else {"0600"},"executed":false}),
            )
        })();
        if !self.partial {
            self.release()?;
        }
        result
    }

    pub(crate) fn linux_inspect(&mut self, helper: &str, collect: bool) -> Result<Value> {
        path(helper)?;
        if collect && self.run.output_dir.is_none() {
            bail!("overview collection requires an explicit host output directory");
        }
        self.claim()?;
        let result = self.inspect_device(helper, collect);
        if !self.partial {
            self.release()?;
        }
        result
    }
    fn inspect_device(&mut self, helper: &str, collect: bool) -> Result<Value> {
        let caps = self.extended_caps(helper, "inspect")?;
        let started = chrono::Utc::now().to_rfc3339();
        let mut records = Vec::new();
        let mut unavailable = Vec::new();
        for section in [
            "identity", "cpu", "memory", "uptime", "mounts", "storage", "flash",
        ] {
            self.run.data.lock().unwrap().progress =
                json!({"stage":"inspecting device","section":section});
            let raw = self.checked_payload(
                &format!("{} \"$t\" inspect {section}", path(helper)?),
                &caps,
                None,
            )?;
            let separator = raw
                .iter()
                .position(|b| *b == 0)
                .context("invalid inspection response")?;
            let (status, data) = (&raw[..separator], &raw[separator + 1..]);
            let status = std::str::from_utf8(status)?;
            if !matches!(status, "ok" | "unavailable" | "truncated") || data.len() > 16_384 {
                bail!("invalid or oversized inspection section");
            }
            if status != "ok" {
                unavailable.push(section);
                self.run
                    .warn(&format!("Device overview section {section}: {status}"));
            }
            let text = String::from_utf8_lossy(data);
            let values = match section {
                "identity" => parse_fields(data, 2)?,
                "storage" => parse_fields(data, 5)?,
                _ => Value::Null,
            };
            let preview: String = text.chars().take(2048).collect();
            let record = json!({"section":section,"status":status,"observed_at":chrono::Utc::now().to_rfc3339(),
                "values":values,"preview":preview,"preview_truncated":text.chars().count()>2048,
                "bytes":data.len(),"sha256":hex(&Sha256::digest(data)),"raw_base64":STANDARD.encode(data)});
            let mut shown = record.clone();
            shown.as_object_mut().unwrap().remove("raw_base64");
            self.run.emit(shown)?;
            records.push(record);
        }
        let ended = chrono::Utc::now().to_rfc3339();
        let mut summary = json!({"operation":if collect {"collect-overview"} else {"inspect"},"sections":records.len(),
            "started_at":started,"finished_at":ended,"incomplete_sections":unavailable,"atomic_snapshot":false,"helper":helper});
        if collect {
            let overview = json!({"schema":"sericon-device-overview/1","session":self.run.session,"summary":summary,"sections":records});
            let bytes = serde_json::to_vec_pretty(&overview)?;
            self.artifact_open("device-overview.json")?;
            self.artifact_write("device-overview.json", bytes.clone())?;
            self.artifact_close("device-overview.json")?;
            let manifest = json!({"schema":"sericon-collection-manifest/1","session":self.run.session,"started_at":started,"finished_at":ended,
                "files":[{"name":"device-overview.json","bytes":bytes.len(),"sha256":hex(&Sha256::digest(&bytes))}],
                "incomplete_sections":unavailable,"atomic_snapshot":false});
            self.artifact_open("manifest.json")?;
            self.artifact_write("manifest.json", serde_json::to_vec_pretty(&manifest)?)?;
            self.artifact_close("manifest.json")?;
            summary["destination"] = json!(self.artifacts["device-overview.json"].path);
        }
        self.run.data.lock().unwrap().progress = json!({"stage":if collect {"overview saved"} else {"inspection complete"},"sections":records.len()});
        Ok(summary)
    }
}

fn parse_fields(bytes: &[u8], width: usize) -> Result<Value> {
    if bytes.is_empty() {
        return Ok(json!([]));
    }
    let parts: Vec<_> = bytes.split(|b| *b == 0).collect();
    if parts.last() != Some(&b"".as_slice()) || (parts.len() - 1) % width != 0 {
        bail!("invalid inspection fields");
    }
    Ok(json!(
        parts[..parts.len() - 1]
            .chunks(width)
            .map(|row| row
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    ))
}
