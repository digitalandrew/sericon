use crate::{
    ipc::{self, Request},
    journal::{Page, printable},
    live::{Live, Screen},
    menu::{self, Menu},
    search::{Action, View},
    session::Status,
    terminal_input::{Decoder, Event},
    tui::{self, Tone},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::{self, IsTerminal, Read},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

struct Raw(libc::termios);
impl Raw {
    fn enter() -> Result<Self> {
        let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
        // stdin is checked to be a terminal before querying and restoring its attributes.
        unsafe {
            if libc::tcgetattr(0, saved.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error().into());
            }
            let saved = saved.assume_init();
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error().into());
            }
            Ok(Self(saved))
        }
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}
fn notice(live: &Arc<Mutex<Live>>, message: &str) {
    let tone = if message.starts_with("ERROR:") || message.starts_with("input not completed:") {
        Tone::Error
    } else if message.contains("evicted") {
        Tone::Warning
    } else {
        Tone::Muted
    };
    let mut live = live.lock().unwrap();
    live.notice(message, tone);
    let _ = live.render();
}

// Redraw the view under the popup without letting the output thread paint over it.
fn redraw_view(
    live: &Arc<Mutex<Live>>,
    help: &mut Option<tui::Help>,
    search: &mut Option<View>,
    formulas: &mut Option<crate::formula::ui::View>,
    files: &mut Option<crate::files::ui::View>,
) -> Result<()> {
    if let Some(view) = help {
        view.redraw()?;
    } else if let Some(view) = search {
        view.render()?;
    } else if let Some(view) = formulas {
        view.redraw()?;
    } else if let Some(view) = files {
        view.redraw()?;
    } else {
        live.lock().unwrap().redraw()?;
    }
    Ok(())
}
fn resume_view(
    live: &Arc<Mutex<Live>>,
    help: &mut Option<tui::Help>,
    search: &mut Option<View>,
    formulas: &mut Option<crate::formula::ui::View>,
    files: &mut Option<crate::files::ui::View>,
) -> Result<()> {
    if help.is_none() && search.is_none() && formulas.is_none() && files.is_none() {
        live.lock().unwrap().resume();
    }
    redraw_view(live, help, search, formulas, files)
}

pub fn attach(id: &str) -> Result<()> {
    attach_with_config(id, None)
}
pub fn attach_with_config(id: &str, config: Option<&std::path::Path>) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("attach needs a terminal; use 'sericon read' and 'sericon send' for scripts");
    }
    let status: Status = serde_json::from_value(ipc::call(id, &Request::Status)?)?;
    let terminal = crate::config::Config::load(config)?.terminal;
    terminal.validate()?;
    let log_directory = status.log_directory.clone();
    let client_id = format!("human-{}", uuid::Uuid::new_v4().simple());
    let done = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, done.clone())?;
    }
    let _raw = Raw::enter()?;
    let screen = Screen::enter(terminal.mouse)?;
    let live = Arc::new(Mutex::new(Live::new(status, terminal.scrollback_lines)));
    live.lock().unwrap().render()?;
    let output_done = done.clone();
    let output_id = id.to_string();
    let output_live = live.clone();
    let output = thread::spawn(move || -> Result<()> {
        let mut cursor = 0;
        let result = (|| -> Result<()> {
            while !output_done.load(Ordering::Relaxed) {
                if output_live.lock().unwrap().paused {
                    thread::sleep(Duration::from_millis(25));
                    continue;
                }
                let page: Page = serde_json::from_value(ipc::call(
                    &output_id,
                    &Request::Read {
                        after: cursor,
                        limit: 128,
                        wait_ms: 50,
                    },
                )?)?;
                // Entering search and live rendering share this lock. Keep the
                // live cursor unchanged while searching; the broker continues
                // capture and supplies the backlog when the view returns.
                let mut display = output_live.lock().unwrap();
                if display.paused || output_done.load(Ordering::Relaxed) {
                    continue;
                }
                if page.history_gap {
                    display.notice(
                        "earlier in-memory history was evicted; showing available history",
                        Tone::Warning,
                    );
                }
                for event in &page.events {
                    match event.kind.as_str() {
                        "rx" => {
                            if let Some(encoded) = &event.data_base64 {
                                let bytes = STANDARD.decode(encoded)?;
                                display.receive(&bytes);
                            }
                        }
                        "tx" if event.actor != "human" => {
                            if let Some(encoded) = &event.data_base64 {
                                display.notice(
                                    &format!(
                                        "{} TX: {}",
                                        event.actor,
                                        printable(&STANDARD.decode(encoded)?)
                                    ),
                                    Tone::Accent,
                                );
                            }
                        }
                        "baud" => display.notice(
                            &format!("{} baud: {}", event.baud, event.message),
                            Tone::Muted,
                        ),
                        "error" => {
                            display.notice(&format!("ERROR: {}", event.message), Tone::Error)
                        }
                        "writer" => (),
                        "stopped" => {
                            display.notice("session stopped", Tone::Muted);
                            output_done.store(true, Ordering::Relaxed);
                        }
                        _ => (),
                    }
                }
                cursor = page.next_cursor;
                if page
                    .events
                    .iter()
                    .any(|e| matches!(e.kind.as_str(), "baud" | "writer" | "error"))
                    && !output_done.load(Ordering::Relaxed)
                {
                    let status: Status =
                        serde_json::from_value(ipc::call(&output_id, &Request::Status)?)?;
                    if !status.connected {
                        output_done.store(true, Ordering::Relaxed);
                    }
                    display.update_status(status);
                }
                display.render()?;
            }
            Ok(())
        })();
        output_done.store(true, Ordering::Relaxed);
        result
    });
    let mut prefix = false;
    let mut hold = false;
    let mut stop_session = false;
    let mut menu: Option<Menu> = None;
    let mut search: Option<View> = None;
    let mut help_view: Option<tui::Help> = None;
    let mut files_view: Option<crate::files::ui::View> = None;
    let mut formula_view: Option<crate::formula::ui::View> = None;
    let mut decoder = Decoder::default();
    let input_result = (|| -> Result<()> {
        while !done.load(Ordering::Relaxed) {
            if menu.as_mut().is_some_and(Menu::escape_expired) {
                menu = None;
                resume_view(
                    &live,
                    &mut help_view,
                    &mut search,
                    &mut formula_view,
                    &mut files_view,
                )?;
            }
            if let Some(view) = &mut menu {
                if view.resized() {
                    redraw_view(
                        &live,
                        &mut help_view,
                        &mut search,
                        &mut formula_view,
                        &mut files_view,
                    )?;
                }
                view.tick()?;
            } else {
                live.lock().unwrap().render()?;
                if let Some(view) = &mut help_view {
                    view.tick()?;
                }
                if let Some(view) = &mut search {
                    view.tick()?;
                }
                if let Some(view) = &mut files_view {
                    view.tick()?;
                }
                if let Some(view) = &mut formula_view {
                    view.tick()?;
                }
                if files_view.as_ref().is_some_and(|v| v.closed) {
                    files_view = None;
                    live.lock().unwrap().resume();
                }
                if formula_view.as_ref().is_some_and(|v| v.closed) {
                    formula_view = None;
                    live.lock().unwrap().resume();
                }
            }
            let mut pfd = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut pfd, 1, 100) };
            if n < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::last_os_error().into());
            }
            if pfd.revents & libc::POLLHUP != 0 {
                break;
            }
            let mut bytes = [0u8; 256];
            let events = if n == 0 {
                decoder.expire()
            } else {
                let n = io::stdin().read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                decoder.push(&bytes[..n])
            };
            let mut pending = Vec::new();
            'events: for event in events {
                let modal = menu.is_some()
                    || help_view.is_some()
                    || search.is_some()
                    || formula_view.is_some()
                    || files_view.is_some();
                let keys = match event {
                    Event::Wheel { up, x, y } => {
                        send_pending(id, &client_id, &mut pending, hold, &live);
                        if let Some(view) = &mut menu {
                            view.key(if up { b'k' } else { b'j' });
                        } else if let Some(view) = &mut help_view {
                            for _ in 0..3 {
                                view.key(if up { b'k' } else { b'j' });
                            }
                        } else if let Some(view) = &mut files_view {
                            view.wheel(up)?;
                        } else if let Some(view) = &mut formula_view {
                            view.wheel(up)?;
                        } else if let Some(view) = &mut search {
                            view.wheel(up)?;
                        } else if !modal {
                            let mut display = live.lock().unwrap();
                            if display.contains(x, y) {
                                display.scroll(up, false);
                                display.render()?;
                            }
                        }
                        continue;
                    }
                    Event::Page { up, bytes } => {
                        if !modal && !prefix {
                            send_pending(id, &client_id, &mut pending, hold, &live);
                            let mut display = live.lock().unwrap();
                            display.scroll(up, true);
                            display.render()?;
                            continue;
                        }
                        bytes
                    }
                    Event::End(bytes) => {
                        if !modal && !prefix {
                            send_pending(id, &client_id, &mut pending, hold, &live);
                            let mut display = live.lock().unwrap();
                            if display.scrolled() {
                                display.bottom();
                                display.render()?;
                                continue;
                            }
                        }
                        bytes
                    }
                    Event::Bytes(bytes) => bytes,
                };
                for &input in &keys {
                    let mut byte = input;
                    if let Some(view) = &mut menu {
                        match view.key(byte) {
                            menu::Action::Stay => continue,
                            menu::Action::Close => {
                                menu = None;
                                resume_view(
                                    &live,
                                    &mut help_view,
                                    &mut search,
                                    &mut formula_view,
                                    &mut files_view,
                                )?;
                                continue;
                            }
                            menu::Action::Command(command) => {
                                menu = None;
                                help_view = None;
                                search = None;
                                formula_view = None;
                                files_view = None;
                                live.lock().unwrap().resume();
                                byte = command;
                                prefix = true;
                            }
                        }
                    } else {
                        let action = if let Some(view) = &mut help_view {
                            Some(view.key(byte))
                        } else if let Some(view) = &mut files_view {
                            Some(view.key(byte)?)
                        } else if let Some(view) = &mut formula_view {
                            Some(view.key(byte)?)
                        } else if let Some(view) = &mut search {
                            Some(view.key(byte)?)
                        } else {
                            None
                        };
                        if let Some(action) = action {
                            match action {
                                Action::Stay => continue,
                                Action::Menu => {
                                    menu = Some(Menu::new(hold));
                                    continue;
                                }
                                Action::Back | Action::Stop | Action::Detach => {
                                    help_view = None;
                                    formula_view = None;
                                    files_view = None;
                                    search = None;
                                    live.lock().unwrap().resume();
                                    byte = match action {
                                        Action::Stop => b'q',
                                        Action::Detach => b'd',
                                        _ => b'b',
                                    };
                                    prefix = true;
                                }
                            }
                        }
                    }
                    if prefix {
                        prefix = false;
                        let request = match byte {
                            b'm' => {
                                live.lock().unwrap().paused = true;
                                menu = Some(Menu::new(hold));
                                None
                            }
                            b'l' => {
                                live.lock().unwrap().paused = true;
                                files_view = Some(crate::files::ui::View::open(id, &client_id));
                                None
                            }
                            b'x' => {
                                live.lock().unwrap().paused = true;
                                match crate::formula::ui::View::open(id, &client_id, config) {
                                    Ok(view) => formula_view = Some(view),
                                    Err(e) => {
                                        live.lock().unwrap().resume();
                                        notice(&live, &format!("{e:#}"));
                                    }
                                }
                                None
                            }
                            b'f' => {
                                live.lock().unwrap().paused = true;
                                search = Some(View::open(id)?);
                                None
                            }
                            b'b' => {
                                let mut display = live.lock().unwrap();
                                display.bottom();
                                display.render()?;
                                None
                            }
                            b'q' => {
                                stop_session = true;
                                done.store(true, Ordering::Relaxed);
                                Some(Request::Stop)
                            }
                            b'd' => {
                                done.store(true, Ordering::Relaxed);
                                None
                            }
                            b'a' => {
                                hold = false;
                                Some(Request::Release {
                                    client_id: client_id.clone(),
                                })
                            }
                            b't' => Some(Request::Claim {
                                actor: "human".into(),
                                client_id: client_id.clone(),
                                takeover: true,
                            }),
                            b'o' => {
                                hold = !hold;
                                if hold {
                                    Some(Request::Claim {
                                        actor: "human".into(),
                                        client_id: client_id.clone(),
                                        takeover: true,
                                    })
                                } else {
                                    Some(Request::Release {
                                        client_id: client_id.clone(),
                                    })
                                }
                            }
                            b'r' => Some(Request::Rescan),
                            b']' => {
                                pending.push(29);
                                None
                            }
                            _ => {
                                live.lock().unwrap().paused = true;
                                help_view = Some(tui::Help::open()?);
                                None
                            }
                        };
                        if let Some(request) = request
                            && let Err(e) = ipc::call(id, &request)
                        {
                            notice(&live, &format!("{e:#}"));
                        }
                        if done.load(Ordering::Relaxed) {
                            break 'events;
                        }
                    } else if byte == 29 {
                        send_pending(id, &client_id, &mut pending, hold, &live);
                        prefix = true;
                    } else {
                        pending.push(byte);
                        if matches!(byte, b'\r' | b'\n' | 3 | 4) {
                            send_pending(id, &client_id, &mut pending, hold, &live);
                        }
                    }
                }
            }
            send_pending(id, &client_id, &mut pending, hold, &live);
        }
        Ok(())
    })();
    done.store(true, Ordering::Relaxed);
    drop(search);
    drop(help_view);
    drop(formula_view);
    drop(files_view);
    live.lock().unwrap().paused = true;
    let output_result = output
        .join()
        .map_err(|_| anyhow::anyhow!("terminal output thread failed"))?;
    drop(screen);
    // An unfinished command remains reserved even after its terminal disconnects.
    if !stop_session {
        tui::notice(
            &format!(
                "detached; 'sericon attach {id}' resumes. Any unfinished input remains reserved; Ctrl-] t takes it over."
            ),
            Tone::Muted,
        );
    } else {
        let log_message = match log_directory {
            Some(path) => format!("log saved to {}", path.display()),
            None => "logging disabled; no session log saved".into(),
        };
        tui::notice(&format!("{log_message}\nsession stopped"), Tone::Muted);
    }
    input_result?;
    if !stop_session {
        output_result.context("session output ended")?;
    }
    Ok(())
}
fn send_pending(
    id: &str,
    client: &str,
    pending: &mut Vec<u8>,
    hold: bool,
    live: &Arc<Mutex<Live>>,
) {
    if pending.is_empty() {
        return;
    }
    {
        let mut display = live.lock().unwrap();
        display.bottom();
        let _ = display.render();
    }
    let release = !hold
        && pending
            .last()
            .is_some_and(|b| matches!(*b, b'\r' | b'\n' | 3 | 4));
    let request = Request::Send {
        data_base64: STANDARD.encode(&*pending),
        actor: "human".into(),
        client_id: client.into(),
        release,
    };
    if let Err(e) = ipc::call(id, &request) {
        notice(
            live,
            &format!("input not completed: {e:#}; Ctrl-] t takes input ownership"),
        );
    }
    pending.clear();
}
