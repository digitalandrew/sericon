use super::{Definition, runtime};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const RESULT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Serialize)]
pub(crate) struct RunData {
    pub id: String,
    pub formula: String,
    pub revision: String,
    pub initiator: String,
    pub state: String,
    pub started: String,
    pub finished: Option<String>,
    pub error: Option<String>,
    pub progress: Value,
    pub warnings: Vec<String>,
    pub results: Vec<Value>,
    pub artifacts: Vec<Value>,
    pub writer_retained: bool,
    #[serde(skip)]
    result_bytes: usize,
}
pub(crate) struct Run {
    pub data: Mutex<RunData>,
    pub cancel: AtomicBool,
    pub deadline: Instant,
    pub definition: Definition,
    pub params: Value,
    pub session: String,
    pub client: String,
    pub actor: String,
    pub output_dir: Option<PathBuf>,
    pub history_end: u64,
    archive: Option<PathBuf>,
}
impl Run {
    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            bail!("formula cancelled");
        }
        if Instant::now() >= self.deadline {
            bail!("formula deadline exceeded");
        }
        Ok(())
    }
    pub fn summary(&self) -> Value {
        let d = self.data.lock().unwrap();
        json!({"id":d.id,"session":self.session,"formula":d.formula,"revision":d.revision,
            "initiator":d.initiator,"interactive":self.definition.interactive,"state":d.state,
            "started":d.started,"finished":d.finished,"error":d.error,"progress":d.progress,
            "warnings":d.warnings,"result_count":d.results.len(),"artifacts":d.artifacts,
            "writer_retained":d.writer_retained,"archive":self.archive,"history_end":self.history_end})
    }
    pub fn emit(&self, value: Value) -> Result<()> {
        self.check()?;
        let bytes = serde_json::to_vec(&value)?.len();
        let mut d = self.data.lock().unwrap();
        if bytes > 32768 || d.result_bytes + bytes > RESULT_BYTES || d.results.len() >= 5000 {
            bail!(
                "formula result limit reached (32 KiB per result, 2 MiB / 5000 results per run); partial results retained"
            );
        }
        d.result_bytes += bytes;
        d.results.push(value);
        Ok(())
    }
    pub fn warn(&self, text: &str) {
        let mut d = self.data.lock().unwrap();
        if d.warnings.len() < 32 && !d.warnings.iter().any(|w| w == text) {
            d.warnings.push(text.into());
        }
    }
    fn persist(&self) -> Result<()> {
        if let Some(path) = &self.archive {
            let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            let d = self.data.lock().unwrap();
            serde_json::to_writer(
                &mut file,
                &json!({"run":*d,"session":self.session,
                "definition":self.definition,"parameters":self.params,"history_end":self.history_end}),
            )?;
            file.write_all(b"\n")?;
            file.as_file().sync_all()?;
            file.persist(path).map_err(|e| e.error)?;
        }
        Ok(())
    }
}

pub struct Manager {
    runs: Mutex<VecDeque<Arc<Run>>>,
    stopped: AtomicBool,
    session: String,
    directory: Option<PathBuf>,
}
impl Manager {
    pub fn new(session: String, directory: Option<PathBuf>) -> Self {
        Self {
            runs: Mutex::new(VecDeque::new()),
            stopped: AtomicBool::new(false),
            session,
            directory,
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        definition: Definition,
        params: Value,
        initiator: String,
        timeout_ms: u64,
        output_dir: Option<PathBuf>,
        history_end: u64,
    ) -> Result<Value> {
        if !(100..=86_400_000).contains(&timeout_ms) {
            bail!("timeout_ms must be 100..86400000");
        }
        if initiator.is_empty()
            || initiator.len() > 64
            || !initiator
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            bail!("invalid formula initiator");
        }
        // Resolve file references at the initiating client, never in the broker.
        if definition.script.is_some() {
            bail!("formula_start requires resolved source");
        }
        let definition = definition.resolve()?;
        let params = definition.arguments(&params)?;
        runtime::compile(&definition)?;
        if let Some(path) = &output_dir
            && !path.is_absolute()
        {
            bail!("output_dir must be absolute");
        }
        let mut runs = self.runs.lock().unwrap();
        if self.stopped.load(Ordering::Relaxed) {
            bail!("session is stopping");
        }
        if runs
            .iter()
            .filter(|r| r.data.lock().unwrap().state == "running")
            .count()
            >= 4
        {
            bail!("four formulas are already running");
        }
        if runs.len() >= 16 {
            let index = runs
                .iter()
                .position(|r| r.data.lock().unwrap().state != "running")
                .context("run limit reached")?;
            runs.remove(index);
        }
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
        let run = Arc::new(Run {
            data: Mutex::new(RunData {
                id: id.clone(),
                formula: definition.name.clone(),
                revision: definition.revision(),
                initiator,
                state: "running".into(),
                started: chrono::Utc::now().to_rfc3339(),
                finished: None,
                error: None,
                progress: Value::Null,
                warnings: Vec::new(),
                results: Vec::new(),
                artifacts: Vec::new(),
                writer_retained: false,
                result_bytes: 0,
            }),
            cancel: AtomicBool::new(false),
            deadline: Instant::now() + Duration::from_millis(timeout_ms),
            definition,
            params,
            session: self.session.clone(),
            client: format!("formula-{id}"),
            actor: format!("formula:{id}"),
            output_dir,
            history_end,
            archive: self
                .directory
                .as_ref()
                .map(|p| p.join(format!("formula-{id}.json"))),
        });
        run.persist().context("create formula run record")?;
        let reply = run.summary();
        runs.push_back(run.clone());
        let result = thread::Builder::new()
            .name(format!("formula-{id}"))
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    runtime::execute(run.clone())
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("formula runtime panicked")));
                {
                    let mut d = run.data.lock().unwrap();
                    let cancelled = run.cancel.load(Ordering::Relaxed);
                    d.state = if cancelled {
                        "cancelled"
                    } else if result.is_err() {
                        "failed"
                    } else {
                        "completed"
                    }
                    .into();
                    if let Err(e) = result
                        && d.error.is_none()
                    {
                        d.error = Some(format!("{e:#}"));
                    }
                    d.finished = Some(chrono::Utc::now().to_rfc3339());
                }
                if let Err(e) = run.persist() {
                    let mut d = run.data.lock().unwrap();
                    d.state = "failed".into();
                    d.error = Some(format!("save formula run record: {e:#}"));
                }
            });
        if let Err(e) = result {
            runs.retain(|r| r.data.lock().unwrap().id != id);
            bail!("start formula worker: {e}");
        }
        Ok(reply)
    }
    fn get(&self, id: &str) -> Result<Arc<Run>> {
        self.runs
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.data.lock().unwrap().id == id)
            .cloned()
            .context("unknown run; only the latest 16 runs remain in session memory")
    }
    pub fn list(&self) -> Value {
        json!(
            self.runs
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.summary())
                .collect::<Vec<_>>()
        )
    }
    pub fn read(&self, id: &str, after: usize, limit: usize) -> Result<Value> {
        if !(1..=128).contains(&limit) {
            bail!("limit must be 1..128");
        }
        let run = self.get(id)?;
        let summary = run.summary();
        let d = run.data.lock().unwrap();
        if after > d.results.len() {
            bail!("result cursor exceeds available results");
        }
        // Bound response bytes even when individual results approach their limit.
        let mut bytes = 0;
        let results: Vec<_> = d
            .results
            .iter()
            .skip(after)
            .take(limit)
            .take_while(|v| {
                bytes += serde_json::to_vec(v).map_or(32768, |s| s.len());
                bytes <= 512 * 1024
            })
            .cloned()
            .collect();
        let next = after + results.len();
        Ok(
            json!({"run":summary,"results":results,"next_cursor":next,"has_more":next < d.results.len()}),
        )
    }
    pub fn cancel(&self, id: &str, reason: &str) -> Result<Value> {
        let run = self.get(id)?;
        let mut d = run.data.lock().unwrap();
        if d.state == "running" {
            run.cancel.store(true, Ordering::Relaxed);
            d.error = Some(reason.into());
        }
        drop(d);
        Ok(run.summary())
    }
    pub fn revoke_writer(&self, client: &str) {
        if let Some(id) = client.strip_prefix("formula-") {
            let _ = self.cancel(id, "human takeover revoked formula input");
        }
    }
    pub fn authorize(&self, client: &str) -> Result<()> {
        if let Some(id) = client.strip_prefix("formula-") {
            let run = self.get(id)?;
            run.check()?;
            if !run.definition.interactive || run.data.lock().unwrap().state != "running" {
                bail!("formula is not authorized to write");
            }
        }
        Ok(())
    }
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        for run in self.runs.lock().unwrap().iter() {
            run.cancel.store(true, Ordering::Relaxed);
        }
    }
}
