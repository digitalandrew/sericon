use crate::{
    config::{Config, validate_rate},
    detect::{Decision, Detector},
    devices::Device,
    ipc::{self, Request},
    journal::Journal,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};
use std::{
    fs,
    io::{BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Start {
    pub config: Config,
    pub device: Device,
    pub launch_dir: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Writer {
    pub client_id: String,
    pub actor: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub id: String,
    pub port: String,
    pub device: Device,
    pub started: String,
    pub launch_dir: PathBuf,
    pub baud: u32,
    pub detection: String,
    pub confidence: Option<f64>,
    pub connected: bool,
    pub log_directory: Option<PathBuf>,
    pub writer: Option<Writer>,
    pub latest_cursor: u64,
    pub error: Option<String>,
}
struct State {
    status: Status,
    journal: Journal,
}
type Shared = Arc<(Mutex<State>, Condvar)>;
struct Job {
    request: Request,
    answer: mpsc::SyncSender<Result<Value>>,
    expires: Instant,
}
struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn launch(start: &Start) -> Result<Status> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("__serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // The broker survives terminal detachment and shell hangup.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("start session process")?;
    let mut input = child.stdin.take().unwrap();
    serde_json::to_writer(&mut input, start)?;
    input.write_all(b"\n")?;
    drop(input);
    let output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = ipc::frame(&mut BufReader::new(output));
        let _ = tx.send(result);
    });
    let response = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(Some(response))) => response,
        other => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("session did not start: {other:?}");
        }
    };
    let value: Value = serde_json::from_str(&response)?;
    if let Some(error) = value.get("error") {
        let _ = child.wait();
        bail!("{}", error.as_str().unwrap_or("session startup failed"));
    }
    serde_json::from_value(value["result"].clone()).context("invalid session startup response")
}
fn record(
    shared: &Shared,
    kind: &str,
    actor: &str,
    message: &str,
    data: Option<&[u8]>,
) -> Result<()> {
    let mut state = shared.0.lock().unwrap();
    let baud = state.status.baud;
    if let Err(error) = state.journal.record(kind, baud, actor, message, data) {
        state.status.error = Some(format!("logging failed: {error:#}"));
        return Err(error);
    }
    state.status.latest_cursor = state.journal.last_seq;
    shared.1.notify_all();
    Ok(())
}
fn with_status(shared: &Shared, f: impl FnOnce(&mut Status)) {
    f(&mut shared.0.lock().unwrap().status);
}
fn identity(actor: &str, client: &str) -> Result<()> {
    for s in [actor, client] {
        if s.is_empty()
            || s.len() > 64
            || !s
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
        {
            bail!("actor/client identity must be 1..64 letters, digits, '.', '_', ':', or '-'");
        }
    }
    Ok(())
}
fn available(shared: &Shared, client: &str) -> Result<()> {
    let state = shared.0.lock().unwrap();
    if let Some(writer) = &state.status.writer
        && writer.client_id != client
    {
        bail!(
            "input busy: {} holds the writer; wait for release or use the terminal takeover shortcut",
            writer.actor
        );
    }
    Ok(())
}
fn handle(
    job: &Request,
    port: &mut serialport::TTYPort,
    shared: &Shared,
    detector: &mut Detector,
    config: &Config,
    stop: &AtomicBool,
    formulas: &crate::formula::Manager,
) -> Result<Value> {
    match job {
        Request::Send {
            data_base64,
            actor,
            client_id,
            release,
        } => {
            identity(actor, client_id)?;
            formulas.authorize(client_id)?;
            available(shared, client_id)?;
            let data = STANDARD
                .decode(data_base64)
                .context("invalid base64 input")?;
            if data.is_empty() || data.len() > 4096 {
                bail!("send requires 1..4096 bytes");
            }
            with_status(shared, |s| {
                s.writer = Some(Writer {
                    client_id: client_id.clone(),
                    actor: actor.clone(),
                });
                if detector.active {
                    s.detection = "manual".into();
                    s.confidence = None;
                }
            });
            detector.pause();
            // Only the broker writes; record exactly what the OS accepted, including partial writes.
            let mut written = 0;
            let mut failure = None;
            while written < data.len() {
                match port.write(&data[written..]) {
                    Ok(0) => {
                        failure = Some("serial write returned zero".to_string());
                        break;
                    }
                    Ok(n) => {
                        record(shared, "tx", actor, "", Some(&data[written..written + n]))?;
                        written += n;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        failure = Some(e.to_string());
                        break;
                    }
                }
            }
            // Retain ownership on a partial command; the caller must resolve it explicitly.
            if let Some(error) = failure {
                record(
                    shared,
                    "error",
                    actor,
                    &format!("write failed after {written} bytes: {error}"),
                    None,
                )?;
                bail!("write failed after {written} bytes; input ownership retained: {error}");
            }
            if *release {
                with_status(shared, |s| s.writer = None);
            }
            Ok(json!({"written":written,"cursor":shared.0.lock().unwrap().status.latest_cursor}))
        }
        Request::Claim {
            actor,
            client_id,
            takeover,
        } => {
            identity(actor, client_id)?;
            formulas.authorize(client_id)?;
            if !takeover {
                available(shared, client_id)?;
            } else {
                if client_id.starts_with("formula-") {
                    bail!("formulas cannot force input takeover");
                }
                if let Some(writer) = &shared.0.lock().unwrap().status.writer {
                    formulas.revoke_writer(&writer.client_id);
                }
            }
            with_status(shared, |s| {
                s.writer = Some(Writer {
                    client_id: client_id.clone(),
                    actor: actor.clone(),
                })
            });
            record(
                shared,
                "writer",
                actor,
                if *takeover {
                    "input taken over"
                } else {
                    "input reserved"
                },
                None,
            )?;
            Ok(json!({"writer":client_id}))
        }
        Request::Release { client_id } => {
            available(shared, client_id)?;
            with_status(shared, |s| s.writer = None);
            record(shared, "writer", client_id, "input released", None)?;
            Ok(json!({"released":true}))
        }
        Request::Baud { rate } => {
            validate_rate(*rate)?;
            if shared.0.lock().unwrap().status.writer.is_some() {
                bail!("release input before changing baud");
            }
            port.set_baud_rate(*rate)?;
            detector.pause();
            with_status(shared, |s| {
                s.baud = *rate;
                s.detection = "fixed".into();
                s.confidence = None;
            });
            record(
                shared,
                "baud",
                "sericon",
                &format!("fixed baud {rate}"),
                None,
            )?;
            Ok(json!({"baud":rate}))
        }
        Request::Rescan => {
            if shared.0.lock().unwrap().status.writer.is_some() {
                bail!("release input before rescanning");
            }
            let rate = config.serial.baud_rates[0];
            port.set_baud_rate(rate)?;
            detector.rescan(Instant::now());
            with_status(shared, |s| {
                s.baud = rate;
                s.detection = "detecting".into();
                s.confidence = None;
            });
            record(
                shared,
                "baud",
                "sericon",
                &format!("rescanning from {rate}"),
                None,
            )?;
            Ok(json!({"baud":rate}))
        }
        Request::Stop => {
            stop.store(true, Ordering::Relaxed);
            Ok(json!({"stopping":true}))
        }
        _ => bail!("unsupported broker request"),
    }
}
fn client(
    mut stream: UnixStream,
    shared: Shared,
    tx: mpsc::SyncSender<Job>,
    formulas: Arc<crate::formula::Manager>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let line = ipc::frame(&mut BufReader::new(stream.try_clone()?))?.context("empty request")?;
    if line.len() > 256 * 1024 {
        bail!("request too large");
    }
    let result = (|| -> Result<Value> {
        let request: Request = serde_json::from_str(&line)?;
        match request {
            Request::FormulaStart {
                formula,
                params,
                initiator,
                timeout_ms,
                output_dir,
            } => {
                let state = shared.0.lock().unwrap();
                if !state.status.connected {
                    bail!("serial device disconnected");
                }
                let cursor = state.status.latest_cursor;
                drop(state);
                formulas.start(formula, params, initiator, timeout_ms, output_dir, cursor)
            }
            Request::FormulaRuns => Ok(formulas.list()),
            Request::HelperInventory => Ok(crate::formula::helper_inventory()),
            Request::FormulaRead { run, after, limit } => formulas.read(&run, after, limit),
            Request::FormulaCancel { run } => formulas.cancel(&run, "cancelled by client"),
            Request::Status => Ok(serde_json::to_value(&shared.0.lock().unwrap().status)?),
            Request::Read {
                after,
                limit,
                wait_ms,
            } => {
                if limit == 0 || limit > 128 || wait_ms > 30_000 {
                    bail!("limit must be 1..128 and wait_ms <= 30000");
                }
                let state = shared.0.lock().unwrap();
                let (state, _) = shared
                    .1
                    .wait_timeout_while(state, Duration::from_millis(wait_ms), |s| {
                        s.journal.last_seq <= after && s.status.connected
                    })
                    .unwrap();
                Ok(serde_json::to_value(state.journal.page(after, limit)?)?)
            }
            request => {
                if !shared.0.lock().unwrap().status.connected {
                    bail!("serial device disconnected");
                }
                let (answer, rx) = mpsc::sync_channel(1);
                tx.try_send(Job {
                    request,
                    answer,
                    expires: Instant::now() + Duration::from_secs(4),
                })
                .context("session request queue is full or closed")?;
                rx.recv_timeout(Duration::from_secs(5)).context(
                    "session operation timed out; inspect history before retrying a write",
                )?
            }
        }
    })();
    ipc::reply(&mut stream, result)
}

pub fn serve(start: Start) -> Result<()> {
    let config = &start.config;
    config.validate()?;
    let baud = config
        .serial
        .baud
        .fixed()
        .unwrap_or(config.serial.baud_rates[0]);
    let data_bits = match config.serial.data_bits {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    };
    let parity = match config.serial.parity.as_str() {
        "odd" => Parity::Odd,
        "even" => Parity::Even,
        _ => Parity::None,
    };
    let flow = match config.serial.flow_control.as_str() {
        "hardware" => FlowControl::Hardware,
        "software" => FlowControl::Software,
        _ => FlowControl::None,
    };
    let mut port = serialport::new(&start.device.port, baud)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(if config.serial.stop_bits == 2 {
            StopBits::Two
        } else {
            StopBits::One
        })
        .flow_control(flow)
        .timeout(Duration::from_millis(20))
        .dtr_on_open(config.serial.dtr)
        .open_native()
        .with_context(|| {
            format!(
                "open {} exclusively (check permissions and whether another terminal owns it)",
                start.device.port
            )
        })?;
    // PTYs do not implement modem-control ioctls. A real UART must accept requested asserted lines.
    if let Err(e) = port.write_request_to_send(config.serial.rts)
        && config.serial.rts
    {
        return Err(e.into());
    }
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let journal = Journal::new(
        config
            .logging
            .enabled
            .then_some(config.logging.directory.as_path()),
        &id,
    )?;
    let fixed = config.serial.baud.fixed().is_some();
    let status = Status {
        id: id.clone(),
        port: start.device.port.clone(),
        device: start.device,
        started: chrono::Utc::now().to_rfc3339(),
        launch_dir: start.launch_dir,
        baud,
        detection: if fixed { "fixed" } else { "waiting" }.into(),
        confidence: None,
        connected: true,
        log_directory: journal.directory.clone(),
        writer: None,
        latest_cursor: 0,
        error: None,
    };
    let shared: Shared = Arc::new((Mutex::new(State { status, journal }), Condvar::new()));
    let formulas = Arc::new(crate::formula::Manager::new(
        id.clone(),
        shared.0.lock().unwrap().status.log_directory.clone(),
    ));
    let socket = ipc::socket_path(&id)?;
    let listener = UnixListener::bind(&socket)?;
    let _socket_guard = SocketGuard(socket.clone());
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let startup_message = format!(
        "{} at {baud}; {}",
        shared.0.lock().unwrap().status.port,
        if fixed {
            "fixed baud"
        } else {
            "baud unconfirmed"
        }
    );
    record(&shared, "started", "sericon", &startup_message, None)?;
    let (tx, rx) = mpsc::sync_channel::<Job>(32);
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, stop.clone())?;
    }
    let accepting = Arc::new(AtomicBool::new(true));
    let handler_shared = shared.clone();
    let handler_accepting = accepting.clone();
    let handler_formulas = formulas.clone();
    let listener_thread = thread::spawn(move || {
        let active = Arc::new(AtomicUsize::new(0));
        while handler_accepting.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if active.load(Ordering::Relaxed) >= 32 {
                        drop(stream);
                        continue;
                    }
                    active.fetch_add(1, Ordering::Relaxed);
                    let shared = handler_shared.clone();
                    let tx = tx.clone();
                    let active = active.clone();
                    let formulas = handler_formulas.clone();
                    thread::spawn(move || {
                        let _ = client(stream, shared, tx, formulas);
                        active.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
    });
    println!("{}", json!({"result":shared.0.lock().unwrap().status}));
    std::io::stdout().flush()?;
    let mut detector = Detector::new(!fixed, config.serial.sample_ms, config.serial.min_bytes);
    let mut buffer = [0u8; 4096];
    let mut last_sync = Instant::now();
    let outcome = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            for _ in 0..32 {
                let Ok(job) = rx.try_recv() else {
                    break;
                };
                // Expired queued operations must not execute after the caller timed out.
                let result = if Instant::now() > job.expires {
                    Err(anyhow::anyhow!(
                        "queued operation expired without executing"
                    ))
                } else {
                    handle(
                        &job.request,
                        &mut port,
                        &shared,
                        &mut detector,
                        config,
                        &stop,
                        &formulas,
                    )
                };
                let _ = job.answer.send(result);
                if let Some(error) = shared.0.lock().unwrap().status.error.clone() {
                    bail!("{error}");
                }
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match port.read(&mut buffer) {
                Ok(0) => bail!("serial device reached end of stream"),
                Ok(n) => {
                    record(&shared, "rx", "device", "", Some(&buffer[..n]))?;
                    detector.observe(&buffer[..n], Instant::now());
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e).context("serial receive failed"),
            }
            match detector.decide(Instant::now()) {
                Decision::Lock(confidence) => {
                    detector.pause();
                    with_status(&shared, |s| {
                        s.detection = "locked".into();
                        s.confidence = Some(confidence);
                    });
                    record(
                        &shared,
                        "baud",
                        "sericon",
                        "baud locked from readable text",
                        None,
                    )?;
                }
                Decision::Next => loop {
                    detector.next(Instant::now());
                    if detector.index >= config.serial.baud_rates.len() {
                        let rate = config.serial.baud_rates[0];
                        port.set_baud_rate(rate)?;
                        detector.pause();
                        with_status(&shared, |s| {
                            s.baud = rate;
                            s.detection = "inconclusive".into();
                        });
                        record(
                            &shared,
                            "baud",
                            "sericon",
                            "no confident match; restored first rate, use rescan when target emits text",
                            None,
                        )?;
                        break;
                    }
                    let rate = config.serial.baud_rates[detector.index];
                    match port.set_baud_rate(rate) {
                        Ok(()) => {
                            with_status(&shared, |s| {
                                s.baud = rate;
                                s.detection = "detecting".into();
                            });
                            record(&shared, "baud", "sericon", &format!("trying {rate}"), None)?;
                            break;
                        }
                        Err(e) => record(
                            &shared,
                            "baud",
                            "sericon",
                            &format!("adapter rejected {rate}: {e}"),
                            None,
                        )?,
                    }
                },
                Decision::Inconclusive => {
                    with_status(&shared, |s| s.detection = "unconfirmed".into());
                }
                Decision::None => (),
            }
            if last_sync.elapsed() >= Duration::from_secs(1) {
                shared.0.lock().unwrap().journal.sync()?;
                last_sync = Instant::now();
            }
        }
        record(&shared, "stopped", "sericon", "session stopped", None)?;
        shared.0.lock().unwrap().journal.sync()?;
        Ok(())
    })();
    if let Err(e) = &outcome {
        let message = format!("{e:#}");
        with_status(&shared, |s| s.error = Some(message.clone()));
        if record(&shared, "error", "sericon", &message, None).is_err() {
            shared.0.lock().unwrap().journal.fail_logging();
            let _ = record(&shared, "error", "sericon", &message, None);
        }
    }
    with_status(&shared, |s| s.connected = false);
    formulas.shutdown();
    shared.1.notify_all();
    drop(port);
    // Give attached readers time to consume the final event before removing the socket.
    thread::sleep(Duration::from_millis(750));
    accepting.store(false, Ordering::Relaxed);
    let _ = listener_thread.join();
    outcome
}
