use super::*;
use chrono::{DateTime, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::Client;

#[derive(Serialize)]
struct Claims<'a> {
    iat: i64,
    exp: i64,
    iss: &'a str,
}

#[derive(Deserialize)]
struct Token {
    token: String,
    expires_at: DateTime<Utc>,
}

async fn installation_token(client: &Client, names: &BTreeSet<String>) -> Result<Token> {
    let secret = env_path("GITHUB_APP_SECRET_DIR", "/run/secrets/github");
    let app = fs::read_to_string(secret.join("app_id"))?;
    let installation = fs::read_to_string(secret.join("installation_id"))?;
    ensure!(installation.trim().bytes().all(|b| b.is_ascii_digit()) && !installation.trim().is_empty(), "invalid installation ID");
    let key = EncodingKey::from_rsa_pem(&fs::read(secret.join("private_key"))?)
        .context("invalid GitHub App key")?;
    let now = Utc::now().timestamp();
    let jwt = encode(&Header::new(Algorithm::RS256), &Claims {
        iat: now - 30,
        exp: now + 540,
        iss: app.trim(),
    }, &key).context("GitHub App signing failed")?;
    let response = client
        .post(format!("https://api.github.com/app/installations/{}/access_tokens", installation.trim()))
        .bearer_auth(jwt)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .json(&json!({"repositories": names, "permissions": {"contents": "read"}}))
        .send().await?;
    let status = response.status();
    ensure!(status.is_success(), "GitHub App token request failed: {status}");
    let token: Token = response.json().await?;
    ensure!(!token.token.is_empty() && token.expires_at > Utc::now(), "invalid installation token response");
    Ok(token)
}

async fn sync_one(name: &str, owner: &str, token: &str) -> Result<()> {
    let _lock = lock(name, true).await?.context("missing sync lock")?;
    let path = checkout(name);
    no_symlink(&path)?;
    no_symlink(&path.join(".git"))?;
    let remote_url = format!("https://github.com/{owner}/{name}.git");
    if !path.join(".git").is_dir() {
        fs::create_dir_all(&path)?;
        ensure!(fs::read_dir(&path)?.next().is_none(), "refusing nonempty unmanaged checkout");
        git(&path, &["init", "-b", "main"], None, 30).await?;
    }
    match git(&path, &["config", "--get", "remote.origin.url"], None, 20).await {
        Ok(origin) => ensure!(origin == remote_url, "checkout origin differs from configuration"),
        Err(_) => {
            git(&path, &["remote", "add", "origin", &remote_url], None, 30).await?;
        }
    }
    let remote = git(&path, &["ls-remote", &remote_url, "refs/heads/main"], Some(token), 60).await?;
    let fields: Vec<_> = remote.split_whitespace().collect();
    ensure!(fields.len() == 2 && valid_sha(fields[0]) && fields[1] == "refs/heads/main", "unexpected remote HEAD");
    let wanted = fields[0];
    let local = git(&path, &["rev-parse", "HEAD"], None, 20).await.ok();
    let previous = source_sha(name)?;
    if local.as_deref() == Some(wanted) && previous.as_deref() == Some(wanted) {
        return Ok(());
    }
    // Invalidate before changing any working-tree files: a crash cannot leave
    // an old successful marker attached to a partially updated checkout.
    remove_if_exists(&marker(name))?;
    git(&path, &["fetch", "--no-tags", "--depth=1", &remote_url,
        "+refs/heads/main:refs/remotes/origin/main"], Some(token), 900).await?;
    git(&path, &["checkout", "-B", "main", "refs/remotes/origin/main"], None, 300).await?;
    git(&path, &["reset", "--hard", "refs/remotes/origin/main"], None, 300).await?;
    git(&path, &["clean", "-fdx"], None, 300).await?;
    ensure!(git(&path, &["rev-parse", "HEAD"], None, 20).await? == wanted, "remote changed during synchronization");
    atomic_json(&marker(name), &json!({"sha": wanted, "synced_at": Utc::now().to_rfc3339()}))?;
    println!("{}", json!({"event":"repo_synced", "repo":name, "sha":wanted}));
    Ok(())
}

pub async fn run(config: Config) -> Result<()> {
    let owner = env::var("GITHUB_OWNER").context("GITHUB_OWNER must be set")?;
    ensure!(safe_name(&owner) && !owner.contains('.') && !owner.contains('_'), "invalid GitHub owner");
    let interval = seconds("SYNC_INTERVAL_SECONDS", 120)?;
    let names = config.names();
    no_symlink(&Path::new(ROOT).join(".sync"))?;
    fs::create_dir_all(Path::new(ROOT).join(".sync"))?;
    let client = Client::builder().timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("codebase-memory-server").build()?;
    let mut token: Option<Token> = None;
    loop {
        for name in &names {
            if token.as_ref().is_none_or(|t| t.expires_at.timestamp() <= Utc::now().timestamp() + 300) {
                match installation_token(&client, &names).await {
                    Ok(value) => token = Some(value),
                    Err(_) => {
                        // Do not emit credentials or token-response bodies.
                        println!("{}", json!({"event":"github_app_auth_failed"}));
                        token = None;
                        break;
                    }
                }
            }
            if let Some(value) = &token {
                if sync_one(name, &owner, &value.token).await.is_err() {
                    log("repo_sync_failed", name);
                }
            }
        }
        tokio::time::sleep(interval).await;
    }
}
