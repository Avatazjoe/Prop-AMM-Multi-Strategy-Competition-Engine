#!/usr/bin/env bash
set -euo pipefail

PORT="${PORT:-8000}"
export PYTHONPATH=/app
export PROP_AMM_INLINE_JOB_EXECUTION=false
uvicorn apps.backend.app.main:app --host 0.0.0.0 --port "$PORT"
