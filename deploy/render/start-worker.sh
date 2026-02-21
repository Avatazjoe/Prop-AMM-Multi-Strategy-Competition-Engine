#!/usr/bin/env bash
set -euo pipefail

export PYTHONPATH=/app
export PROP_AMM_INLINE_JOB_EXECUTION=false
python -m apps.worker.worker
