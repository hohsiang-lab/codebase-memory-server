use super::*;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;

#[derive(Clone)]
pub struct Engine {
    cache: PathBuf,
    runtime: PathBuf,
    index_timeout: Duration,
}

impl Engine {
    pub async fn new() -> Result<Self> {
        let cache = env_path("CBM_CACHE_DIR", "/data/cbm");
        let runtime = env_path("CBM_RUNTIME_DIR", "/data/cbm/runtime");
        fs::create_dir_all(&cache)?;
        no_symlink(&runtime)?;
        fs::create_dir_all(&runtime)?;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;
        let engine = Self {
            cache,
            runtime,
            index_timeout: seconds("INDEX_TIMEOUT_SECONDS", 5400)?,
        };
        for key in ["auto_index", "auto_watch", "watcher_enabled"] {
            output(engine.command().args(["config", "set", key, "false"]), Duration::from_secs(30)).await?;
        }
        Ok(engine)
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new("codebase-memory-mcp");
        cmd.env("CBM_CACHE_DIR", &self.cache)
            .env("CBM_RUNTIME_DIR", &self.runtime)
            .env("CBM_ALLOWED_ROOT", ROOT);
        cmd
    }

    async fn cli(&self, tool: &str, mut args: Value, limit: Duration) -> Result<Value> {
        args["format"] = json!("json");
        let mut file = tempfile::NamedTempFile::new()?;
        serde_json::to_writer(&mut file, &args)?;
        file.flush()?;
        let result = output(self.command().args(["cli", "--quiet", tool, "--args-file"])
            .arg(file.path()).args(["--format", "json"]), limit).await?;
        let value: Value = serde_json::from_str(&result).context("CBM returned invalid JSON")?;
        ensure!(value.get("isError").and_then(Value::as_bool) != Some(true), "CBM tool failed");
        // Accept the engine's JSON payload, or a standard MCP text result.
        let value = match value.get("content").and_then(Value::as_array) {
            Some(content) => {
                let text = content.first().and_then(|v| v.get("text")).and_then(Value::as_str)
                    .context("missing CBM response payload")?;
                serde_json::from_str(text)?
            }
            None => value,
        };
        ensure!(value.get("error").is_none()
            && !matches!(value.get("status").and_then(Value::as_str), Some("error" | "failed" | "cancelled" | "running" | "queued")),
            "CBM returned an unsuccessful result");
        Ok(value)
    }

    async fn catalog(&self) -> Result<BTreeMap<String, String>> {
        let mut projects = BTreeMap::new();
        let mut offset = 0_u64;
        loop {
            let page = self.cli("list_projects", json!({"limit":500, "offset":offset, "detail":"identity"}), Duration::from_secs(120)).await?;
            let rows = page["projects"].as_array().context("invalid CBM catalog")?;
            for item in rows {
                let name = item["name"].as_str().context("missing project name")?;
                let root = item["root_path"].as_str().context("missing project root")?;
                ensure!(projects.insert(name.to_owned(), root.to_owned()).is_none(), "duplicate project in catalog");
            }
            if !page["has_more"].as_bool().context("invalid catalog paging")? {
                return Ok(projects);
            }
            let next = page["next_offset"].as_u64().context("missing catalog offset")?;
            ensure!(next > offset, "non-advancing catalog offset");
            offset = next;
        }
    }

    fn status(&self, name: &str) -> PathBuf {
        self.cache.join("sync-status").join(format!("{name}.json"))
    }

    async fn index_one(&self, name: &str, force: bool) -> Result<Option<String>> {
        let Some(_lock) = lock(name, false).await? else { return Ok(None) };
        let Some(wanted) = source_sha(name)? else { return Ok(None) };
        let path = checkout(name);
        ensure!(git(&path, &["rev-parse", "HEAD"], None, 20).await? == wanted, "checkout SHA differs from marker");
        // The engine can auto-import a committed graph artifact. Do not
        // let source repositories restore untrusted or revoked graph data.
        match fs::symlink_metadata(path.join(".codebase-memory/graph.db.zst")) {
            Ok(_) => bail!("repository-provided graph artifacts are not supported"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {},
            Err(e) => return Err(e.into()),
        }
        let state = read_json(&self.status(name))?.unwrap_or_else(|| json!({}));
        if !force && state["indexed_sha"].as_str() == Some(&wanted) {
            return Ok(Some(wanted));
        }
        // Invalidate before indexing; never report a failed index as current.
        remove_if_exists(&self.status(name))?;
        log("index_started", name);
        self.cli("index_repository", json!({"repo_path":path, "name":name,
            "mode":"full", "persistence":false}), self.index_timeout).await?;
        atomic_json(&self.status(name), &json!({"indexed_sha":wanted,
            "indexed_at":Utc::now().to_rfc3339()}))?;
        println!("{}", json!({"event":"index_succeeded", "repo":name, "sha":wanted}));
        Ok(Some(wanted))
    }

    async fn cross_repo(&self, name: &str, peers: &[String]) -> Result<()> {
        if peers.is_empty() { return Ok(()) }
        // Lock all participating checkouts in deterministic order, then check
        // both synchronization and index SHA under those locks.
        let participants: BTreeSet<&str> = std::iter::once(name)
            .chain(peers.iter().map(String::as_str)).collect();
        let mut locks = Vec::new();
        let mut shas = BTreeMap::new();
        for repo in participants {
            let Some(handle) = lock(repo, false).await? else { return Ok(()) };
            locks.push(handle);
            let Some(sha) = source_sha(repo)? else { return Ok(()) };
            let state = read_json(&self.status(repo))?.unwrap_or_else(|| json!({}));
            if state["indexed_sha"].as_str() != Some(&sha)
                || git(&checkout(repo), &["rev-parse", "HEAD"], None, 20).await? != sha {
                return Ok(());
            }
            shas.insert(repo, sha);
        }
        let state_path = self.status(name);
        let mut state = read_json(&state_path)?.context("missing source index state")?;
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&shas)?));
        if state["cross_repo_fingerprint"].as_str() == Some(&fingerprint) {
            return Ok(());
        }
        self.cli("index_repository", json!({"repo_path":checkout(name), "name":name,
            "mode":"cross-repo-intelligence", "persistence":false, "target_projects":peers}),
            self.index_timeout).await?;
        state["cross_repo_fingerprint"] = json!(fingerprint);
        state["cross_repo_at"] = json!(Utc::now().to_rfc3339());
        atomic_json(&state_path, &state)?;
        log("cross_repo_succeeded", name);
        Ok(())
    }

    /// Reconcile the dedicated cache before opening the HTTP endpoint.
    pub async fn prepare(&self, config: &Config) -> Result<BTreeSet<String>> {
        let names = config.names();
        let catalog = self.catalog().await?;
        let policy = self.cache.join("catalog-policy.json");
        let rebuild = self.cache.join("catalog-rebuild.json");
        let desired = serde_json::to_value(config)?;
        let old: Option<Config> = read_json(&policy)?
            .map(serde_json::from_value).transpose()?;
        // Cross-repo edges are bidirectional. The upstream full mode can
        // take an unchanged-source no-op, and cross-repo mode cannot clear
        // edges with an empty target list. A revoked or unknown policy must
        // therefore discard derived graph generations, not merely reindex.
        let reset = rebuild.exists()
            || catalog.keys().any(|name| !names.contains(name))
            || old.as_ref().map_or(!catalog.is_empty(), |old| {
                !old.names().is_subset(&names)
                    || old.cross_repo.iter().any(|(source, peers)| {
                        peers.iter().any(|peer| {
                            !config.cross_repo.get(source).is_some_and(|new| new.contains(peer))
                        })
                    })
            });
        for (name, root) in &catalog {
            ensure!(safe_name(name) && Path::new(root) == checkout(name),
                "cache contains an unmanaged project; use a dedicated cache");
        }
        if reset {
            // Persist the reset before the first deletion. An interrupted
            // reset repeats before HTTP opens. Only graph data is discarded;
            // checkouts remain untouched and the normal worker rebuilds
            // approved projects after startup.
            atomic_json(&rebuild, &json!({"required":true}))?;
            for name in catalog.keys() {
                self.cli("delete_project", json!({"project":name}), Duration::from_secs(120)).await?;
                remove_if_exists(&self.status(name))?;
                log("project_removed", name);
            }
            ensure!(self.catalog().await?.is_empty(), "catalog reset incomplete");
        }
        let current = if reset { BTreeMap::new() } else { catalog };
        atomic_json(&policy, &desired)?;
        remove_if_exists(&rebuild)?;
        Ok(current.into_keys().collect())
    }

    pub async fn run(&self, config: Config, mut available: BTreeSet<String>) -> Result<()> {
        let interval = seconds("INDEX_POLL_SECONDS", 15)?;
        loop {
            for name in config.names() {
                match self.index_one(&name, !available.contains(&name)).await {
                    Ok(Some(_)) => { available.insert(name); }
                    Ok(None) => {}
                    Err(_) => log("index_failed", &name),
                }
            }
            for (name, peers) in &config.cross_repo {
                if self.cross_repo(name, peers).await.is_err() {
                    log("cross_repo_failed", name);
                }
            }
            tokio::time::sleep(interval).await;
        }
    }
}
