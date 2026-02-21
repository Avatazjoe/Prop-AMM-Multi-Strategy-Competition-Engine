#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT_DIR"

python -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt

export PROP_AMM_API_PORT="${PROP_AMM_API_PORT:-18002}"
export PROP_AMM_DASHBOARD_PORT="${PROP_AMM_DASHBOARD_PORT:-15173}"

bash scripts/dev-up.sh
bash scripts/dev-status.sh

echo "Logs are in .run/backend.log, .run/worker.log, .run/dashboard.log"
