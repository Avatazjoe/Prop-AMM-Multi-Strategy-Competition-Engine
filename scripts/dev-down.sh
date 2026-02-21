#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
RUN_DIR="$ROOT_DIR/.run"

stop_from_pid_file() {
  local pid_file="$1"
  local label="$2"

  if [[ ! -f "$pid_file" ]]; then
    echo "[skip] $label not started by this project"
    return
  fi

  local pid
  pid="$(cat "$pid_file")"

  if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
    kill "$pid" >/dev/null 2>&1 || true
    sleep 0.5
    if kill -0 "$pid" >/dev/null 2>&1; then
      kill -9 "$pid" >/dev/null 2>&1 || true
    fi
    echo "[ok] stopped $label (PID $pid)"
  else
    echo "[skip] stale PID for $label"
  fi

  rm -f "$pid_file"
}

stop_from_pid_file "$RUN_DIR/backend.pid" "backend"
stop_from_pid_file "$RUN_DIR/worker.pid" "worker"
stop_from_pid_file "$RUN_DIR/dashboard.pid" "dashboard"
