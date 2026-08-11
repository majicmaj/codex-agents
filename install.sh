#!/bin/sh
set -eu

repository="${CODEX_AGENTS_REPOSITORY:-majicmaj/codex-agents}"
install_dir="${CODEX_AGENTS_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Darwin) platform="darwin" ;;
  Linux) platform="linux" ;;
  *) echo "codex-agents: unsupported operating system: $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  arm64|aarch64) architecture="arm64" ;;
  x86_64|amd64) architecture="amd64" ;;
  *) echo "codex-agents: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

asset="codex-agents_${platform}_${architecture}"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/codex-agents-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  gh release download --repo "$repository" --pattern "$asset" --pattern SHA256SUMS --dir "$temporary"
else
  release_url="https://github.com/$repository/releases/latest/download"
  if ! curl -fL --retry 3 --connect-timeout 10 "$release_url/$asset" -o "$temporary/$asset" ||
     ! curl -fL --retry 3 --connect-timeout 10 "$release_url/SHA256SUMS" -o "$temporary/SHA256SUMS"; then
    echo "codex-agents: download failed. For a private repository, install GitHub CLI and run: gh auth login" >&2
    exit 1
  fi
fi

(
  cd "$temporary"
  checksum_line="$(grep "  $asset\$" SHA256SUMS || true)"
  if [ -z "$checksum_line" ]; then
    echo "codex-agents: release checksum is missing for $asset" >&2
    exit 1
  fi
  if command -v shasum >/dev/null 2>&1; then
    printf '%s\n' "$checksum_line" | shasum -a 256 -c -
  elif command -v sha256sum >/dev/null 2>&1; then
    printf '%s\n' "$checksum_line" | sha256sum -c -
  else
    echo "codex-agents: shasum or sha256sum is required" >&2
    exit 1
  fi
)

mkdir -p "$install_dir"
install -m 0755 "$temporary/$asset" "$install_dir/codex-agents"

echo "installed codex-agents in $install_dir"
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    echo "add this directory to PATH:"
    echo "  export PATH=\"$install_dir:\$PATH\""
    ;;
esac
echo "run: codex-agents"

