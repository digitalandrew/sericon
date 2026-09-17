use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const SOURCE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub parameters: BTreeMap<String, Parameter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<PathBuf>,
}

pub fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 48
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        bail!("name must be 1..48 ASCII letters, digits, '_' or '-'");
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String> {
    let mut text = String::new();
    fs::File::open(path)?
        .take((SOURCE_LIMIT * 2 + 1) as u64)
        .read_to_string(&mut text)?;
    if text.len() > SOURCE_LIMIT * 2 {
        bail!("formula file exceeds 128 KiB");
    }
    Ok(text)
}

impl Definition {
    pub fn resolve(mut self) -> Result<Self> {
        valid_name(&self.name)?;
        if self.description.len() > 2048 || self.parameters.len() > 32 {
            bail!("formula metadata exceeds limits");
        }
        match (&self.source, &self.script) {
            (None, Some(path)) => {
                self.source = Some(
                    read_text(path).with_context(|| format!("read formula {}", path.display()))?,
                )
            }
            (Some(_), None) => (),
            _ => bail!(
                "formula {} needs exactly one of source or script",
                self.name
            ),
        }
        self.script = None;
        if self.source.as_ref().unwrap().len() > SOURCE_LIMIT {
            bail!("formula source exceeds 64 KiB");
        }
        for (name, p) in &self.parameters {
            valid_name(name)?;
            if !["string", "integer", "boolean", "number"].contains(&p.kind.as_str())
                || p.description.len() > 2048
            {
                bail!("invalid parameter definition: {name}");
            }
            if let Some(value) = &p.default {
                check_type(name, p, value)?;
            }
        }
        Ok(self)
    }
    pub fn revision(&self) -> String {
        Sha256::digest(serde_json::to_vec(self).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    pub fn arguments(&self, value: &Value) -> Result<Value> {
        let supplied = value
            .as_object()
            .context("parameters must be a JSON object")?;
        if serde_json::to_vec(value)?.len() > 32768 {
            bail!("parameters exceed 32 KiB");
        }
        let mut merged = serde_json::Map::new();
        for name in supplied.keys() {
            if !self.parameters.contains_key(name) {
                bail!("unknown parameter: {name}");
            }
        }
        for (name, p) in &self.parameters {
            if let Some(v) = supplied.get(name).or(p.default.as_ref()) {
                check_type(name, p, v)?;
                merged.insert(name.clone(), v.clone());
            } else if p.required {
                bail!("missing parameter: {name}");
            }
        }
        Ok(Value::Object(merged))
    }
    pub fn summary(&self) -> Value {
        json!({"name":self.name,"description":self.description,"interactive":self.interactive,
            "parameters":self.parameters,"revision":self.revision()})
    }
}
fn check_type(name: &str, p: &Parameter, v: &Value) -> Result<()> {
    let valid = match p.kind.as_str() {
        "string" => v.is_string(),
        "integer" => v.as_i64().is_some(),
        "boolean" => v.is_boolean(),
        "number" => v.is_number(),
        _ => false,
    };
    if !valid {
        bail!("parameter {name} must be {}", p.kind);
    }
    Ok(())
}

fn builtins() -> Vec<Definition> {
    [
        (
            "passwords",
            "Find possible password and Wi-Fi PSK assignments in retained RX/TX history",
            include_str!("../../formulas/passwords.rhai"),
        ),
        (
            "endpoints",
            "List unique URLs and IP address candidates with counts and context",
            include_str!("../../formulas/endpoints.rhai"),
        ),
    ]
    .into_iter()
    .map(|(name, description, source)| Definition {
        name: name.into(),
        description: description.into(),
        interactive: false,
        parameters: BTreeMap::new(),
        source: Some(source.into()),
        script: None,
    })
    .collect()
}
fn directory(config: Option<&Path>) -> PathBuf {
    config
        .map(PathBuf::from)
        .unwrap_or_else(crate::config::default_path)
        .parent()
        .unwrap_or(Path::new("."))
        .join("formulas")
}
pub fn list(config: Option<&Path>) -> Result<Vec<Definition>> {
    let mut formulas = BTreeMap::new();
    let cfg = crate::config::Config::load(config)?;
    let mut all = builtins();
    all.extend(crate::files::definitions());
    all.extend(cfg.formulas);
    match fs::read_dir(directory(config)) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path.extension().is_some_and(|x| x == "toml") {
                    if all.len() >= 256 {
                        bail!("formula registry exceeds 256 entries");
                    }
                    let mut d: Definition = toml::from_str(&read_text(&path)?)
                        .with_context(|| format!("invalid formula {}", path.display()))?;
                    if let Some(script) = &mut d.script
                        && script.is_relative()
                    {
                        *script = path.parent().unwrap().join(&*script);
                    }
                    all.push(d);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    for d in all {
        let d = d.resolve()?;
        if formulas.contains_key(&d.name) {
            bail!("duplicate formula name: {}", d.name);
        }
        formulas.insert(d.name.clone(), d);
    }
    Ok(formulas.into_values().collect())
}
pub fn get(config: Option<&Path>, name: &str) -> Result<Definition> {
    list(config)?
        .into_iter()
        .find(|d| d.name == name)
        .context("unknown formula; use 'sericon formulas list'")
}
pub fn validate(definition: Definition, params: Option<&Value>) -> Result<Value> {
    let d = definition.resolve()?;
    super::runtime::compile(&d)?;
    let arguments = params.map(|p| d.arguments(p)).transpose()?;
    Ok(
        json!({"valid":true,"formula":d.summary(),"parameters":arguments,
        "note":"Syntax and supplied parameter types checked; no UART or file operations executed. Runtime names and device responses are checked when run."}),
    )
}
pub fn put(config: Option<&Path>, definition: Definition, replace: bool) -> Result<Value> {
    let d = definition.resolve()?;
    validate(d.clone(), None)?;
    if builtins()
        .iter()
        .chain(crate::files::definitions().iter())
        .any(|b| b.name == d.name)
    {
        bail!("built-in formulas cannot be replaced; use another name");
    }
    let registered = crate::config::Config::load(config)?;
    if registered.formulas.iter().any(|f| f.name == d.name) {
        bail!("name is registered in config; edit that entry or use another name");
    }
    let dir = directory(config);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", d.name));
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(toml::to_string_pretty(&d)?.as_bytes())?;
    tmp.as_file().sync_all()?;
    if replace {
        tmp.persist(&path).map_err(|e| e.error)?;
    } else {
        tmp.persist_noclobber(&path)
            .map_err(|e| e.error)
            .context("formula already exists; use --replace to update it")?;
    }
    Ok(json!({"formula":d.summary(),"path":path}))
}
