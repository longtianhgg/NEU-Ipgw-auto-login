#!/usr/bin/env bash
set -euo pipefail
mode=system
if [[ ${1:-} == --user ]]; then mode=user; shift; fi
purge=false
if [[ ${1:-} == --purge-data ]]; then purge=true; shift; fi
if [[ $# != 0 ]]; then echo '用法：uninstall.sh [--user] [--purge-data]' >&2; exit 1; fi
if [[ $mode == system ]]; then
  if [[ $EUID != 0 ]]; then echo '系统服务卸载需要 sudo。' >&2; exit 1; fi
  systemctl disable --now neu-login.service || true
  rm -f /etc/systemd/system/neu-login.service /usr/local/bin/neu-login /usr/local/bin/neu-login.previous
  systemctl daemon-reload
  if [[ $purge == true ]]; then rm -rf -- /var/lib/neu-login; rm -f /etc/neu-login.env; fi
else
  if [[ $EUID == 0 ]]; then echo '--user 应由普通用户执行。' >&2; exit 1; fi
  systemctl --user disable --now neu-login.service || true
  rm -f -- "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/neu-login.service" "${XDG_BIN_HOME:-$HOME/.local/bin}/neu-login" "${XDG_BIN_HOME:-$HOME/.local/bin}/neu-login.previous"
  systemctl --user daemon-reload
  if [[ $purge == true ]]; then rm -rf -- "${XDG_DATA_HOME:-$HOME/.local/share}/neu-login" "${XDG_CONFIG_HOME:-$HOME/.config}/neu-login"; fi
fi
echo '服务已卸载。默认保留账号、密钥、背景、设置和操作记录；--purge-data 会删除这些数据。'
