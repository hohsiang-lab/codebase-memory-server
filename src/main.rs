mod gateway;
mod index;
mod sync;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{process::Command, time::timeout};

const ROOT: &str = "/repos";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Config {
    repositories: Vec<Repository>,
    #[serde(default)]
    cross_repo: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Repository {
    name: String,
    branch: String,
}

impl Config {
    fn load() -> Result<Self> {
        let path = env_path("REPOS_CONFIG", "/etc/codebase/repos.json");
        let config: Self = serde_json::from_slice(&fs::read(path)?)?;
        config.validate()?;
        Ok(config)
    }

    fn names(&self) -> BTreeSet<String> {
        self.repositories.iter().map(|r| r.name.clone()).collect()
    }

    fn validate(&self) -> Result<()> {
        let names = self.names();
        ensure!(!names.is_empty(), "at least one repository is required");
        ensure!(names.len() == self.repositories.len(), "duplicate repository name");
        for repo in &self.repositories {
            ensure!(safe_name(&repo.name) && repo.branch == "main", "unsafe name or unsupported branch");
        }
        for (source, peers) in &self.cross_repo {
            ensure!(names.contains(source), "unknown cross-repository source");
            let unique: BTreeSet<_> = peers.iter().collect();
            ensure!(unique.len() == peers.len(), "duplicate cross-repository peer");
            for peer in peers {
                ensure!(peer != source && names.contains(peer), "invalid cross-repository peer");
            }
        }
        Ok(())
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

fn valid_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn env_path(key: &str, default: &str) -> PathBuf {
    env::var_os(key).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(default))
}

fn seconds(key: &str, default: u64) -> Result<Duration> {
    let value = match env::var(key) {
        Ok(s) => s.parse()?,
        Err(env::VarError::NotPresent) => default,
        Err(e) => return Err(e.into()),
    };
    ensure!(value > 0, "{key} must be positive");
    Ok(Duration::from_secs(value))
}

fn log(event: &str, repo: &str) {
    println!("{}", json!({"event": event, "repo": repo}));
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    match fs::read(path) {
        Ok(data) => Ok(Some(serde_json::from_slice(&data)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("state path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    // The sync marker is shared with a read-only mount using the same UID.
    serde_json::to_writer(&mut file, value)?;
    writeln!(file)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn checkout(name: &str) -> PathBuf {
    Path::new(ROOT).join(name)
}
fn marker(name: &str) -> PathBuf {
    Path::new(ROOT).join(".sync").join(format!("{name}.json"))
}
fn source_sha(name: &str) -> Result<Option<String>> {
    let value = read_json(&marker(name))?;
    match value.as_ref().and_then(|v| v.get("sha")).and_then(Value::as_str) {
        Some(s) if valid_sha(s) => Ok(Some(s.to_owned())),
        None if value.is_none() => Ok(None),
        _ => bail!("invalid synchronization marker"),
    }
}

fn no_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(!m.file_type().is_symlink(), "symlinked path refused"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

async fn lock(name: &str, create: bool) -> Result<Option<File>> {
    let dir = Path::new(ROOT).join(".locks");
    no_symlink(&dir)?;
    if create {
        fs::create_dir_all(&dir)?;
    }
    let path = dir.join(format!("{name}.lock"));
    no_symlink(&path)?;
    let file = if create {
        OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?
    } else {
        match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    };
    // ponytail: one advisory lock per checkout; all writers obey the same flock.
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(Some(file)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

async fn output(cmd: &mut Command, limit: Duration) -> Result<String> {
    cmd.kill_on_drop(true).stdin(Stdio::null());
    let result = timeout(limit, cmd.output()).await.context("command timed out")??;
    // Command output can contain credentials, URLs, or repository source.
    // Never include stderr, arguments, or environment in error messages.
    ensure!(result.status.success(), "child process failed: {}", result.status);
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}

async fn git(path: &Path, args: &[&str], token: Option<&str>, limit: u64) -> Result<String> {
    no_symlink(path)?;
    no_symlink(&path.join(".git"))?;
    let mut cmd = Command::new("git");
    cmd.args([
        "-c", "core.hooksPath=/dev/null",
        "-c", "core.fsmonitor=false",
        "-c", "credential.helper=",
        "-c", "http.followRedirects=false",
        "-c", "protocol.file.allow=never",
        "-C",
    ])
    .arg(path)
    .args(args)
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_LFS_SKIP_SMUDGE", "1")
    .env_remove("GH_INSTALL_TOKEN")
    .env_remove("GIT_ASKPASS");
    if let Some(token) = token {
        cmd.env("GH_INSTALL_TOKEN", token)
            .env("GIT_ASKPASS", "/opt/codebase/git_askpass.sh");
    }
    output(&mut cmd, Duration::from_secs(limit)).await
}

async fn shutdown() -> Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result?,
        _ = sigterm.recv() => {},
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mode = env::args().nth(1).unwrap_or_else(|| "serve".to_string());
    let config = Config::load()?;
    match mode.as_str() {
        "sync" => tokio::select! {
            result = sync::run(config) => result,
            result = shutdown() => result,
        },
        "serve" => tokio::select! {
            result = gateway::serve(config) => result,
            result = shutdown() => result,
        },
        _ => bail!("usage: codebase-memory-server [serve|sync]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unapproved_peers_and_unsafe_names() {
        let mut config: Config = serde_json::from_value(json!({
            "repositories": [{"name":"example","branch":"main"}]
        })).unwrap();
        assert!(config.validate().is_ok());
        config.cross_repo.insert("example".into(), vec!["other".into()]);
        assert!(config.validate().is_err());
        config.cross_repo.clear();
        config.repositories[0].name = "../escape".into();
        assert!(config.validate().is_err());
        assert!(!safe_name(".locks"));
        assert!(!valid_sha("wrong"));
        assert!(valid_sha(&"a".repeat(40)));
    }
}
