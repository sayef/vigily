#!/bin/sh
# Install the `vigily` binary from a GitHub release, without building it.
#
#   curl -LsSf https://raw.githubusercontent.com/sayef/vigily/main/scripts/install.sh | sh
#
# Environment:
#   VIGILY_VERSION       release tag to install, such as v0.1.0-beta.1 (default: the latest release)
#   VIGILY_INSTALL_DIR   where to put the binary (default: $HOME/.local/bin)
#   VIGILY_DOWNLOAD_BASE where the release files are (default: the GitHub release); for testing
#
# The archive's SHA-256 is checked against the release's checksum file before anything is
# installed. Nothing runs with sudo.
set -eu

REPO="sayef/vigily"

say() { printf 'vigily: %s\n' "$*"; }
fail() { printf 'vigily: %s\n' "$*" >&2; exit 1; }

download() { # url file
  if command -v curl >/dev/null 2>&1; then
    curl --proto "$protocols" -fsSL "$1" -o "$2" || fail "could not download $1"
  elif command -v wget >/dev/null 2>&1; then
    if [ "$protocols" = "=https" ]; then
      wget -q --https-only "$1" -O "$2" || fail "could not download $1"
    else
      wget -q "$1" -O "$2" || fail "could not download $1"
    fi
  else
    fail "needs curl or wget"
  fi
}

sha256() { # file
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    fail "needs sha256sum or shasum to check the download"
  fi
}

main() {
  version="${VIGILY_VERSION:-latest}"
  install_dir="${VIGILY_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}"

  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux) os_part="unknown-linux-gnu" ;;
    Darwin) os_part="apple-darwin" ;;
    *) fail "no prebuilt binary for $os; build from source instead (see the getting-started guide)" ;;
  esac
  case "$arch" in
    x86_64 | amd64) arch_part="x86_64" ;;
    arm64 | aarch64) arch_part="aarch64" ;;
    *) fail "no prebuilt binary for $os on $arch; build from source instead (see the getting-started guide)" ;;
  esac
  target="$arch_part-$os_part"

  # HTTPS only, including redirects; plain HTTP only for a test server given explicitly.
  protocols="=https"
  if [ -n "${VIGILY_DOWNLOAD_BASE:-}" ]; then
    base="$VIGILY_DOWNLOAD_BASE"
    protocols="=https,http"
  elif [ "$version" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$version"
  fi
  asset="vigily-$target.tar.gz"

  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT INT TERM

  say "downloading $asset ($version) from $base"
  download "$base/$asset" "$tmp/$asset"
  download "$base/$asset.sha256" "$tmp/$asset.sha256"

  want="$(cut -d' ' -f1 "$tmp/$asset.sha256")"
  got="$(sha256 "$tmp/$asset")"
  if [ -z "$want" ] || [ "$want" != "$got" ]; then
    fail "checksum mismatch for $asset (expected ${want:-nothing}, got $got); nothing was installed"
  fi

  tar -C "$tmp" -xzf "$tmp/$asset" vigily || fail "$asset does not contain a vigily binary"
  mkdir -p "$install_dir" || fail "could not create $install_dir"
  # Move a new file into place rather than overwriting: macOS kills a binary rewritten in place.
  cp "$tmp/vigily" "$install_dir/vigily.new"
  chmod 755 "$install_dir/vigily.new"
  mv -f "$install_dir/vigily.new" "$install_dir/vigily"

  say "installed $("$install_dir/vigily" --version) to $install_dir/vigily"
  case ":${PATH:-}:" in
    *":$install_dir:"*) ;;
    *)
      say "$install_dir is not on your PATH; add it, for example:"
      # Printed for the user to run, so the variable stays unexpanded.
      # shellcheck disable=SC2016
      printf '  echo '\''export PATH="%s:$PATH"'\'' >> ~/.profile\n' "$install_dir"
      ;;
  esac
  say "next: vigily up, then open http://127.0.0.1:7600"
}

# Everything runs from main, so a download cut short cannot run half a script.
main "$@"
