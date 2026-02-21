#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
RUN_DIR="$ROOT_DIR/.run"

API_HOST="${PROP_AMM_API_HOST:-127.0.0.1}"
API_PORT="${PROP_AMM_API_PORT:-18002}"
DASH_HOST="${PROP_AMM_DASHBOARD_HOST:-127.0.0.1}"
DASH_PORT="${PROP_AMM_DASHBOARD_PORT:-15173}"

print_status() {
  local pid_file="$1"
  local label="$2"
  if [[ -f "$pid_file" ]]; then
    local pid
    pid="$(cat "$pid_file")"
    if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
      echo "[up]   $label (PID $pid)"
      return
    fi
  fi
  echo "[down] $label"
}

print_status "$RUN_DIR/backend.pid" "backend"
print_status "$RUN_DIR/worker.pid" "worker"
print_status "$RUN_DIR/dashboard.pid" "dashboard"

echo "API URL:       http://$API_HOST:$API_PORT"
echo "Dashboard URL: http://$DASH_HOST:$DASH_PORT"
