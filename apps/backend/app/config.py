from __future__ import annotations

import os
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parents[3]
DATA_DIR = Path(os.getenv("PROP_AMM_DATA_DIR", ROOT_DIR / "data"))
DB_PATH = Path(os.getenv("PROP_AMM_DB_PATH", DATA_DIR / "jobs.db"))

CORS_ORIGINS = [origin.strip() for origin in os.getenv("PROP_AMM_CORS_ORIGINS", "*").split(",") if origin.strip()]
INLINE_JOB_EXECUTION = os.getenv("PROP_AMM_INLINE_JOB_EXECUTION", "false").lower() in {"1", "true", "yes"}
MAX_SOURCE_BYTES = int(os.getenv("PROP_AMM_MAX_SOURCE_BYTES", "300000"))
API_HOST = os.getenv("PROP_AMM_API_HOST", "127.0.0.1")
API_PORT = int(os.getenv("PROP_AMM_API_PORT", "18002"))
DASHBOARD_HOST = os.getenv("PROP_AMM_DASHBOARD_HOST", "127.0.0.1")
DASHBOARD_PORT = int(os.getenv("PROP_AMM_DASHBOARD_PORT", "15173"))
JOB_POLL_SECONDS = float(os.getenv("PROP_AMM_JOB_POLL_SECONDS", "1.0"))
