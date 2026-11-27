# Configuration and deployment

## Prerequisites

- Docker on Linux AMD64 or ARM64 (or Docker Desktop).
- A GitHub App installed on the selected repositories with Contents: read.
- A `repos.json` configuration file based on [repos.example.json](../repos.example.json). Currently only the `main` branch is supported.

## Run with Docker

Use the locally built image from the [README](../README.md), or replace `IMAGE` with the published Docker Hub image.

Create a private directory named `github-app` with these files:

| File | Contents |
| --- | --- |
| `app_id` | GitHub App ID |
| `installation_id` | Installation ID |
| `private_key` | GitHub App RSA private key in PEM format |

Ensure that the directory and files are readable by container UID/GID 10001 without granting access to unrelated users. Keep this directory and its contents out of version control (prefer a directory outside the source checkout). Mount these credentials only into the sync worker. Set `GITHUB_OWNER` to the GitHub account or organization that owns the selected repositories.

From the project directory (replace the example owner and credential directory path):

```bash
IMAGE=codebase-memory-server:local
GITHUB_OWNER=your-github-owner
GITHUB_APP_DIR=/absolute/path/to/github-app

docker volume create cbm-repos
docker volume create cbm-cache

docker run -d --name cbm-sync \
  -e GITHUB_OWNER="$GITHUB_OWNER" \
  --mount type=volume,src=cbm-repos,dst=/repos \
  --mount type=bind,src="$PWD/repos.json",dst=/etc/codebase/repos.json,readonly \
  --mount type=bind,src="$GITHUB_APP_DIR",dst=/run/secrets/github,readonly \
  "$IMAGE" sync

docker run -d --name cbm-server \
  -p 127.0.0.1:8000:8000 \
  --mount type=volume,src=cbm-repos,dst=/repos,readonly \
  --mount type=volume,src=cbm-cache,dst=/data/cbm \
  --mount type=bind,src="$PWD/repos.json",dst=/etc/codebase/repos.json,readonly \
  "$IMAGE" serve

curl --fail http://127.0.0.1:8000/healthz
```

Ensure the configuration file is readable by UID/GID 10001. Use a fresh cache and checkout volume for each installation/owner. The example binds the MCP endpoint to the host loopback interface; it does not expose the service publicly.

The readiness endpoint confirms that the server is running, not that all repositories have finished indexing.

## Runtime settings

The same image has two modes:

- `codebase-memory-server sync`: GitHub App installation-token synchronization of selected main branches. Mount the GitHub App credentials only here.
- `codebase-memory-server serve`: serialized indexing, catalog reconciliation, and a read-only Streamable HTTP MCP endpoint at `/mcp`; readiness endpoint at `/healthz`.

Both need the same repository volume at `/repos` and the same configuration file (see [repos.example.json](../repos.example.json)). Mount the repository volume read-only in the server. The server alone needs a writable dedicated graph cache at `/data/cbm`. Keep UID/GID 10001 for both processes. Use one server replica per cache. The runtime directory must be private (0700), with non-world-writable ancestors, and should live on ephemeral storage for Kubernetes.

| Setting | Default |
| --- | --- |
| GITHUB_OWNER (sync only) | Required, GitHub organization or account |
| GITHUB_APP_SECRET_DIR (sync only) | /run/secrets/github (app_id, installation_id, private_key files) |
| REPOS_CONFIG | /etc/codebase/repos.json |
| SYNC_INTERVAL_SECONDS | 120 |
| INDEX_POLL_SECONDS | 15 |
| INDEX_TIMEOUT_SECONDS | 5400 |
| CBM_CACHE_DIR | /data/cbm |
| CBM_RUNTIME_DIR | /data/cbm/runtime |
| LISTEN_ADDR | 127.0.0.1:8000 (container sets 0.0.0.0:8000) |

Use a GitHub App with Contents: read on the selected repositories. The service requests a short-lived token restricted to the configured names. Do not store credentials or real private repository lists in this repository. Use a separate dedicated cache and checkout volume for each installation/owner.

Configuration is loaded on process startup; restart both modes when it changes. Removing a repository or cross-repository permission resets derived graph generations before the HTTP endpoint starts; the worker rebuilds only approved repositories afterwards. The first start against a legacy cache with unknown policy also resets its derived graphs. Additions alone do not reset the catalog. Existing checkouts remain untouched. Committed graph artifacts are not accepted. Use a dedicated cache for this read-only service, not a cache containing user-authored graph data such as ADRs; back up the cache before migration.

## Security boundary

The HTTP endpoint itself has no authentication or per-user authorization. Put it behind a trusted private network or an authenticated gateway. Every permitted client can read every configured repository. Never publish this endpoint to the internet without appropriate authentication and authorization.

The MCP proxy forwards only the upstream analysis tools and requires canonical configured project names for project queries; mutation tools remain local to the index worker. The upstream binary independently runs with the analysis tool profile.

