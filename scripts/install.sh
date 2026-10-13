#!/usr/bin/env bash
set -euo pipefail
umask 077
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_dir=$(dirname -- "$script_dir")
mode=system
if [[ ${1:-} == --user ]]; then mode=user; shift; fi
binary=${1:-"$project_dir/target/release/neu-login"}
if [[ ! -x "$binary" && -x "$project_dir/neu-login" ]]; then binary="$project_dir/neu-login"; fi
if [[ ! -x "$binary" ]]; then echo "找不到可执行文件：请先 cargo build --release --locked，或指定二进制路径。" >&2; exit 1; fi
if [[ $mode == system && $EUID != 0 ]]; then echo "系统服务安装需要管理员权限：sudo $0 [二进制路径]；也可使用 --user 安装用户服务。" >&2; exit 1; fi
command -v systemctl >/dev/null || { echo '此安装脚本需要 systemd。' >&2; exit 1; }
if [[ $mode == system ]]; then
  command -v curl >/dev/null || { echo '安装验证需要 curl。' >&2; exit 1; }
  if ! getent passwd neu-login >/dev/null; then useradd --system --user-group --home-dir /var/lib/neu-login --shell /usr/sbin/nologin neu-login; fi
  install -d -m 700 -o neu-login -g neu-login /var/lib/neu-login
  destination=/usr/local/bin/neu-login
  config=/etc/neu-login.env
  unit=/etc/systemd/system/neu-login.service
  unit_source="$project_dir/deploy/neu-login.service"
  control=(systemctl)
  if [[ ! -e $config ]]; then
    cat > "$config" <<'EOF'
NEU_LISTEN=0.0.0.0:11099
NEU_DATA_DIR=/var/lib/neu-login
# NEU_IMAGE_DIR=/srv/neu-backgrounds
EOF
    chmod 600 "$config"
  fi
else
  if [[ $EUID == 0 ]]; then echo '--user 应由普通用户执行。' >&2; exit 1; fi
  binary_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
  config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/neu-login"
  data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/neu-login"
  unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
  install -d -m 700 "$config_dir" "$data_dir"
  mkdir -p "$binary_dir" "$unit_dir"
  destination="$binary_dir/neu-login"
  config="$config_dir/environment"
  unit="$unit_dir/neu-login.service"
  control=(systemctl --user)
  unit_config=${config//%/%%}
  unit_binary=${destination//%/%%}
  unit_working_directory=${data_dir//%/%%}
  if [[ ! -e $config ]]; then
    printf 'NEU_LISTEN=0.0.0.0:11099\nNEU_DATA_DIR="%s"\n' "$data_dir" > "$config"
    chmod 600 "$config"
  fi
  cat > "$unit" <<EOF
[Unit]
Description=NEU Campus Network Web Manager (user)
After=network.target
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
Type=simple
EnvironmentFile=$unit_config
WorkingDirectory=$unit_working_directory
Environment=TZ=Asia/Shanghai
Environment=RUST_LOG=neu_login=info
ExecStart="$unit_binary"
Restart=on-failure
RestartSec=3
TimeoutStopSec=10
UMask=0077
NoNewPrivileges=true
MemoryMax=256M

[Install]
WantedBy=default.target
EOF
fi
if [[ -e $destination ]]; then cp -p -- "$destination" "$destination.previous"; fi
install -m 755 -- "$binary" "$destination.new"
mv -f -- "$destination.new" "$destination"
if [[ $mode == system ]]; then install -m 644 -- "$unit_source" "$unit"; fi
"${control[@]}" daemon-reload
"${control[@]}" enable neu-login.service
"${control[@]}" restart neu-login.service
if command -v curl >/dev/null; then
  # Read only the listen address. Do not source an environment file as shell code.
  listen=$(sed -n 's/^NEU_LISTEN=//p' "$config" | tail -n 1 | tr -d '"')
  listen=${listen:-0.0.0.0:11099}
  port=${listen##*:}
  verified=false
  for attempt in {1..20}; do
    if "${control[@]}" is-active --quiet neu-login.service && curl --noproxy '*' -fsS --max-time 2 "http://127.0.0.1:$port/healthz" >/dev/null 2>&1; then verified=true; break; fi
    sleep 1
  done
  if [[ $verified != true ]]; then
    if [[ -e $destination.previous ]]; then
      cp -p -- "$destination.previous" "$destination"
      "${control[@]}" restart neu-login.service || true
      echo '新版本启动检查失败，已恢复之前的程序。' >&2
    else
      "${control[@]}" stop neu-login.service || true
      echo '服务启动检查失败，请查看 journalctl 日志。' >&2
    fi
    exit 1
  fi
fi
echo "NEU 校园网服务已安装（$mode），访问 http://127.0.0.1:${port:-11099}/"
if [[ $mode == user ]]; then echo '用户服务在登录后自动启动；无需登录的开机启动请使用系统服务或由管理员启用 linger。'; fi
