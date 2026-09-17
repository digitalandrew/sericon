use super::{Definition, Parameter};
use crate::ipc::{self, Request};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Subcommand)]
pub enum Command {
    /// List built-in and registered custom formulas
    List {
        #[arg(long)]
        json: bool,
    },
    /// Inspect formula source, parameters, and revision
    Show { name: String },
    /// Save a Rhai script in your formula library (syntax checked first)
    Put {
        name: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        interactive: bool,
        /// JSON object mapping parameter names to {type, required, default, description}
        #[arg(long, default_value = "{}")]
        parameters: String,
        #[arg(long)]
        replace: bool,
    },
    /// Check syntax and optional parameter values without executing the formula
    Validate {
        name: Option<String>,
        #[arg(long, conflicts_with = "name")]
        file: Option<PathBuf>,
        #[arg(long)]
        params: Option<String>,
    },
    /// Start a background run; returns a run ID immediately
    Run {
        name: String,
        /// Print JSON (also the default output format)
        #[arg(long)]
        json: bool,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value = "{}")]
        params: String,
        #[arg(long, default_value_t = 300_000)]
        timeout_ms: u64,
        /// Enable artifact files under a new private formula-RUN directory here
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    /// List recent runs for a session
    Runs {
        #[arg(long)]
        session: Option<String>,
    },
    /// Read run status, artifacts, and a page of results
    Read {
        run: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 0)]
        after: usize,
        #[arg(long, default_value_t = 32)]
        limit: usize,
    },
    /// Cancel a run; already transmitted bytes and partial artifacts remain
    Cancel {
        run: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// Print the Rhai API, execution limits, and ownership rules
    Api,
}
fn source(path: &Path) -> Result<String> {
    let mut s = String::new();
    std::fs::File::open(path)?
        .take((super::registry::SOURCE_LIMIT + 1) as u64)
        .read_to_string(&mut s)?;
    if s.len() > super::registry::SOURCE_LIMIT {
        bail!("formula source exceeds 64 KiB");
    }
    Ok(s)
}
pub fn run(command: Command, config: Option<&Path>) -> Result<()> {
    let value = match command {
        Command::List { json: as_json } => {
            let formulas = super::list(config)?;
            if !as_json {
                for f in formulas {
                    println!(
                        "{}  [{}]  {}",
                        f.name,
                        if f.interactive {
                            "interactive"
                        } else {
                            "analysis"
                        },
                        f.description
                    );
                }
                return Ok(());
            }
            json!(formulas.iter().map(Definition::summary).collect::<Vec<_>>())
        }
        Command::Show { name } => {
            let d = super::get(config, &name)?;
            json!({"formula":d,"revision":d.revision()})
        }
        Command::Put {
            name,
            file,
            description,
            interactive,
            parameters,
            replace,
        } => {
            let parameters: BTreeMap<String, Parameter> = serde_json::from_str(&parameters)?;
            super::put(
                config,
                Definition {
                    name,
                    description,
                    interactive,
                    parameters,
                    source: Some(source(&file)?),
                    script: None,
                },
                replace,
            )?
        }
        Command::Validate { name, file, params } => {
            let d = if let Some(file) = file {
                Definition {
                    name: "validation".into(),
                    description: String::new(),
                    interactive: false,
                    parameters: BTreeMap::new(),
                    source: Some(source(&file)?),
                    script: None,
                }
            } else {
                super::get(config, &name.context("provide a formula name or --file")?)?
            };
            let p: Option<Value> = params.map(|p| serde_json::from_str(&p)).transpose()?;
            super::validate(d, p.as_ref())?
        }
        Command::Run {
            name,
            json: _,
            session,
            params,
            timeout_ms,
            output_dir,
        } => ipc::call(
            &ipc::resolve(session.as_deref())?,
            &Request::FormulaStart {
                formula: super::get(config, &name)?,
                params: serde_json::from_str(&params)?,
                initiator: "cli".into(),
                timeout_ms,
                output_dir: output_dir.map(std::path::absolute).transpose()?,
            },
        )?,
        Command::Runs { session } => {
            ipc::call(&ipc::resolve(session.as_deref())?, &Request::FormulaRuns)?
        }
        Command::Read {
            run,
            session,
            after,
            limit,
        } => ipc::call(
            &ipc::resolve(session.as_deref())?,
            &Request::FormulaRead { run, after, limit },
        )?,
        Command::Cancel { run, session } => ipc::call(
            &ipc::resolve(session.as_deref())?,
            &Request::FormulaCancel { run },
        )?,
        Command::Api => {
            println!("{}", super::reference());
            return Ok(());
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
