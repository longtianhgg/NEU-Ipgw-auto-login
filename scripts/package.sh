#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_dir=$(dirname -- "$script_dir")
cd "$project_dir"
cargo build --release --locked
version=$(/usr/bin/env "$project_dir/target/release/neu-login" --version | awk '{print $2}')
bundle="neu-login-$version-x86_64-linux"
mkdir -p release
staging_dir=$(mktemp -d "$project_dir/release/.package.XXXXXX")
trap 'rm -rf -- "$staging_dir"' EXIT
mkdir -p "$staging_dir/$bundle/deploy" "$staging_dir/$bundle/scripts"
install -m 755 target/release/neu-login "$staging_dir/$bundle/neu-login"
install -m 755 scripts/install.sh scripts/uninstall.sh "$staging_dir/$bundle/scripts/"
install -m 644 deploy/neu-login.service "$staging_dir/$bundle/deploy/"
install -m 644 README.md "$staging_dir/$bundle/README.md"
tar -C "$staging_dir" -czf "release/$bundle.tar.gz.tmp" "$bundle"
mv -- "release/$bundle.tar.gz.tmp" "release/$bundle.tar.gz"
(cd release && sha256sum "$bundle.tar.gz" > "$bundle.tar.gz.sha256")
echo "release/$bundle.tar.gz"
