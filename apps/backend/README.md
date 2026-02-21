# Backend (FastAPI)

API for validate/run/submit jobs backed by the Rust CLI.

## Run

```bash
cd /path/to/prop-amm-multi
python -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
export PYTHONPATH="$PWD"
uvicorn apps.backend.app.main:app --host 0.0.0.0 --port 8000
```

## Endpoints

- `POST /api/jobs`
- `GET /api/jobs/{job_id}`
- `GET /api/jobs/{job_id}/logs`
- `GET /api/leaderboard`
- `GET /healthz`

`POST /api/jobs` accepts either:

- `strategy_files`: existing `.rs` file paths in repo, or
- `source_code` + `source_filename`: inline editor submission (used by UX dashboard).
