#!/bin/sh
# Installs the latest Draftmux release for this user:
#
#   curl -fsSL https://raw.githubusercontent.com/raine/draftmux/main/install.sh | sh
#
# macOS (Apple silicon): Draftmux.app in /Applications, or ~/Applications when
# /Applications isn't writable, after checking its Developer ID signature.
# Linux (x86_64, arm64): the app tree in ~/.local/lib/draftmux. Both link
# the draftmux command into ~/.local/bin. Running it again updates Draftmux.
#
# DRAFTMUX_VERSION picks a release instead of the latest,
# DRAFTMUX_INSTALL_PREFIX replaces ~/.local, and DRAFTMUX_APP_DIR picks the
# folder for Draftmux.app on macOS.
set -eu

releases="${DRAFTMUX_RELEASES_URL:-https://github.com/raine/draftmux/releases}"
team=YG824X87Y2
bundle_id=fi.zendit.Draftmux

fail() {
  echo "draftmux install: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

latest_version() {
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$releases/latest") ||
    fail "couldn't reach $releases"
  case $url in
    */tag/v*) echo "${url##*/tag/v}" ;;
    *) fail "no release found at $releases" ;;
  esac
}

# Downloads NAME and its checksum file CHECKSUM into the working directory
# and checks one against the other.
fetch() {
  curl -fsSL -o "$1" "$releases/download/v$version/$1" || fail "couldn't download $1"
  curl -fsSL -o "$2" "$releases/download/v$version/$2" || fail "couldn't download $2"
  expected=$(awk '{ print $1; exit }' "$2")
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$1" | awk '{ print $1 }')
  else
    actual=$(shasum -a 256 "$1" | awk '{ print $1 }')
  fi
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    fail "checksum mismatch for $1"
  fi
}

install_macos() {
  [ "$(uname -m)" = arm64 ] || fail "Draftmux for macOS needs an Apple silicon Mac"
  need codesign
  name="Draftmux-$version-arm64-app"
  fetch "$name.tar.gz" "$name.sha256"
  mkdir extracted
  tar -xzf "$name.tar.gz" -C extracted
  app=extracted/Draftmux.app
  codesign --verify --deep --strict "$app" 2>/dev/null || fail "Draftmux.app's signature is invalid"
  codesign --verify -R="anchor apple generic and certificate leaf[subject.OU] = \"$team\"" \
    "$app" 2>/dev/null || fail "Draftmux.app isn't signed by Draftmux's developer"

  dir="${DRAFTMUX_APP_DIR:-/Applications}"
  if [ -z "${DRAFTMUX_APP_DIR:-}" ] && [ ! -w "$dir" ]; then dir="$HOME/Applications"; fi
  mkdir -p "$dir"
  target="$dir/Draftmux.app"
  if [ -e "$target" ]; then
    existing=$(defaults read "$target/Contents/Info" CFBundleIdentifier 2>/dev/null || true)
    [ "$existing" = "$bundle_id" ] || fail "refusing to replace $target, which isn't Draftmux"
  fi
  # Swapped in with renames so a failed copy leaves the installed app intact.
  rm -rf "$dir/.Draftmux-install.app" "$dir/.Draftmux-old.app"
  ditto "$app" "$dir/.Draftmux-install.app"
  if [ -e "$target" ]; then mv "$target" "$dir/.Draftmux-old.app"; fi
  mv "$dir/.Draftmux-install.app" "$target"
  rm -rf "$dir/.Draftmux-old.app"

  bin="${DRAFTMUX_INSTALL_PREFIX:-$HOME/.local}/bin"
  link="$bin/draftmux"
  mkdir -p "$bin"
  if [ -e "$link" ] || [ -L "$link" ]; then
    case $(readlink "$link" || true) in
      */Draftmux.app/Contents/MacOS/draftmux) rm "$link" ;;
      *) fail "refusing to replace $link, which isn't Draftmux's command" ;;
    esac
  fi
  ln -s "$target/Contents/MacOS/draftmux" "$link"
  echo "Installed Draftmux $version in $dir and linked $link"
  case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "Add $bin to your PATH to use the draftmux command." ;;
  esac
}

install_linux() {
  case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "no Linux build for $(uname -m)" ;;
  esac
  name="Draftmux-$version-linux-$arch"
  fetch "$name.tar.gz" "$name.sha256"
  tar -xzf "$name.tar.gz"
  draftmux-linux/install
  echo "Installed Draftmux $version"
}

main() {
  need curl
  need tar
  version="${DRAFTMUX_VERSION:-$(latest_version)}"
  version="${version#v}"
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  cd "$work"
  case $(uname -s) in
    Darwin) install_macos ;;
    Linux) install_linux ;;
    *) fail "Draftmux runs on macOS and Linux" ;;
  esac
}

# Runs only once the whole script has downloaded.
main
