#!/bin/sh
# Download a release archive and verify it before installing anything.
set -eu

fail() { printf '%s\n' "navarch install: $*" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || fail 'curl is required'
command -v tar >/dev/null 2>&1 || fail 'tar is required'
case $(uname -s) in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-gnu ;;
    *) fail 'only macOS and Linux are supported' ;;
esac
case $(uname -m) in
    arm64|aarch64) arch=aarch64 ;;
    x86_64|amd64) arch=x86_64 ;;
    *) fail 'only x86-64 and arm64 are supported' ;;
esac
if command -v sha256sum >/dev/null 2>&1; then
    checksum() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
    checksum() { shasum -a 256 "$1" | awk '{print $1}'; }
else
    fail 'sha256sum or shasum is required'
fi

repo=https://github.com/navarch-dev/navarch
version=${NAVARCH_VERSION:-}
if [ -z "$version" ]; then
    latest=$(curl --proto '=https' --tlsv1.2 -fLsS -o /dev/null -w '%{url_effective}' "$repo/releases/latest") || fail 'cannot resolve latest release'
    version=${latest##*/}
fi
version=${version#v}
case $version in
    ''|*[!0-9A-Za-z.+-]*) fail 'invalid NAVARCH_VERSION or latest release tag' ;;
esac
case $version in
    [0-9]*) ;;
    *) fail 'version must start with a digit' ;;
esac
archive=navarch-$version-$arch-$os.tar.gz
url=$repo/releases/download/v$version
work=$(mktemp -d) || fail 'cannot create temporary directory'
trap 'rm -rf "$work"' 0
trap 'exit 1' HUP INT TERM
curl --proto '=https' --tlsv1.2 -fLsS "$url/$archive" -o "$work/$archive" || fail 'release download failed'
curl --proto '=https' --tlsv1.2 -fLsS "$url/SHA256SUMS" -o "$work/SHA256SUMS" || fail 'checksum download failed'
expected=$(awk -v name="$archive" '$2 == name {print $1}' "$work/SHA256SUMS")
[ "${#expected}" -eq 64 ] || fail 'missing, duplicate or malformed checksum'
case $expected in *[!0-9a-fA-F]*) fail 'malformed checksum' ;; esac
actual=$(checksum "$work/$archive") || fail 'cannot calculate checksum'
expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
[ "$actual" = "$expected" ] || fail 'checksum mismatch; nothing was installed'
tar -xzf "$work/$archive" -C "$work" navarch || fail 'cannot extract binary'
[ -f "$work/navarch" ] && [ ! -L "$work/navarch" ] || fail 'archive does not contain a regular binary'
install_dir=${NAVARCH_INSTALL_DIR:-$HOME/.local/bin}
mkdir -p "$install_dir" || fail 'cannot create installation directory'
# Copy to a temporary file in the destination before replacing an existing binary.
staged=$(mktemp "$install_dir/.navarch.XXXXXXXX") || fail 'cannot stage installation'
if ! cp "$work/navarch" "$staged" || ! chmod 755 "$staged" || ! mv -f "$staged" "$install_dir/navarch"; then
    rm -f "$staged"
    fail 'cannot install binary'
fi
printf 'Installed navarch %s to %s/navarch\n' "$version" "$install_dir"
case :${PATH:-}: in
    *:"$install_dir":*) ;;
    *)
        escaped=$(printf '%s' "$install_dir" | sed "s/'/'\\\\''/g")
        printf "Add this to your shell profile:\n  export PATH='%s':\"\$PATH\"\n" "$escaped"
        ;;
esac
