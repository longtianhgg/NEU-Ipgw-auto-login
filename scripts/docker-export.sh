#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_dir=$(dirname -- "$script_dir")
version=$(awk '
  { sub(/\r$/, "") }
  /^[[:space:]]*\[/ { package_section = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*(#.*)?$/) }
  package_section && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"/ {
    sub(/^[[:space:]]*version[[:space:]]*=[[:space:]]*"/, "")
    sub(/".*$/, "")
    print; exit
  }
' "$project_dir/Cargo.toml")
if [[ -z $version ]]; then echo '无法读取 Cargo.toml 中的版本。' >&2; exit 1; fi
skip_build=false
if [[ ${1:-} == --skip-build ]]; then skip_build=true; shift; fi
if [[ $# -gt 2 ]]; then echo '用法：docker-export.sh [--skip-build] [镜像标签] [输出目录]' >&2; exit 1; fi
image=${1:-"neu-login:$version-arm64"}
output_dir=${2:-"$project_dir/release"}

for tool in docker gzip sha256sum; do
  command -v "$tool" >/dev/null || { echo "缺少命令：$tool" >&2; exit 1; }
done
if [[ $skip_build == false ]] && ! docker buildx version >/dev/null 2>&1; then
  echo 'ARM64 镜像构建需要 Docker Buildx 插件，经典构建器可能选错基础镜像架构，已停止构建。' >&2
  echo '请安装 Buildx，并确认实际执行脚本的用户可运行 docker buildx version（使用 sudo 时验证 sudo docker buildx version）。' >&2
  echo '安装及 ARMv7 构建器配置步骤见 README.md。' >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo '无法访问 Docker daemon。请确认 Docker 已启动且当前用户有访问权限，或使用 sudo 执行此脚本。' >&2
  exit 1
fi
if [[ $skip_build == false ]]; then
  build_jobs=${NEU_BUILD_JOBS:-2}
  if [[ ! $build_jobs =~ ^[1-9][0-9]*$ ]]; then
    echo 'NEU_BUILD_JOBS 必须是正整数。' >&2
    exit 1
  fi
  build_options=(--platform linux/arm64 --load --tag "$image" --build-arg "CARGO_BUILD_JOBS=$build_jobs")
  if [[ -n ${NEU_BUILD_NETWORK:-} ]]; then
    build_options+=(--network "$NEU_BUILD_NETWORK")
  fi
  # Forward predefined build proxy variables by name, without printing their values.
  # These variables are used only during the build, not in the runtime image.
  for proxy_variable in HTTP_PROXY HTTPS_PROXY NO_PROXY http_proxy https_proxy no_proxy; do
    if [[ -n ${!proxy_variable:-} ]]; then
      build_options+=(--build-arg "$proxy_variable")
    fi
  done
  docker buildx build "${build_options[@]}" "$project_dir"
fi
platform=$(docker image inspect --format '{{.Os}}-{{.Architecture}}' "$image")
if [[ $platform != linux-arm64 ]]; then echo "镜像必须为 linux-arm64，实际为：$platform" >&2; exit 1; fi

output_created=false
if [[ ! -d $output_dir ]]; then output_created=true; fi
(umask 022; mkdir -p "$output_dir")
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
archive="neu-login-$version-docker-$platform.tar.gz"
staging_dir=$(mktemp -d "$output_dir/.docker-export.XXXXXX")
trap 'rm -rf -- "$staging_dir"' EXIT

# docker save includes the runtime base layers; the target host needs no source or Rust.
docker image save "$image" | gzip > "$staging_dir/$archive"
mv -f -- "$staging_dir/$archive" "$output_dir/$archive"
(cd "$output_dir" && sha256sum "$archive" > "$archive.sha256")

# A loaded image runs with --no-build --pull never on the target host.
if [[ ! "$project_dir/compose.yaml" -ef "$output_dir/compose.yaml" ]]; then
  cp -- "$project_dir/compose.yaml" "$output_dir/compose.yaml"
fi

# sudo should not leave the invoking user's exported files owned by root.
if [[ $EUID == 0 && ${SUDO_UID:-} =~ ^[0-9]+$ && ${SUDO_GID:-} =~ ^[0-9]+$ ]]; then
  chown "$SUDO_UID:$SUDO_GID" "$output_dir/$archive" "$output_dir/$archive.sha256"
  if [[ ! "$project_dir/compose.yaml" -ef "$output_dir/compose.yaml" ]]; then
    chown "$SUDO_UID:$SUDO_GID" "$output_dir/compose.yaml"
  fi
  if [[ $output_created == true ]]; then chown "$SUDO_UID:$SUDO_GID" "$output_dir"; fi
fi
printf '镜像：%s\n平台：%s\n镜像包：%s\n校验文件：%s\n部署配置：%s\n' \
  "$image" "$platform" "$output_dir/$archive" "$output_dir/$archive.sha256" "$output_dir/compose.yaml"
printf '目标主机运行：\n  docker load -i %q\n  NEU_IMAGE=%q docker compose -f %q up -d --no-build --pull never\n' \
  "$archive" "$image" 'compose.yaml'
