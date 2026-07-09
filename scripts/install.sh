#!/usr/bin/env bash
set -euo pipefail

repo="laulauland/pando"
bin_dir="${BIN_DIR:-/usr/local/bin}"
version="${PANDO_VERSION:-latest}"
install_completions="${INSTALL_COMPLETIONS:-1}"

usage() {
  cat <<'EOF'
Install pando from GitHub Releases.

Usage:
  curl -fsSL https://raw.githubusercontent.com/laulauland/pando/main/scripts/install.sh | bash

Environment:
  PANDO_VERSION         Version to install, e.g. 0.2.0 or v0.2.0 (default: latest)
  BIN_DIR               Install directory (default: /usr/local/bin)
  INSTALL_COMPLETIONS   Install shell completions: 1 or 0 (default: 1)
  BASH_COMPLETION_DIR   Bash completion directory
  ZSH_COMPLETION_DIR    Zsh completion directory
  FISH_COMPLETION_DIR   Fish completion directory
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

install_completion_file() {
  local shell="$1"
  local bin="$2"
  local dir="$3"
  local file="$4"
  local binary_path="${bin_dir}/${bin}"
  local completion_tmp="${tmp}/completions/${shell}/${file}"

  mkdir -p "${dir}" "$(dirname "${completion_tmp}")"
  "${binary_path}" completions "${shell}" >"${completion_tmp}"
  install -m 0644 "${completion_tmp}" "${dir}/${file}"
}

if [ "${install_completions}" != "0" ]; then
  xdg_data_home="${XDG_DATA_HOME:-${HOME}/.local/share}"
  xdg_config_home="${XDG_CONFIG_HOME:-${HOME}/.config}"
  bash_completion_dir="${BASH_COMPLETION_DIR:-${xdg_data_home}/bash-completion/completions}"
  zsh_completion_dir="${ZSH_COMPLETION_DIR:-${ZDOTDIR:-${HOME}}/.zsh/completions}"
  fish_completion_dir="${FISH_COMPLETION_DIR:-${xdg_config_home}/fish/completions}"

  for bin in pando pd; do
    install_completion_file bash "${bin}" "${bash_completion_dir}" "${bin}"
    install_completion_file zsh "${bin}" "${zsh_completion_dir}" "_${bin}"
    install_completion_file fish "${bin}" "${fish_completion_dir}" "${bin}.fish"
  done

  echo "Installed shell completions to:" >&2
  echo "  bash: ${bash_completion_dir}" >&2
  echo "  zsh:  ${zsh_completion_dir}" >&2
  echo "  fish: ${fish_completion_dir}" >&2
fi

echo "Installed pando and pd to ${bin_dir}" >&2
