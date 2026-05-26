#!/usr/bin/env bash
set -euo pipefail

repo="laulauland/pando"
bin_dir="${BIN_DIR:-/usr/local/bin}"
version="${PANDO_VERSION:-latest}"

usage() {
  cat <<'EOF'
Install pando from GitHub Releases.

Usage:
  curl -fsSL https://raw.githubusercontent.com/laulauland/pando/main/scripts/install.sh | bash

Environment:
  PANDO_VERSION  Version to install, e.g. 0.2.0 or v0.2.0 (default: latest)
  BIN_DIR        Install directory (default: /usr/local/bin)
EOF
}

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 1
  fi
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
esac

need curl
need tar
need mktemp

os="$(uname -s)"
arch="$(uname -m)"

case "${os}:${arch}" in
  Darwin:arm64|Darwin:aarch64)
    bottle_tag="arm64_sequoia"
    ;;
  Linux:x86_64|Linux:amd64)
    bottle_tag="x86_64_linux"
    ;;
  *)
    echo "error: unsupported platform: ${os} ${arch}" >&2
    echo "supported release artifacts: macOS arm64, Linux x86_64" >&2
    exit 1
    ;;
esac

if [ "${version}" = "latest" ]; then
  tag="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${repo}/releases/latest" | sed 's#.*/##')"
else
  tag="${version}"
  case "${tag}" in
    v*) ;;
    *) tag="v${tag}" ;;
  esac
fi

plain_version="${tag#v}"
archive="pando-${plain_version}.${bottle_tag}.bottle.tar.gz"
url="https://github.com/${repo}/releases/download/${tag}/${archive}"
tmp="$(mktemp -d)"

cleanup() {
  rm -rf "${tmp}"
}
trap cleanup EXIT

echo "Downloading ${url}" >&2
curl -fL "${url}" -o "${tmp}/${archive}"
tar -xzf "${tmp}/${archive}" -C "${tmp}"

if ! mkdir -p "${bin_dir}" 2>/dev/null; then
  need sudo
  sudo mkdir -p "${bin_dir}"
fi

install_cmd=(install -m 0755)
if [ ! -w "${bin_dir}" ]; then
  need sudo
  install_cmd=(sudo "${install_cmd[@]}")
fi

for bin in pando pd; do
  src="${tmp}/pando/${plain_version}/bin/${bin}"
  dest="${bin_dir}/${bin}"
  "${install_cmd[@]}" "${src}" "${dest}"
done

echo "Installed pando and pd to ${bin_dir}" >&2
