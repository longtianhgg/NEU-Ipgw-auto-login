#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_dir=$(dirname -- "$script_dir")
cd "$project_dir"
cargo build --release --locked --target armv7-unknown-linux-gnueabihf
version=$(awk '
  { sub(/\r$/, "") }
  /^[[:space:]]*\[/ { package_section = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*(#.*)?$/) }
  package_section && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"/ {
    sub(/^[[:space:]]*version[[:space:]]*=[[:space:]]*"/, "")
    sub(/".*$/, "")
    print; exit
  }
' Cargo.toml)
if [[ -z $version ]]; then echo '无法读取 Cargo.toml 中的版本。' >&2; exit 1; fi
binary="${CARGO_TARGET_DIR:-$project_dir/target}/armv7-unknown-linux-gnueabihf/release/neu-login"
bundle="neu-login-$version-arm32-linux"
mkdir -p release
staging_dir=$(mktemp -d "$project_dir/release/.package.XXXXXX")
trap 'rm -rf -- "$staging_dir"' EXIT
mkdir -p "$staging_dir/$bundle/deploy" "$staging_dir/$bundle/scripts"
install -m 755 "$binary" "$staging_dir/$bundle/neu-login"
install -m 755 scripts/install.sh scripts/uninstall.sh "$staging_dir/$bundle/scripts/"
install -m 644 deploy/neu-login.service "$staging_dir/$bundle/deploy/"
install -m 644 README.md "$staging_dir/$bundle/README.md"
tar -C "$staging_dir" -czf "release/$bundle.tar.gz.tmp" "$bundle"
mv -- "release/$bundle.tar.gz.tmp" "release/$bundle.tar.gz"
(cd release && sha256sum "$bundle.tar.gz" > "$bundle.tar.gz.sha256")
echo "release/$bundle.tar.gz"
