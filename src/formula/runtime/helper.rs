//! Opt-in shell bootstrap for embedded static helpers. No external host tools.
use super::files::{hex, path, quote};
use super::*;

include!(concat!(env!("OUT_DIR"), "/helper-bundle.rs"));

pub(crate) fn inventory() -> Value {
    json!(
        BUNDLED
            .iter()
            .map(|(arch, bytes)| json!({
                "arch":arch,"bytes":bytes.len(),"sha256":hex(&Sha256::digest(bytes)),
                "protocol":2,
                "abi":match *arch {
                    "mipsel" => "ELF32 little-endian, o32, MIPS32r2, soft float",
                    "mips" => "ELF32 big-endian, o32, MIPS32r2, soft float",
                    "arm" => "ELF32 little-endian, ARMv6KZ EABI hard float (VFPv2)",
                    "aarch64" => "ELF64 little-endian, AArch64 LP64",
                    _ => "ELF64 little-endian, x86-64"
                }
            }))
            .collect::<Vec<_>>()
    )
}
fn encoded(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\0{b:03o}")).collect()
}
fn wire(bytes: &[u8], crlf: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len());
    for &b in bytes {
        if crlf && b == b'\n' {
            result.push(b'\r');
        }
        result.push(b);
    }
    result
}
impl Host {
    pub(super) fn helper_install(&mut self, arch: &str, directory: &str) -> Result<Value> {
        // Resolve/validate locally before claiming input or creating anything.
        let bytes = BUNDLED
            .iter()
            .find(|(a, _)| *a == arch)
            .with_context(|| format!("no bundled helper for {arch}; use 'sericon files helpers'"))?
            .1;
        path(directory)?;
        let target = format!(
            "{}/sericon-{}",
            directory.trim_end_matches('/'),
            uuid::Uuid::new_v4().simple()
        );
        let helper = format!("{target}/helper");
        path(&helper)?;
        self.claim()?;
        let result = self.upload_helper(bytes, arch, &target, &helper);
        if !self.partial {
            self.release()?;
        }
        result
    }
    fn upload_helper(
        &mut self,
        bytes: &[u8],
        arch: &str,
        target: &str,
        helper: &str,
    ) -> Result<Value> {
        self.run.data.lock().unwrap().progress = json!({"stage":"probing","message":"Checking binary shell output; no DUT files created yet"});
        let sample: Vec<u8> = (0..=255).collect();
        let escaped = encoded(&sample);
        let mut encoder = None;
        for command in [
            "printf '%b'",
            "busybox printf '%b'",
            "echo",
            "echo -ne",
            "busybox echo -ne",
        ] {
            for _ in 0..3 {
                let body = format!("{command} '{}\\c' 2>/dev/null", escaped);
                match self.exchange(&body) {
                    Ok((received, _)) => {
                        if let Some(crlf) = [false, true]
                            .into_iter()
                            .find(|c| received == wire(&sample, *c))
                        {
                            encoder = Some((command, crlf));
                            break;
                        }
                    }
                    Err(e) if self.partial => return Err(e),
                    Err(_) => self.run.check()?,
                }
            }
            if encoder.is_some() {
                break;
            }
        }
        let (encoder,crlf)=encoder.context("shell cannot emit all 256 byte values exactly; helper upload unavailable, no DUT files created")?;
        let d = quote(target);
        self.exchange(&format!("umask 077; mkdir {d}"))?;
        self.run.emit(json!({"helper_directory":target,"helper":helper,"arch":arch,"bytes":bytes.len(),"message":"Private DUT upload directory; retained on failure, remove when no longer needed"}))?;
        // Independently named parts make every retry a replacement, never a
        // duplicate append. A lost command boundary stops rather than guessing.
        for (index, chunk) in bytes.chunks(256).enumerate() {
            self.run.data.lock().unwrap().progress = json!({"stage":"uploading helper","bytes":index*256,"total":bytes.len(),"helper":helper});
            let part = format!("{target}/p{index:04}");
            let body = format!(
                "umask 077; {encoder} '{}\\c' > {} && cat {}",
                encoded(chunk),
                quote(&part),
                quote(&part)
            );
            self.verify_upload(&body, chunk, crlf, Duration::from_secs(10))?;
        }
        self.run.data.lock().unwrap().progress = json!({"stage":"verifying helper before execution","bytes":bytes.len(),"total":bytes.len(),"helper":helper});
        let assemble =
            format!("umask 077; d={d}; cat \"$d\"/p* > \"$d/helper\" && cat \"$d/helper\"");
        self.verify_upload(&assemble, bytes, crlf, Duration::from_secs(120))?;
        // The complete ELF must match BEFORE making it executable/launching.
        self.exchange(&format!("chmod 700 {} && rm {d}/p*", quote(helper)))?;
        let probe = self.linux_with_helper("probe", "", "", helper)?;
        self.run.data.lock().unwrap().progress =
            json!({"stage":"helper ready","helper":helper,"bytes":bytes.len(),"total":bytes.len()});
        Ok(
            json!({"helper":helper,"directory":target,"arch":arch,"bytes":bytes.len(),
            "sha256":hex(&Sha256::digest(bytes)),"verified_before_execution":true,
            "probe":probe,"auto_start":false,"storage_persistence":"depends on target filesystem"}),
        )
    }
    fn verify_upload(
        &mut self,
        body: &str,
        expected: &[u8],
        crlf: bool,
        timeout: Duration,
    ) -> Result<()> {
        let expected = wire(expected, crlf);
        for attempt in 1..=3 {
            match self.exchange_timeout(body, timeout) {
                Ok((got, _)) if got == expected => return Ok(()),
                Err(e) if self.partial => return Err(e),
                _ => {
                    self.run.check()?;
                    self.run.warn("Helper readback failed validation; retrying an idempotent write. No unverified helper is executed.");
                    self.run.data.lock().unwrap().progress["retry"] = json!(attempt);
                }
            }
        }
        bail!(
            "helper upload/readback failed after 3 attempts; private DUT files retained, helper not executed"
        )
    }
}
