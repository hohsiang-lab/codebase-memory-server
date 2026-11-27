#!/usr/bin/env bash
set -euo pipefail
image=$1
work=$(mktemp -d)
volume=$(docker volume create)
container="cbm-smoke-$RANDOM"
cleanup() {
  docker logs "$container" 2>/dev/null || true
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
  sudo rm -rf "$work"
}
trap cleanup EXIT

# Only synthetic local code; no credentials or private repositories in CI.
mkdir -p "$work/repos/.sync" "$work/repos/.locks"
for name in example removed; do
  mkdir "$work/repos/$name"
  git -C "$work/repos/$name" init -q -b main
  printf 'def hello():\n    return "%s"\n' "$name" > "$work/repos/$name/sample.py"
  git -C "$work/repos/$name" add sample.py
  git -C "$work/repos/$name" -c user.name=CI -c user.email=ci@example.invalid commit -qm fixture
  sha=$(git -C "$work/repos/$name" rev-parse HEAD)
  printf '{"sha":"%s"}\n' "$sha" > "$work/repos/.sync/$name.json"
  touch "$work/repos/.locks/$name.lock"
done
sudo chown -R 10001:10001 "$work/repos"
printf '{"repositories":[{"name":"example","branch":"main"}],"cross_repo":{}}\n' > "$work/repos.json"

mounts=(-v "$work/repos:/repos:ro" -v "$volume:/data/cbm" -e CBM_RUNTIME_DIR=/data/cbm/runtime)
docker run --rm "${mounts[@]}" --entrypoint sh "$image" -ec '
  mkdir -p "$CBM_RUNTIME_DIR"
  chmod 700 "$CBM_RUNTIME_DIR"
  for name in example removed; do
    printf "{\"repo_path\":\"/repos/%s\",\"name\":\"%s\",\"mode\":\"full\",\"persistence\":false}" "$name" "$name" >/tmp/index.json
    codebase-memory-mcp cli --quiet index_repository --args-file /tmp/index.json --format json
  done
'
docker run -d --name "$container" -p 127.0.0.1::8000 "${mounts[@]}" \
  -v "$work/repos.json:/etc/codebase/repos.json:ro" "$image" serve
port=$(docker port "$container" 8000/tcp | cut -d: -f2)
url="http://127.0.0.1:$port"
for i in $(seq 1 120); do
  if curl -fsS "$url/healthz" >/dev/null 2>&1; then break; fi
  sleep 2
done
curl -fsS "$url/healthz"
decode() {
  if grep -q '^data: ' "$1"; then
    sed -n 's/^data: //p' "$1" | tail -n1
  else
    cat "$1"
  fi
}
curl -fsS --max-time 30 -D "$work/headers" -o "$work/body" \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}' "$url/mcp"
decode "$work/body" | jq -e '.result.capabilities.tools != null'
session=$(awk 'tolower($1)=="mcp-session-id:" {gsub(/\r/,"",$2);print $2}' "$work/headers")
test -n "$session"
rpc() {
  curl -sS --max-time 30 -o "$work/body" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    -H "Mcp-Session-Id: $session" -H 'MCP-Protocol-Version: 2024-11-05' \
    -d "$1" "$url/mcp"
  decode "$work/body"
}
rpc '{"jsonrpc":"2.0","method":"notifications/initialized"}' >/dev/null
rpc '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | jq -e '
  [.result.tools[].name] | length == 13 and
  (index("search_code") != null) and (index("delete_project") == null) and
  (index("index_repository") == null) and (index("manage_adr") == null)'
rpc '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_projects","arguments":{"format":"json"}}}' | jq -e '
  (.result.structuredContent // (.result.content[0].text | fromjson)) |
  [.projects[].name] == ["example"]'
rpc '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search_code","arguments":{"project":"example","pattern":"hello"}}}' | jq -e '
  .result != null and (.result.isError != true)'
for name in delete_project index_repository manage_adr; do
  rpc "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"$name\",\"arguments\":{\"project\":\"example\"}}}" | jq -e '.error != null or .result.isError == true'
done
rpc '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"search_code","arguments":{"project":"removed","pattern":"hello"}}}' | jq -e '.error != null or .result.isError == true'
echo "MCP smoke passed: approved project searchable; stale project and mutation tools denied."
