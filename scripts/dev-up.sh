#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
RUN_DIR="$ROOT_DIR/.run"
mkdir -p "$RUN_DIR"

API_HOST="${PROP_AMM_API_HOST:-127.0.0.1}"
API_PORT="${PROP_AMM_API_PORT:-18002}"
DASH_HOST="${PROP_AMM_DASHBOARD_HOST:-127.0.0.1}"
DASH_PORT="${PROP_AMM_DASHBOARD_PORT:-15173}"
INLINE_EXEC="${PROP_AMM_INLINE_JOB_EXECUTION:-false}"

if [[ -x "$ROOT_DIR/.venv/bin/python" ]]; then
  PYTHON_BIN="$ROOT_DIR/.venv/bin/python"
else
  PYTHON_BIN="${PYTHON_BIN:-python3}"
fi

port_in_use() {
  local port="$1"
  lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1
}

wait_for_url() {
  local url="$1"
  local label="$2"
  local retries="${3:-40}"
  local sleep_secs="${4:-0.25}"

  for _ in $(seq 1 "$retries"); do
    if curl -fsS "$url" >/dev/null 2>&1; then
      echo "[ok] $label ready"
      return 0
    fi
    sleep "$sleep_secs"
  done

  echo "[warn] $label not ready yet. Check logs in $RUN_DIR"
  return 1
}

assert_pid_not_running() {
  local pid_file="$1"
  local label="$2"
  if [[ -f "$pid_file" ]]; then
    local pid
    pid="$(cat "$pid_file")"
    if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
      echo "[skip] $label already running with PID $pid"
      return 1
    fi
    rm -f "$pid_file"
  fi
  return 0
}

if port_in_use "$API_PORT"; then
  echo "[error] API port $API_PORT is already in use."
  echo "        Set PROP_AMM_API_PORT to a free port and retry."
  exit 1
fi

if port_in_use "$DASH_PORT"; then
  echo "[error] Dashboard port $DASH_PORT is already in use."
  echo "        Set PROP_AMM_DASHBOARD_PORT to a free port and retry."
  exit 1
fi

BACKEND_PID_FILE="$RUN_DIR/backend.pid"
WORKER_PID_FILE="$RUN_DIR/worker.pid"
DASH_PID_FILE="$RUN_DIR/dashboard.pid"

if assert_pid_not_running "$BACKEND_PID_FILE" "backend"; then
  (
    cd "$ROOT_DIR"
    export PYTHONPATH="$ROOT_DIR"
    export PROP_AMM_INLINE_JOB_EXECUTION="$INLINE_EXEC"
    nohup "$PYTHON_BIN" -m uvicorn apps.backend.app.main:app --host "$API_HOST" --port "$API_PORT" \
      > "$RUN_DIR/backend.log" 2>&1 &
    echo $! > "$BACKEND_PID_FILE"
  )
fi

if assert_pid_not_running "$WORKER_PID_FILE" "worker"; then
  (
    cd "$ROOT_DIR"
    export PYTHONPATH="$ROOT_DIR"
    nohup "$PYTHON_BIN" -m apps.worker.worker > "$RUN_DIR/worker.log" 2>&1 &
    echo $! > "$WORKER_PID_FILE"
  )
fi

if assert_pid_not_running "$DASH_PID_FILE" "dashboard"; then
  (
    cd "$ROOT_DIR/apps/dashboard"
    export VITE_API_BASE_URL="http://$API_HOST:$API_PORT"
    nohup npm run dev -- --host "$DASH_HOST" --port "$DASH_PORT" --strictPort \
      > "$RUN_DIR/dashboard.log" 2>&1 &
    echo $! > "$DASH_PID_FILE"
  )
fi

echo "Started services with project-scoped PID files under $RUN_DIR"
echo "Dashboard: http://$DASH_HOST:$DASH_PORT"
echo "API:       http://$API_HOST:$API_PORT"
echo "Docs:      http://$API_HOST:$API_PORT/docs"

wait_for_url "http://$API_HOST:$API_PORT/healthz" "backend"
wait_for_url "http://$DASH_HOST:$DASH_PORT" "dashboard"
