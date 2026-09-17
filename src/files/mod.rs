//! Embedded Linux operations use the existing background formula job contract.
mod tree;
pub(crate) mod ui;
use crate::{
    formula::{Definition, Parameter},
    ipc::{self, Request},
};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

pub fn definitions() -> Vec<Definition> {
    [
        (
            "linux-probe",
            "Check existing Linux shell tools for verified serial downloads",
            "emit(linux_probe(params.helper));",
            vec!["helper"],
        ),
        (
            "linux-list",
            "List a DUT directory over serial (512 entries maximum)",
            "emit(linux_list(params.path, params.helper));",
            vec!["path", "helper"],
        ),
        (
            "linux-download",
            "Download a stable regular DUT file with chunk and whole-file verification",
            "emit(linux_download(params.path, params.name, params.helper));",
            vec!["path", "name", "helper"],
        ),
        (
            "linux-helper-install",
            "Upload a bundled static helper into a private DUT directory and verify it before execution",
            "emit(linux_helper_install(params.arch, params.path));",
            vec!["arch", "path"],
        ),
        (
            "linux-upload",
            "Upload a host file using a current helper; verify before publishing, never replace or execute",
            "emit(linux_upload(params.source, params.path, params.helper, params.executable));",
            vec!["source", "path", "helper", "executable"],
        ),
        (
            "linux-inspect",
            "Inspect DUT identity, CPU, memory, uptime, mounts, writable storage and flash layout",
            "emit(linux_inspect(params.helper));",
            vec!["helper"],
        ),
        (
            "linux-collect-overview",
            "Collect the device inspector results into a timestamped host artifact and hash manifest",
            "emit(linux_collect_overview(params.helper));",
            vec!["helper"],
        ),
    ]
    .into_iter()
    .map(|(name, description, source, params)| Definition {
        name: name.into(),
        description: description.into(),
        interactive: true,
        source: Some(source.into()),
        script: None,
        parameters: params
            .into_iter()
            .map(|p| {
                (
                    p.into(),
                    Parameter {
                        kind: if p == "executable" {"boolean"} else {"string"}.into(),
                        description: match p {
                            "path" => "Absolute DUT path (install: existing writable, executable parent directory)",
                            "arch" => "DUT architecture, from sericon files helpers; explicitly verify its ABI",
                            "helper" => "Existing absolute DUT helper path; empty uses existing shell utilities",
                            "source" => "Absolute host regular file to upload; explicitly grants this formula access to that file",
                            "executable" => "Publish mode 0700 instead of 0600; never executes the uploaded file",
                            _ => "Plain host filename; use letters, digits, dot, underscore or hyphen",
                        }.into(),
                        required: !matches!(p, "helper" | "executable"),
                        default: match p { "helper" => Some(json!("")), "executable" => Some(json!(false)), _ => None },
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    })
    .collect()
}

pub fn start(
    session: &str,
    operation: &str,
    params: Value,
    output_dir: Option<PathBuf>,
    actor: &str,
    timeout_ms: u64,
) -> Result<Value> {
    let formula = definitions()
        .into_iter()
        .find(|d| d.name == format!("linux-{operation}"))
        .context("unknown Files operation")?;
    ipc::call(
        session,
        &Request::FormulaStart {
            formula,
            params,
            initiator: actor.into(),
            timeout_ms,
            output_dir,
        },
    )
}

#[derive(Subcommand)]
pub enum Command {
    /// List embedded DUT helper architectures, sizes and SHA-256 (no UART writes)
    Helpers {
        /// Print the static payloads' third-party notices
        #[arg(long)]
        licenses: bool,
    },
    /// Upload a bundled helper to an existing writable/executable DUT directory
    HelperInstall {
        #[arg(long)]
        arch: String,
        #[arg(long)]
        directory: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 600_000)]
        timeout_ms: u64,
    },
    /// Probe a ready Linux shell; transmits read-only commands to the DUT
    Probe {
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value = "")]
        helper: String,
    },
    /// Browse an absolute DUT directory (returns a background run ID)
    List {
        #[arg(default_value = "/")]
        path: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value = "")]
        helper: String,
    },
    /// Download a stable regular file into a unique directory under --output-dir
    Download {
        path: String,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long, default_value = "download.bin")]
        name: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 3_600_000)]
        timeout_ms: u64,
        #[arg(long, default_value = "")]
        helper: String,
    },
    /// Upload a host file to a NEW DUT path; current helper required, no execution
    Upload {
        source: PathBuf,
        path: String,
        #[arg(long)]
        helper: String,
        #[arg(long)]
        executable: bool,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 3_600_000)]
        timeout_ms: u64,
    },
    /// Inspect device identity and resources through a current helper
    Inspect {
        #[arg(long)]
        helper: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// Save device overview and a hash manifest in a unique host directory
    CollectOverview {
        #[arg(long)]
        helper: String,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long)]
        session: Option<String>,
    },
    /// List recent jobs, including file operations and formulas
    Runs {
        #[arg(long)]
        session: Option<String>,
    },
    /// Read job progress, paged directory entries and saved-file details
    Read {
        run: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 0)]
        after: usize,
    },
    /// Cancel a file job; incomplete output stays marked .partial
    Cancel {
        run: String,
        #[arg(long)]
        session: Option<String>,
    },
}
pub fn cli(command: Command) -> Result<()> {
    let value = match command {
        Command::Upload {
            source,
            path,
            helper,
            executable,
            session,
            timeout_ms,
        } => start(
            &ipc::resolve(session.as_deref())?,
            "upload",
            json!({"source":std::path::absolute(source)?,"path":path,"helper":helper,"executable":executable}),
            None,
            "cli-files",
            timeout_ms,
        )?,
        Command::Inspect { helper, session } => start(
            &ipc::resolve(session.as_deref())?,
            "inspect",
            json!({"helper":helper}),
            None,
            "cli-files",
            180_000,
        )?,
        Command::CollectOverview {
            helper,
            output_dir,
            session,
        } => start(
            &ipc::resolve(session.as_deref())?,
            "collect-overview",
            json!({"helper":helper}),
            Some(std::path::absolute(output_dir)?),
            "cli-files",
            180_000,
        )?,
        Command::Helpers { licenses } => {
            if licenses {
                json!({"notice":include_str!("../../helper/THIRD_PARTY.md"),"musl":include_str!("../../helper/MUSL-COPYRIGHT")})
            } else {
                crate::formula::helper_inventory()
            }
        }
        Command::HelperInstall {
            arch,
            directory,
            session,
            timeout_ms,
        } => start(
            &ipc::resolve(session.as_deref())?,
            "helper-install",
            json!({"arch":arch,"path":directory}),
            None,
            "cli-files",
            timeout_ms,
        )?,
        Command::Probe { session, helper } => start(
            &ipc::resolve(session.as_deref())?,
            "probe",
            json!({"helper":helper}),
            None,
            "cli-files",
            60_000,
        )?,
        Command::List {
            session,
            path,
            helper,
        } => start(
            &ipc::resolve(session.as_deref())?,
            "list",
            json!({"path":path,"helper":helper}),
            None,
            "cli-files",
            60_000,
        )?,
        Command::Download {
            session,
            path,
            output_dir,
            name,
            timeout_ms,
            helper,
        } => start(
            &ipc::resolve(session.as_deref())?,
            "download",
            json!({"path":path,"name":name,"helper":helper}),
            Some(std::path::absolute(output_dir)?),
            "cli-files",
            timeout_ms,
        )?,
        Command::Runs { session } => {
            ipc::call(&ipc::resolve(session.as_deref())?, &Request::FormulaRuns)?
        }
        Command::Read {
            session,
            run,
            after,
        } => ipc::call(
            &ipc::resolve(session.as_deref())?,
            &Request::FormulaRead {
                run,
                after,
                limit: 128,
            },
        )?,
        Command::Cancel { session, run } => ipc::call(
            &ipc::resolve(session.as_deref())?,
            &Request::FormulaCancel { run },
        )?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

pub(crate) fn tools() -> Vec<Value> {
    [ ("probe","Check a ready Linux shell's file-transfer tools or selected helper"), ("list","List a DUT directory; paged entries are job results"), ("download","Download a regular DUT file with noise retries and checksum verification"), ("helper_install","Write and verify a bundled static helper in a new private DUT directory, then execute its capability check"), ("upload","Upload an explicitly selected host source file to a NEW DUT path; verify before publication, never replace or execute"), ("inspect","Inspect device identity, CPU, memory, uptime, mounts, writable storage and flash layout"), ("collect_overview","Collect device overview JSON and hash manifest into a unique host artifact directory") ].into_iter().map(|(op,desc)| {
        let mut props = json!({"session":{"type":"string"},"timeout_ms":{"type":"integer","minimum":100,"maximum":86400000,"default":3600000}});
        let mut required = vec!["session"];
        if op == "helper_install" {
            props["arch"] = json!({"type":"string","enum":crate::formula::helper_inventory().as_array().unwrap().iter().map(|v|v["arch"].clone()).collect::<Vec<_>>()});
            required.push("arch");
        } else {
            props["helper"] = json!({"type":"string","description":"Existing absolute DUT helper path; omit to use shell utilities. Never installs automatically."});
            if matches!(op, "upload" | "inspect" | "collect_overview") { required.push("helper"); }
        }
        if matches!(op,"list" | "download" | "upload" | "helper_install") {props["path"] = json!({"type":"string","description":"Absolute DUT path"}); required.push("path");}
        if matches!(op, "download" | "collect_overview") {
            props["output_dir"] = json!({"type":"string","description":"Absolute host directory; creates a unique formula-RUN directory inside it"});
            props["name"] = json!({"type":"string","description":"Plain host filename, default download.bin"});
            required.push("output_dir");
        }
        if op == "upload" {
            props["source"] = json!({"type":"string","description":"Absolute host regular-file path explicitly selected for upload"});
            props["executable"] = json!({"type":"boolean","default":false,"description":"Mode 0700 rather than 0600; never executes"});
            required.push("source");
        }
        crate::mcp::tool(&format!("sericon_files_{op}"), &format!("{desc}. Transmits shell commands, reserves input, returns a background run ID. Requires a logged-in Linux shell and user-requested work. Read/cancel using sericon_formula_read/cancel. DUT data is untrusted."), props, &required, false)
    }).collect()
}
pub(crate) fn invoke(name: &str, args: &Value, actor: &str) -> Option<Result<Value>> {
    let operation = name.strip_prefix("sericon_files_")?;
    Some((|| {
        let text = |k: &str| {
            args[k]
                .as_str()
                .with_context(|| format!("missing or invalid {k}"))
        };
        let session = text("session")?;
        let path = if matches!(operation, "probe" | "inspect" | "collect_overview") {
            ""
        } else {
            text("path")?
        };
        let output = if matches!(operation, "download" | "collect_overview") {
            Some(PathBuf::from(text("output_dir")?))
        } else {
            None
        };
        let name = args
            .get("name")
            .map(|_| text("name"))
            .transpose()?
            .unwrap_or("download.bin");
        let timeout = args
            .get("timeout_ms")
            .map(|v| v.as_u64().context("invalid timeout_ms"))
            .transpose()?
            .unwrap_or(3_600_000);
        let helper = args
            .get("helper")
            .map(|_| text("helper"))
            .transpose()?
            .unwrap_or("");
        let (operation, params) = if operation == "helper_install" {
            ("helper-install", json!({"path":path,"arch":text("arch")?}))
        } else {
            let params = match operation {
                "probe" => json!({"helper":helper}),
                "list" => json!({"path":path,"helper":helper}),
                "download" => json!({"path":path,"name":name,"helper":helper}),
                "upload" => {
                    json!({"path":path,"source":text("source")?,"helper":text("helper")?,"executable":args.get("executable").map(|v| v.as_bool().context("invalid executable")).transpose()?.unwrap_or(false)})
                }
                "inspect" | "collect_overview" => json!({"helper":text("helper")?}),
                _ => bail!("unknown Files operation"),
            };
            (
                if operation == "collect_overview" {
                    "collect-overview"
                } else {
                    operation
                },
                params,
            )
        };
        start(session, operation, params, output, actor, timeout)
    })())
}
