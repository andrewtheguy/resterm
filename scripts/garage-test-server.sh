#!/usr/bin/env bash

set -euo pipefail

GARAGE_VERSION="2.3.0"
# SHA-256 of the official v2.3.0 x86_64-unknown-linux-musl garage binary.
# Garage publishes no separate checksum file, so this is vendored from a
# known-good download (verified `garage --version` reports v2.3.0) to detect
# a corrupted or tampered fetch.
GARAGE_SHA256="f98d317942bb341151a2775162016bb50cf86b865d0108de03eb5db16e2120cd"
GARAGE_S3_PORT="${GARAGE_S3_PORT:-3900}"
GARAGE_RPC_PORT="${GARAGE_RPC_PORT:-3901}"
GARAGE_ACCESS_KEY="GK22222222222222222222222222222222"
GARAGE_SECRET_KEY="3333333333333333333333333333333333333333333333333333333333333333"
GARAGE_BUCKET="wrustic-it"

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime="${project_root}/tmp/garage-e2e/runtime"
garage_config="${runtime}/garage.toml"
garage_binary="${GARAGE_BIN:-${project_root}/tmp/tools/garage-${GARAGE_VERSION}}"

fail() {
    printf '[garage-server] ERROR: %s\n' "$*" >&2
    exit 1
}

[[ "$GARAGE_S3_PORT" =~ ^[0-9]+$ ]] &&
    (( GARAGE_S3_PORT >= 1 && GARAGE_S3_PORT <= 65535 )) ||
    fail "GARAGE_S3_PORT must be an integer from 1 through 65535"
[[ "$GARAGE_RPC_PORT" =~ ^[0-9]+$ ]] &&
    (( GARAGE_RPC_PORT >= 1 && GARAGE_RPC_PORT <= 65535 )) ||
    fail "GARAGE_RPC_PORT must be an integer from 1 through 65535"

if [[ "${1:-}" == "--reset" ]]; then
    # This is an intentionally disposable path under the project-local tmp directory.
    rm -rf "$runtime"
elif (( $# != 0 )); then
    fail "usage: $0 [--reset]"
fi

if [[ ! -x "$garage_binary" ]]; then
    [[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" ]] ||
        fail "set GARAGE_BIN to a Garage v${GARAGE_VERSION} binary"
    command -v curl >/dev/null 2>&1 || fail "curl is required to download Garage"
    command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required to verify Garage"
    mkdir -p "$(dirname "$garage_binary")"
    printf '[garage-server] downloading Garage v%s\n' "$GARAGE_VERSION"
    curl --fail --location --silent --show-error \
        --output "${garage_binary}.download" \
        "https://garagehq.deuxfleurs.fr/_releases/v${GARAGE_VERSION}/x86_64-unknown-linux-musl/garage"
    actual_sha256="$(sha256sum "${garage_binary}.download" | awk '{ print $1 }')"
    if [[ "$actual_sha256" != "$GARAGE_SHA256" ]]; then
        rm -f "${garage_binary}.download"
        fail "Garage download checksum mismatch: expected ${GARAGE_SHA256}, got ${actual_sha256}"
    fi
    chmod +x "${garage_binary}.download"
    mv "${garage_binary}.download" "$garage_binary"
fi

mkdir -p "${runtime}/meta" "${runtime}/data"
cat >"$garage_config" <<EOF
metadata_dir = "${runtime}/meta"
data_dir = "${runtime}/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "127.0.0.1:${GARAGE_RPC_PORT}"
rpc_bind_outgoing = false
rpc_secret = "1111111111111111111111111111111111111111111111111111111111111111"

[s3_api]
s3_region = "garage"
api_bind_addr = "127.0.0.1:${GARAGE_S3_PORT}"
root_domain = ".s3.garage.localhost"
EOF

printf '[garage-server] S3 endpoint: http://127.0.0.1:%s\n' "$GARAGE_S3_PORT"
printf '[garage-server] RPC endpoint: 127.0.0.1:%s\n' "$GARAGE_RPC_PORT"
printf '[garage-server] press Ctrl-C to stop Garage\n'
exec env \
    GARAGE_CONFIG_FILE="$garage_config" \
    GARAGE_DEFAULT_ACCESS_KEY="$GARAGE_ACCESS_KEY" \
    GARAGE_DEFAULT_SECRET_KEY="$GARAGE_SECRET_KEY" \
    GARAGE_DEFAULT_BUCKET="$GARAGE_BUCKET" \
    "$garage_binary" server --single-node --default-bucket
