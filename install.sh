#!/bin/sh
# llca installer: downloads a release, checks its SHA-256, installs one file.
#
#   curl -fsSL https://raw.githubusercontent.com/llca-ai/llca/main/install.sh | sh
#
# Settings (environment variables, all optional):
#   LLCA_VERSION      install this version, for example 0.8.0 (default: latest)
#   LLCA_INSTALL_DIR  install into this folder (default: ~/.local/bin)
#
# The script never uses sudo, never edits a shell file and never runs
# "llca init". It installs the binary and tells you what to do next.

set -eu

REPO="llca-ai/llca"
WORK_DIR=""
NEW_FILE=""

say() {
  printf '%s\n' "$*"
}

# Colours only when the stream is a terminal and NO_COLOR is not set.
paint() {
  # $1 = stream (1 or 2), $2 = colour code, $3 = text
  if [ -t "$1" ] && [ -z "${NO_COLOR:-}" ]; then
    printf '\033[%sm%s\033[0m' "$2" "$3"
  else
    printf '%s' "$3"
  fi
}

fail() {
  printf '%s %s\n' "$(paint 2 31 'error:')" "$*" >&2
  exit 1
}

warn() {
  printf '%s %s\n' "$(paint 2 33 'warning:')" "$*" >&2
}

cleanup() {
  if [ -n "$NEW_FILE" ]; then
    rm -f "$NEW_FILE"
  fi
  if [ -n "$WORK_DIR" ]; then
    rm -rf "$WORK_DIR"
  fi
}

have() {
  command -v "$1" >/dev/null 2>&1
}

detect_target() {
  os=$(uname -s)
  cpu=$(uname -m)
  case "$os" in
    Linux)
      case "$cpu" in
        x86_64 | amd64) TARGET="x86_64-unknown-linux-gnu" ;;
        aarch64 | arm64) TARGET="aarch64-unknown-linux-gnu" ;;
        *) fail "unsupported CPU: $cpu on Linux. llca supports x86_64 and aarch64." ;;
      esac
      ;;
    Darwin)
      case "$cpu" in
        arm64 | aarch64) TARGET="aarch64-apple-darwin" ;;
        x86_64) TARGET="x86_64-apple-darwin" ;;
        *) fail "unsupported CPU: $cpu on macOS. llca supports Apple silicon and Intel." ;;
      esac
      ;;
    *)
      fail "unsupported system: $os. llca supports Linux and macOS."
      ;;
  esac
  OS_NAME="$os"
}

# Sets VERSION (without a leading "v") and BASE (folder holding the files).
resolve_version() {
  version="${LLCA_VERSION:-}"
  version="${version#v}"
  if [ -n "${LLCA_BASE_URL:-}" ]; then
    # Test override: the files sit directly under $LLCA_BASE_URL/v<version>/.
    [ -n "$version" ] || fail "LLCA_BASE_URL needs LLCA_VERSION to be set."
    base_root="${LLCA_BASE_URL%/}"
  else
    base_root="https://github.com/$REPO/releases/download"
  fi

  if [ -z "$version" ]; then
    say "Looking up the latest release..."
    url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
      fail "could not look up the latest release. Check your connection, or set LLCA_VERSION."
    version="${url##*/}"
    version="${version#v}"
  fi

  case "$version" in
    "" | *[!0-9A-Za-z.-]*) fail "'$version' is not a valid version." ;;
  esac
  VERSION="$version"
  BASE="$base_root/v$VERSION"
}

download() {
  # $1 = url, $2 = output file
  curl -fsSL --retry 2 -o "$2" "$1" ||
    fail "could not download $1 (does this release exist for your system?)"
}

sha256_of() {
  if have sha256sum; then
    out=$(sha256sum "$1")
  elif have shasum; then
    out=$(shasum -a 256 "$1")
  else
    fail "neither sha256sum nor shasum was found, so the download cannot be verified."
  fi
  printf '%s' "${out%% *}"
}

main() {
  trap cleanup EXIT
  trap 'exit 1' HUP INT TERM

  have curl || fail "curl is required."
  have tar || fail "tar is required."
  have uname || fail "uname is required."

  detect_target
  resolve_version

  install_dir="${LLCA_INSTALL_DIR:-}"
  if [ -z "$install_dir" ]; then
    [ -n "${HOME:-}" ] || fail "HOME is not set. Set LLCA_INSTALL_DIR to choose a folder."
    install_dir="$HOME/.local/bin"
  fi

  name="llca-$VERSION-$TARGET"
  archive="$name.tar.gz"

  WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/llca-install.XXXXXX") ||
    fail "could not create a temporary folder."

  say "Installing llca $VERSION for $TARGET"
  download "$BASE/$archive" "$WORK_DIR/$archive"
  download "$BASE/$archive.sha256" "$WORK_DIR/$archive.sha256"

  # The .sha256 file reads "<hash>  <file name>"; only the hash matters here.
  IFS= read -r line <"$WORK_DIR/$archive.sha256" || true
  expected="${line%% *}"
  case "$expected" in
    *[!0-9a-f]*) fail "the checksum file for $archive is not valid." ;;
  esac
  [ "${#expected}" -eq 64 ] || fail "the checksum file for $archive is not valid."

  actual=$(sha256_of "$WORK_DIR/$archive")
  if [ "$actual" != "$expected" ]; then
    rm -f "$WORK_DIR/$archive"
    fail "SHA-256 mismatch for $archive (expected $expected, got $actual). Nothing was installed."
  fi
  say "SHA-256 verified."

  tar -xzf "$WORK_DIR/$archive" -C "$WORK_DIR" || fail "could not unpack $archive."
  [ -f "$WORK_DIR/$name/llca" ] || fail "$archive does not contain llca."

  mkdir -p "$install_dir" || fail "could not create $install_dir."
  # Write next to the final file, then rename: a running llca is never half replaced.
  NEW_FILE="$install_dir/.llca.new.$$"
  cp "$WORK_DIR/$name/llca" "$NEW_FILE" || fail "could not write to $install_dir."
  chmod 755 "$NEW_FILE"
  mv -f "$NEW_FILE" "$install_dir/llca" || fail "could not install llca into $install_dir."
  NEW_FILE=""
  say "Installed $install_dir/llca"

  if [ "$OS_NAME" = "Linux" ]; then
    if ! "$install_dir/llca" --version >/dev/null 2>&1; then
      warn "llca was installed but does not start. It needs the SQLite library. Install it with:"
      say "  Debian, Ubuntu:  sudo apt install libsqlite3-0"
      say "  Fedora:          sudo dnf install sqlite-libs"
      say "  Arch:            sudo pacman -S sqlite"
    fi
  fi

  case ":${PATH:-}:" in
    *":$install_dir:"*) ;;
    *)
      say ""
      say "$install_dir is not in your PATH. Add it with one of these, then open a new terminal:"
      say "  bash:  echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.bashrc"
      say "  zsh:   echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.zshrc"
      ;;
  esac

  say ""
  say "Next steps:"
  say "  llca init --global     connect Codex and Claude Code"
  say "  llca doctor            check that everything works"
  say "  llca open              open the dashboard"
}

main "$@"
