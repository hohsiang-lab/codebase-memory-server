# Codebase Memory Server

A self-hosted Rust service that keeps GitHub repositories indexed for shared, read-only code exploration over MCP. Built on [Codebase Memory MCP](https://github.com/DeusData/codebase-memory-mcp).

- GitHub App synchronization and automatic indexing of selected repositories.
- Code search, graph exploration and optional cross-repository analysis.
- Streamable HTTP MCP endpoint; Linux AMD64 and ARM64 images.

**Development preview.** Real GitHub App deployment has not yet been validated. The MCP endpoint has no built-in authentication or per-user authorization. Every permitted client can read every configured repository. Keep it on a trusted private network or behind an authenticated, authorized gateway.

## Quick start

Pull the image from [Docker Hub](https://hub.docker.com/r/devhohing/codebase-memory-server) and get the example configuration:

```sh
git clone https://github.com/hohsiang-lab/codebase-memory-server.git
cd codebase-memory-server
docker pull devhohing/codebase-memory-server:latest
cp repos.example.json repos.json
```

Edit [repos.example.json](repos.example.json)'s local copy, `repos.json`, to select repositories in one GitHub account or organization. Only the `main` branch is supported.

Create a GitHub App with **Contents: read**, installed only on the selected repositories. In a private directory outside the source checkout, save three files: `app_id`, `installation_id`, and `private_key` (RSA PEM). The files and directory must be readable by container UID/GID 10001, without access by unrelated users.

Run the same image in two modes. Replace the example owner and credential path:

```sh
IMAGE=devhohing/codebase-memory-server:latest
GITHUB_OWNER=your-github-owner
GITHUB_APP_DIR=/absolute/path/to/github-app

docker run -d --name cbm-sync \
  -e GITHUB_OWNER="$GITHUB_OWNER" \
  -v cbm-repos:/repos \
  -v "$PWD/repos.json:/etc/codebase/repos.json:ro" \
  -v "$GITHUB_APP_DIR:/run/secrets/github:ro" \
  "$IMAGE" sync

docker run -d --name cbm-server \
  -p 127.0.0.1:8000:8000 \
  -v cbm-repos:/repos:ro \
  -v cbm-cache:/data/cbm \
  -v "$PWD/repos.json:/etc/codebase/repos.json:ro" \
  "$IMAGE" serve
```

Connect a Streamable HTTP MCP client to `http://127.0.0.1:8000/mcp`. Once the server starts, `curl --fail http://127.0.0.1:8000/healthz` checks readiness; initial indexing continues asynchronously.

## Configuration

| Environment variable | Default |
| --- | --- |
| `GITHUB_OWNER` (sync only) | Required |
| `GITHUB_APP_SECRET_DIR` (sync only) | `/run/secrets/github` |
| `REPOS_CONFIG` | `/etc/codebase/repos.json` |
| `SYNC_INTERVAL_SECONDS` | `120` |
| `INDEX_POLL_SECONDS` | `15` |
| `INDEX_TIMEOUT_SECONDS` | `5400` |
| `CBM_CACHE_DIR` | `/data/cbm` |
| `CBM_RUNTIME_DIR` | `/data/cbm/runtime` |
| `LISTEN_ADDR` | `0.0.0.0:8000` in Docker |

The optional `cross_repo` object maps repository names to lists of other configured repository names that may be linked. Restart both processes after changing the configuration.

## Deployment and security

- Mount GitHub App credentials only into `sync`; it requests short-lived tokens restricted to the configured repositories.
- Both processes use UID/GID 10001. The configuration must be readable by that user. Mount `/repos` read-only in `serve` and give it a separate writable graph cache. Use a private runtime directory (0700) with non-world-writable ancestors; use ephemeral runtime storage on Kubernetes.
- Use dedicated checkout and cache volumes for each installation/owner and one server replica per cache.
- Removing a repository or cross-repository permission, or starting with a legacy cache of unknown policy, resets derived graphs before serving. Only approved repositories are rebuilt; existing checkouts remain untouched. Additions alone do not reset the catalog. Back up legacy caches and never share this cache with user-authored graph data (such as ADRs).
- Mutation tools are not exposed over MCP. The server and upstream engine enforce read-only analysis; committed graph artifacts are not accepted.

## Development

Run `cargo test --locked` or build the Docker image locally with `docker build -t devhohing/codebase-memory-server:latest .` (also works before the first image is published). CI tests both architectures and runs `tests/smoke.sh` before publishing to Docker Hub from `main`. The upstream engine is checksum-verified during the build.

## License

[MIT](LICENSE). The upstream engine is also MIT-licensed; its license is included in the container image.
