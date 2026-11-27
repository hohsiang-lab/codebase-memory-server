# Codebase Memory Server

A Rust service that hosts the upstream [Codebase Memory MCP](https://github.com/DeusData/codebase-memory-mcp) engine for teams. It uses the existing engine rather than reimplementing indexing or the graph database.

**Development preview. Do not expose it to untrusted clients or deploy private repositories until CI and deployment-specific access controls have been verified.**

## Run

The same image has two modes:

- `codebase-memory-server sync`: GitHub App installation-token synchronization of selected main branches. Mount the GitHub App credentials only here.
- `codebase-memory-server serve`: serialized indexing, catalog reconciliation, and a read-only Streamable HTTP MCP endpoint at `/mcp`; readiness endpoint at `/healthz`.

Both need the same repository volume at `/repos` and the same configuration file (see [repos.example.json](repos.example.json)). Mount the repository volume read-only in the server. The server alone needs a writable dedicated graph cache at `/data/cbm`. Keep UID/GID 10001 for both processes. Use one server replica per cache. The runtime directory must be private (0700), with non-world-writable ancestors, and should live on ephemeral storage for Kubernetes.

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

## Container images

Images are published to Docker Hub from the main branch after the native AMD64 and ARM64 image smoke tests. The `latest` and `sha-<commit>` tags are multi-platform manifests; `sha-<commit>-amd64` and `sha-<commit>-arm64` identify the tested architecture images. Prefer the commit tag or digest for reproducible deployments.

To configure publishing, create a public `codebase-memory-server` repository in the intended Docker Hub namespace. Make these GitHub Actions secrets available to this repository (organization-level or repository-level):

- Secret `DOCKER_HUB_USERNAME`: the Docker Hub account or organization used to authenticate.
- Secret `DOCKER_HUB_TOKEN`: a Docker Hub access token with permission to push to that repository.

By default, the image namespace is `DOCKER_HUB_USERNAME`. If the Docker Hub repository owner differs from the login account, set the optional Actions variable `DOCKERHUB_NAMESPACE` to that owner. For organization secrets, explicitly grant this public GitHub repository access; a private-repositories-only policy will not work.

Pull the image using `docker pull <namespace>/codebase-memory-server:latest`, replacing `<namespace>` with the Docker Hub repository owner. Pull requests build and test without registry credentials and do not publish images.

## Build

Run `cargo test` or build the container with Docker Buildx. The upstream binary is fetched for the target Linux architecture and checked against its release checksum manifest. The upstream engine remains MIT-licensed; its license is included in the image.

This project is MIT-licensed.
