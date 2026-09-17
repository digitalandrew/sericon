use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    time::Duration,
};

pub const FRAME_LIMIT: usize = 4 * 1024 * 1024;
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    FormulaStart {
        formula: crate::formula::Definition,
        #[serde(default = "empty_object")]
        params: Value,
        initiator: String,
        #[serde(default = "formula_timeout")]
        timeout_ms: u64,
        #[serde(default)]
        output_dir: Option<PathBuf>,
    },
    FormulaRuns,
    HelperInventory,
    FormulaRead {
        run: String,
        #[serde(default)]
        after: usize,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    FormulaCancel {
        run: String,
    },
    Status,
    Read {
        #[serde(default)]
        after: u64,
        #[serde(default = "default_limit")]
        limit: usize,
        #[serde(default)]
        wait_ms: u64,
    },
    Send {
        data_base64: String,
        actor: String,
        client_id: String,
        #[serde(default = "yes")]
        release: bool,
    },
    Claim {
        actor: String,
        client_id: String,
        #[serde(default)]
        takeover: bool,
    },
    Release {
        client_id: String,
    },
    Baud {
        rate: u32,
    },
    Rescan,
    Stop,
}
fn empty_object() -> Value {
    json!({})
}
fn formula_timeout() -> u64 {
    300_000
}
fn default_limit() -> usize {
    32
}
fn yes() -> bool {
    true
}

fn standard_runtime_dir(base: &Path, uid: u32) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(base).ok()?;
    (meta.is_dir() && meta.uid() == uid && meta.permissions().mode() & 0o077 == 0)
        .then(|| base.join("sericon"))
}

pub fn runtime_dir() -> Result<PathBuf> {
    // Never place the control socket in a shared project or capture directory.
    let uid = unsafe { libc::geteuid() };
    let path = if let Some(p) = env::var_os("SERICON_RUNTIME_DIR") {
        PathBuf::from(p)
    } else if let Some(p) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(p).join("sericon")
    } else {
        // MCP clients may omit XDG_RUNTIME_DIR from their child environment.
        // Recover the conventional Linux login directory so those clients can
        // still join a session started from the user's terminal.
        standard_runtime_dir(&PathBuf::from(format!("/run/user/{uid}")), uid)
            .unwrap_or_else(|| env::temp_dir().join(format!("sericon-{uid}")))
    };
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e).context("create Sericon runtime directory"),
    }
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        bail!(
            "runtime directory must be owned by you, not a symlink, and mode 0700: {}",
            path.display()
        );
    }
    Ok(path)
}
pub fn socket_path(id: &str) -> Result<PathBuf> {
    if id.len() != 12 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid session ID; use 'sericon sessions'");
    }
    Ok(runtime_dir()?.join(format!("{id}.sock")))
}
pub fn frame<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut line = String::new();
    let n = reader.take((FRAME_LIMIT + 1) as u64).read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if n > FRAME_LIMIT || !line.ends_with('\n') {
        bail!("invalid or oversized protocol frame");
    }
    Ok(Some(line))
}
pub fn call(id: &str, request: &Request) -> Result<Value> {
    call_path(&socket_path(id)?, request)
}
fn call_path(path: &Path, request: &Request) -> Result<Value> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("session unavailable: {}", path.display()))?;
    let timeout = match request {
        Request::Read { wait_ms, .. } => Duration::from_millis((*wait_ms).min(30_000) + 3000),
        Request::Status => Duration::from_secs(2),
        _ => Duration::from_secs(6),
    };
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let response =
        frame(&mut BufReader::new(stream))?.context("session closed without replying")?;
    let value: Value = serde_json::from_str(&response)?;
    if let Some(error) = value.get("error") {
        bail!("{}", error.as_str().unwrap_or("session request failed"));
    }
    Ok(value["result"].clone())
}
pub fn sessions() -> Result<Vec<Value>> {
    let mut values = Vec::new();
    for entry in fs::read_dir(runtime_dir()?)? {
        let path = entry?.path();
        if path.extension().is_some_and(|s| s == "sock")
            && let Ok(value) = call_path(&path, &Request::Status)
        {
            values.push(value);
        }
    }
    values.sort_by_key(|v| v["started"].as_str().unwrap_or_default().to_string());
    Ok(values)
}
pub fn resolve(id: Option<&str>) -> Result<String> {
    if let Some(id) = id {
        socket_path(id)?;
        return Ok(id.into());
    }
    let sessions = sessions()?;
    if sessions.len() != 1 {
        bail!(
            "expected one active session, found {}; specify a session ID from 'sericon sessions'",
            sessions.len()
        );
    }
    Ok(sessions[0]["id"]
        .as_str()
        .context("invalid session status")?
        .into())
}
pub fn reply(stream: &mut UnixStream, result: Result<Value>) -> Result<()> {
    let value = match result {
        Ok(value) => json!({"result":value}),
        Err(e) => json!({"error":format!("{e:#}")}),
    };
    serde_json::to_writer(&mut *stream, &value)?;
    stream.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn recover_login_runtime_only_from_a_private_owned_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("login-runtime");
        let uid = unsafe { libc::geteuid() };
        assert_eq!(standard_runtime_dir(&base, uid), None);
        fs::DirBuilder::new().mode(0o700).create(&base).unwrap();
        assert_eq!(standard_runtime_dir(&base, uid), Some(base.join("sericon")));
        assert_eq!(standard_runtime_dir(&base, uid.wrapping_add(1)), None);
        let link = tmp.path().join("linked-runtime");
        symlink(&base, &link).unwrap();
        assert_eq!(standard_runtime_dir(&link, uid), None);
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(standard_runtime_dir(&base, uid), None);
    }
}
