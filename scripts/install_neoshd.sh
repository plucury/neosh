#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-plucury/neosh}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
BIN_NAME="neoshd"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 1
  fi
}

detect_os() {
  local os
  os="$(uname -s)"
  case "$os" in
    Linux) echo "linux" ;;
    Darwin) echo "macos" ;;
    *)
      echo "error: unsupported OS: $os (supported: linux, macos)" >&2
      exit 1
      ;;
  esac
}

detect_arch() {
  local arch
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) echo "x86_64" ;;
    arm64|aarch64) echo "arm64" ;;
    *)
      echo "error: unsupported architecture: $arch (supported: x86_64, arm64)" >&2
      exit 1
      ;;
  esac
}

http_get() {
  local url="$1"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "$url"
  else
    echo "error: neither curl nor wget is available" >&2
    exit 1
  fi
}

download_to() {
  local url="$1"
  local out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fL "$url" -o "$out"
  else
    wget -qO "$out" "$url"
  fi
}

main() {
  need_cmd uname
  need_cmd install
  need_cmd mktemp
  need_cmd chmod
  need_cmd grep
  need_cmd sed

  local os arch asset release_api release_json tag download_url tmpdir tmpbin
  os="$(detect_os)"
  arch="$(detect_arch)"
  asset="${BIN_NAME}-${os}-${arch}"
  release_api="https://api.github.com/repos/${REPO}/releases/latest"

  echo "Resolving latest release from ${REPO} ..."
  release_json="$(http_get "$release_api")"

  tag="$(printf '%s' "$release_json" \
    | grep -m1 '"tag_name":' \
    | sed -E 's/.*"tag_name":[[:space:]]*"([^"]+)".*/\1/')"
  if [[ -z "${tag:-}" ]]; then
    echo "error: failed to parse tag_name from GitHub release API" >&2
    exit 1
  fi

  download_url="$(printf '%s' "$release_json" \
    | grep -Eo 'https://[^"]+/neoshd-[^"]+' \
    | grep -E "/${asset}$" \
    | head -n1 || true)"
  if [[ -z "${download_url:-}" ]]; then
    echo "error: no release asset found for ${asset} in ${tag}" >&2
    echo "hint: check https://github.com/${REPO}/releases/tag/${tag}" >&2
    exit 1
  fi

  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT
  tmpbin="${tmpdir}/${BIN_NAME}"

  echo "Downloading ${asset} (${tag}) ..."
  download_to "$download_url" "$tmpbin"
  chmod 0755 "$tmpbin"

  if [[ -w "$INSTALL_DIR" ]] || [[ ! -e "$INSTALL_DIR" && -w "$(dirname "$INSTALL_DIR")" ]]; then
    mkdir -p "$INSTALL_DIR"
    install -m 0755 "$tmpbin" "${INSTALL_DIR}/${BIN_NAME}"
  else
    echo "Installing to ${INSTALL_DIR} requires elevated privileges (sudo)."
    sudo mkdir -p "$INSTALL_DIR"
    sudo install -m 0755 "$tmpbin" "${INSTALL_DIR}/${BIN_NAME}"
  fi

  echo "Installed ${BIN_NAME} to ${INSTALL_DIR}/${BIN_NAME}"
  "${INSTALL_DIR}/${BIN_NAME}" version || true
}

main "$@"
