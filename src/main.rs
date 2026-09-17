use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use sericon::{
    config::{Baud, Config},
    devices, help,
    ipc::{self, Request},
    session::{self, Start},
    terminal,
};
use std::{
    io::{self, BufReader, IsTerminal},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    version,
    about = "A shared serial terminal. Run without arguments to discover an adapter and baud rate.",
    after_help = help::command_line()
)]
struct Args {
    #[arg(
        short = 'p',
        long,
        help = "Use this serial device instead of automatic adapter selection"
    )]
    port: Option<String>,
    #[arg(short = 'b', long, help = "Fixed baud rate, or 'auto' (default)")]
    baud: Option<String>,
    #[arg(
        long,
        help = "Parent directory for session logs (default: launch directory)"
    )]
    log_dir: Option<PathBuf>,
    #[arg(
        long,
        conflicts_with = "log",
        help = "Disable all persistent session logging"
    )]
    no_log: bool,
    #[arg(long, help = "Enable logging even if configuration disables it")]
    log: bool,
    #[arg(long, help = "Read an explicit configuration file")]
    config: Option<PathBuf>,
    #[arg(long, help = "Select a USB adapter by serial number")]
    serial_number: Option<String>,
    #[arg(
        long,
        help = "Start the session in the background and print its status as JSON"
    )]
    detach: bool,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Browse and download files through a ready embedded Linux shell
    Files {
        #[command(subcommand)]
        command: sericon::files::Command,
    },
    /// Create, validate, and run embedded Rhai formulas against shared sessions
    #[command(after_help = help::FORMULA_CLI)]
    Formulas {
        #[command(subcommand)]
        command: sericon::formula::cli::Command,
    },
    /// Show attached USB serial devices and matching adapter profiles
    Devices {
        #[arg(long)]
        json: bool,
    },
    /// List running sessions
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// Attach a terminal to a running session; omit ID if only one exists
    #[command(after_help = help::terminal())]
    Attach { session: Option<String> },
    /// Show session status as JSON
    Status { session: Option<String> },
    /// Read a page of RX/TX history as JSON; resume with next_cursor
    #[command(after_help = help::READ)]
    Read {
        session: Option<String>,
        #[arg(
            long,
            default_value_t = 0,
            help = "Return events after this cursor; 0 starts at the oldest available history"
        )]
        after: u64,
        #[arg(long, default_value_t = 32, help = "Maximum events per page (1..128)")]
        limit: usize,
        #[arg(
            long,
            default_value_t = 0,
            help = "Wait up to this many milliseconds for new events (0..30000)"
        )]
        wait_ms: u64,
    },
    /// Send text plus Enter, or explicit raw/hex input, into a running session
    #[command(after_help = help::SEND)]
    Send {
        session: String,
        text: Option<String>,
        #[arg(
            long,
            conflicts_with = "text",
            help = "Send hexadecimal bytes without Enter and retain input ownership"
        )]
        hex: Option<String>,
        #[arg(
            long,
            help = "Do not append Enter; retain input ownership until release"
        )]
        raw: bool,
    },
    /// Release the command-line sender's input reservation
    Release { session: Option<String> },
    /// Set a fixed baud rate on a running session
    Baud { session: String, rate: u32 },
    /// Restart passive baud detection
    Rescan { session: Option<String> },
    /// Stop a session, close the serial port, and flush logs
    Stop { session: Option<String> },
    /// Run a local stdio MCP bridge to existing sessions
    #[command(after_help = help::MCP)]
    Mcp,
    /// Print the default configuration as TOML
    Config,
    #[command(name = "__serve", hide = true)]
    Serve,
}
fn print(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn main() {
    let args = Args::parse();
    let server = matches!(args.command, Some(Action::Serve));
    if let Err(error) = run(args) {
        if server {
            println!("{}", json!({"error":format!("{error:#}")}));
        } else {
            eprintln!("sericon: {error:#}");
        }
        std::process::exit(1);
    }
}
fn run(args: Args) -> Result<()> {
    let startup_flags = args.port.is_some()
        || args.baud.is_some()
        || args.log_dir.is_some()
        || args.no_log
        || args.log
        || args.serial_number.is_some()
        || args.detach;
    if args.command.is_some() && startup_flags {
        bail!(
            "connection and logging flags apply when starting a session; an existing session retains its settings"
        );
    }
    if args.config.is_some()
        && args.command.as_ref().is_some_and(|c| {
            !matches!(
                c,
                Action::Devices { .. }
                    | Action::Formulas { .. }
                    | Action::Mcp
                    | Action::Attach { .. }
            )
        })
    {
        bail!("--config applies to startup, discovery, attach, formulas, and MCP");
    }
    if let Some(action) = args.command {
        return match action {
            Action::Files { command } => sericon::files::cli(command),
            Action::Formulas { command } => {
                sericon::formula::cli::run(command, args.config.as_deref())
            }
            Action::Serve => {
                let input = ipc::frame(&mut BufReader::new(io::stdin().lock()))?
                    .context("missing launch specification")?;
                session::serve(serde_json::from_str(&input)?)
            }
            Action::Mcp => sericon::mcp::run_with_config(args.config),
            Action::Config => {
                println!("{}", toml::to_string_pretty(&Config::default())?);
                Ok(())
            }
            Action::Devices { json: as_json } => {
                let config = Config::load(args.config.as_deref())?;
                config.validate()?;
                let devices = devices::discover()?;
                if as_json {
                    print(&json!(devices))
                } else {
                    if devices.is_empty() {
                        println!("No USB serial adapters found.");
                    }
                    for d in devices {
                        let profiles: Vec<_> = config
                            .adapters
                            .prefer
                            .iter()
                            .filter(|p| d.matches(p))
                            .cloned()
                            .collect();
                        println!(
                            "{}  {:04x}:{:04x}  {}  serial={}  interface={}  profiles={}",
                            d.port,
                            d.vid.unwrap_or(0),
                            d.pid.unwrap_or(0),
                            d.product.as_deref().unwrap_or("USB serial"),
                            d.serial_number.as_deref().unwrap_or("unknown"),
                            d.interface.map_or("unknown".into(), |n| n.to_string()),
                            profiles.join(",")
                        );
                    }
                    Ok(())
                }
            }
            Action::Sessions { json: as_json } => {
                let sessions = ipc::sessions()?;
                if as_json {
                    print(&json!(sessions))
                } else {
                    if sessions.is_empty() {
                        println!("No running Sericon sessions.");
                    }
                    for s in sessions {
                        println!(
                            "{}  {}  {} baud ({})  writer={}",
                            s["id"].as_str().unwrap_or("?"),
                            s["port"].as_str().unwrap_or("?"),
                            s["baud"],
                            s["detection"].as_str().unwrap_or("?"),
                            s["writer"]["actor"].as_str().unwrap_or("shared")
                        );
                    }
                    Ok(())
                }
            }
            Action::Attach { session } => terminal::attach_with_config(
                &ipc::resolve(session.as_deref())?,
                args.config.as_deref(),
            ),
            Action::Status { session } => print(&ipc::call(
                &ipc::resolve(session.as_deref())?,
                &Request::Status,
            )?),
            Action::Read {
                session,
                after,
                limit,
                wait_ms,
            } => print(&ipc::call(
                &ipc::resolve(session.as_deref())?,
                &Request::Read {
                    after,
                    limit,
                    wait_ms,
                },
            )?),
            Action::Send {
                session,
                text,
                hex,
                raw,
            } => {
                let binary = hex.is_some();
                let mut data = if let Some(hex) = hex {
                    decode_hex(&hex)?
                } else {
                    text.context("provide text or --hex")?.into_bytes()
                };
                let release = !raw && !binary;
                if release {
                    data.push(b'\r');
                }
                print(&ipc::call(
                    &session,
                    &Request::Send {
                        data_base64: STANDARD.encode(data),
                        actor: "cli".into(),
                        client_id: "cli".into(),
                        release,
                    },
                )?)
            }
            Action::Release { session } => print(&ipc::call(
                &ipc::resolve(session.as_deref())?,
                &Request::Release {
                    client_id: "cli".into(),
                },
            )?),
            Action::Baud { session, rate } => print(&ipc::call(&session, &Request::Baud { rate })?),
            Action::Rescan { session } => print(&ipc::call(
                &ipc::resolve(session.as_deref())?,
                &Request::Rescan,
            )?),
            Action::Stop { session } => print(&ipc::call(
                &ipc::resolve(session.as_deref())?,
                &Request::Stop,
            )?),
        };
    }
    if !args.detach && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        bail!("interactive startup needs a terminal; use --detach to start a background session");
    }
    let mut config = Config::load(args.config.as_deref())?;
    if let Some(port) = args.port {
        config.serial.port = Some(port);
    }
    if let Some(baud) = args.baud {
        config.serial.baud = Baud::parse(&baud)?;
    }
    if let Some(directory) = args.log_dir {
        config.logging.directory = directory;
    }
    if args.no_log {
        config.logging.enabled = false;
    }
    if args.log {
        config.logging.enabled = true;
    }
    if let Some(serial) = args.serial_number {
        config.adapters.serial_number = Some(serial);
    }
    config.validate()?;
    let mut device = if let Some(port) = &config.serial.port {
        let canonical = std::fs::canonicalize(port)
            .with_context(|| format!("serial device not found: {port}"))?;
        devices::discover()?
            .into_iter()
            .find(|d| std::path::Path::new(&d.port) == canonical)
            .unwrap_or_else(|| devices::Device::explicit(canonical.display().to_string()))
    } else {
        devices::select(&devices::discover()?, &config.adapters)?
    };
    device.port = std::fs::canonicalize(&device.port)?.display().to_string();
    for existing in ipc::sessions()? {
        if existing["port"].as_str() == Some(&device.port) {
            bail!(
                "already owned by Sericon session {}; use 'sericon attach {}'",
                existing["id"],
                existing["id"].as_str().unwrap_or("")
            );
        }
    }
    let launch_dir = std::env::current_dir()?;
    if config.logging.directory.is_relative() {
        config.logging.directory = launch_dir.join(&config.logging.directory);
    }
    let status = session::launch(&Start {
        config,
        device,
        launch_dir,
    })?;
    if args.detach {
        print(&serde_json::to_value(status)?)
    } else {
        terminal::attach_with_config(&status.id, args.config.as_deref())
    }
}
fn decode_hex(text: &str) -> Result<Vec<u8>> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        bail!("hex input must contain complete byte pairs");
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).context("invalid hex input"))
        .collect()
}
