# Maintainer guide

## Container images

Images are published to Docker Hub from the main branch after the native AMD64 and ARM64 image smoke tests. The `latest` and `sha-<commit>` tags are multi-platform manifests; `sha-<commit>-amd64` and `sha-<commit>-arm64` identify the tested architecture images. Prefer the commit tag or digest for reproducible deployments.

To configure publishing, create a public `codebase-memory-server` repository in the intended Docker Hub namespace. Make these GitHub Actions secrets available to this repository (organization-level or repository-level):

- Secret `DOCKER_HUB_USERNAME`: the Docker Hub account or organization used to authenticate.
- Secret `DOCKER_HUB_TOKEN`: a Docker Hub access token with permission to push to that repository.

By default, the image namespace is `DOCKER_HUB_USERNAME`. If the Docker Hub repository owner differs from the login account, set the optional Actions variable `DOCKERHUB_NAMESPACE` to that owner. For organization secrets, explicitly grant this public GitHub repository access; a private-repositories-only policy will not work.

Pull the image using `docker pull <namespace>/codebase-memory-server:latest`, replacing `<namespace>` with the Docker Hub repository owner. Pull requests build and test without registry credentials and do not publish images.

## Build and test

```bash
cargo test --locked
docker build -t codebase-memory-server:local .
```

On the Linux CI runner, run `bash tests/smoke.sh codebase-memory-server:local` to verify the actual MCP endpoint, catalog isolation and read-only tool policy. Smoke tests require Docker, Git, curl, jq and sudo.

The upstream binary is fetched for the target Linux architecture and verified against its release checksum manifest.
