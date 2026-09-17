use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Instant,
};

const MEMORY_BYTES: usize = 4 * 1024 * 1024;
const INDEX_STRIDE: u64 = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub time: String,
    pub elapsed_ms: u64,
    pub kind: String,
    pub baud: u32,
    pub actor: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    pub events: Vec<Event>,
    pub next_cursor: u64,
    pub latest_cursor: u64,
    pub oldest_available: u64,
    pub history_gap: bool,
    pub has_more: bool,
}
pub struct Journal {
    pub directory: Option<PathBuf>,
    file: Option<File>,
    transcript: Option<File>,
    offsets: Vec<u64>,
    offset: u64,
    ring: VecDeque<Event>,
    memory_size: usize,
    pub last_seq: u64,
    started: Instant,
}
pub fn private_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}
pub fn printable(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            b'\n' => s.push('\n'),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push('\t'),
            32..=126 => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    s
}
impl Journal {
    pub fn new(directory: Option<&Path>, id: &str) -> Result<Self> {
        let mut j = Self {
            directory: None,
            file: None,
            transcript: None,
            offsets: Vec::new(),
            offset: 0,
            ring: VecDeque::new(),
            memory_size: 0,
            last_seq: 0,
            started: Instant::now(),
        };
        if let Some(parent) = directory {
            fs::create_dir_all(parent)
                .with_context(|| format!("create log destination {}", parent.display()))?;
            let dir = parent.join(format!(
                "sericon-{}-{id}",
                Utc::now().format("%Y%m%dT%H%M%SZ")
            ));
            fs::DirBuilder::new().mode(0o700).create(&dir)?;
            j.file = Some(private_file(&dir.join("events.jsonl"))?);
            j.transcript = Some(private_file(&dir.join("transcript.txt"))?);
            j.directory = Some(dir);
        }
        Ok(j)
    }
    pub fn record(
        &mut self,
        kind: &str,
        baud: u32,
        actor: &str,
        message: &str,
        data: Option<&[u8]>,
    ) -> Result<u64> {
        let event = Event {
            seq: self.last_seq + 1,
            time: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            kind: kind.into(),
            baud,
            actor: actor.into(),
            message: message.into(),
            data_base64: data.map(|d| STANDARD.encode(d)),
            text: data.map(|d| String::from_utf8_lossy(d).into_owned()),
        };
        if let Some(file) = &mut self.file {
            let mut encoded = serde_json::to_vec(&event)?;
            encoded.push(b'\n');
            if (event.seq - 1).is_multiple_of(INDEX_STRIDE) {
                self.offsets.push(self.offset);
            }
            file.write_all(&encoded)
                .context("session journal write failed")?;
            self.offset += encoded.len() as u64;
            if let Some(transcript) = &mut self.transcript {
                let content = data.map(printable).unwrap_or_else(|| message.into());
                writeln!(
                    transcript,
                    "[{} #{} {} {} @{}] {}",
                    event.time, event.seq, kind, actor, baud, content
                )
                .context("transcript write failed")?;
            }
        }
        self.last_seq = event.seq;
        self.memory_size += size(&event);
        self.ring.push_back(event);
        while self.memory_size > MEMORY_BYTES && self.ring.len() > 1 {
            self.memory_size -= size(&self.ring.pop_front().unwrap());
        }
        Ok(self.last_seq)
    }
    pub fn fail_logging(&mut self) {
        self.file = None;
        self.transcript = None;
    }
    pub fn sync(&mut self) -> Result<()> {
        if let Some(f) = &mut self.file {
            f.sync_data()?;
        }
        if let Some(f) = &mut self.transcript {
            f.sync_data()?;
        }
        Ok(())
    }
    pub fn page(&self, after: u64, limit: usize) -> Result<Page> {
        if after > self.last_seq {
            bail!("cursor is ahead of this session (latest {})", self.last_seq);
        }
        let limit = limit.clamp(1, 128);
        let memory_first = self.ring.front().map_or(1, |e| e.seq);
        let disk = self.directory.as_ref().filter(|_| self.file.is_some());
        let oldest = if disk.is_some() { 1 } else { memory_first };
        let mut events = Vec::new();
        let mut byte_count = 0;
        if after.saturating_add(1) < memory_first
            && let Some(dir) = disk
        {
            let mut f = File::open(dir.join("events.jsonl"))?;
            let slot = (after / INDEX_STRIDE) as usize;
            f.seek(SeekFrom::Start(*self.offsets.get(slot).unwrap_or(&0)))?;
            for line in BufReader::new(f).lines() {
                let event: Event = serde_json::from_str(&line?)?;
                if event.seq <= after {
                    continue;
                }
                if event.seq > self.last_seq {
                    break;
                }
                byte_count += size(&event);
                events.push(event);
                if events.len() >= limit || byte_count >= 128 * 1024 {
                    break;
                }
            }
        } else {
            for e in &self.ring {
                if e.seq > after {
                    byte_count += size(e);
                    events.push(e.clone());
                    if events.len() >= limit || byte_count >= 128 * 1024 {
                        break;
                    }
                }
            }
        }
        let next = events.last().map_or(after, |e| e.seq);
        Ok(Page {
            events,
            next_cursor: next,
            latest_cursor: self.last_seq,
            oldest_available: oldest,
            history_gap: after.saturating_add(1) < oldest,
            has_more: next < self.last_seq,
        })
    }
}
fn size(e: &Event) -> usize {
    256 + e.message.len()
        + e.text.as_ref().map_or(0, String::len)
        + e.data_base64.as_ref().map_or(0, String::len)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_survives_memory_eviction_and_preserves_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = Journal::new(Some(dir.path()), "test").unwrap();
        for _ in 0..700 {
            j.record("rx", 115200, "device", "", Some(&[0xff; 4096]))
                .unwrap();
        }
        assert!(j.ring.front().unwrap().seq > 1);
        let page = j.page(0, 2).unwrap();
        assert!(!page.history_gap);
        assert_eq!(page.events[0].seq, 1);
        assert_eq!(
            STANDARD
                .decode(page.events[0].data_base64.as_ref().unwrap())
                .unwrap(),
            vec![0xff; 4096]
        );
        assert_eq!(j.page(511, 2).unwrap().events[0].seq, 512);
    }
    #[test]
    fn no_log_is_memory_only_and_reports_eviction() {
        let mut j = Journal::new(None, "test").unwrap();
        for _ in 0..700 {
            j.record("rx", 9600, "device", "", Some(&[b'A'; 4096]))
                .unwrap();
        }
        assert!(j.directory.is_none());
        assert!(j.page(0, 1).unwrap().history_gap);
        assert!(j.page(900, 1).is_err());
    }

    #[test]
    fn failed_write_does_not_claim_an_event_was_logged() {
        let mut j = Journal::new(None, "test").unwrap();
        j.file = Some(OpenOptions::new().write(true).open("/dev/full").unwrap());
        assert!(j.record("tx", 115200, "human", "", Some(b"test")).is_err());
        assert_eq!(j.last_seq, 0);
        assert!(j.ring.is_empty());
    }
}
