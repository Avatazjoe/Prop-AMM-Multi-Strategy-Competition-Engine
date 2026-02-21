from fastapi import FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import PlainTextResponse

from .config import CORS_ORIGINS, DASHBOARD_HOST, DASHBOARD_PORT, INLINE_JOB_EXECUTION

from .job_runtime import (
    create_job,
    dashboard_stats,
    get_job,
    get_logs,
    init_db,
    latest_leaderboard,
    normalize_strategy_inputs,
    spawn_job,
)
from .models import DashboardStats, JobCreateRequest, JobCreateResponse, JobStatusResponse, LeaderboardEntry

app = FastAPI(title="Prop AMM Submit API", version="0.1.0")

app.add_middleware(
    CORSMiddleware,
    allow_origins=CORS_ORIGINS,
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


@app.on_event("startup")
def startup() -> None:
    init_db()


@app.get("/")
def root() -> dict:
    return {
        "service": "prop-amm-backend",
        "status": "ok",
        "ui_url": f"http://{DASHBOARD_HOST}:{DASHBOARD_PORT}",
        "docs_url": "/docs",
    }


@app.get("/health")
def health() -> dict:
    return {"status": "ok"}


@app.get("/healthz")
def healthz() -> dict:
    return {"status": "ok"}


@app.post("/api/jobs", response_model=JobCreateResponse)
def create_job_endpoint(payload: JobCreateRequest) -> JobCreateResponse:
    try:
        strategy_files = normalize_strategy_inputs(
            payload.strategy_files,
            payload.source_code,
            payload.source_filename,
        )
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc

    if payload.simulations <= 0 or payload.steps <= 0 or payload.epoch_len <= 0:
        raise HTTPException(status_code=400, detail="simulations, steps, epoch_len must be > 0")

    job_id = create_job(
        job_type=payload.job_type.value,
        strategy_files=strategy_files,
        submitter_handle=payload.submitter_handle,
        simulations=payload.simulations,
        steps=payload.steps,
        epoch_len=payload.epoch_len,
        seed_start=payload.seed_start,
    )
    if INLINE_JOB_EXECUTION:
        spawn_job(job_id)
    return JobCreateResponse(job_id=job_id, status="queued")


@app.get("/api/jobs/{job_id}", response_model=JobStatusResponse)
def get_job_endpoint(job_id: int) -> JobStatusResponse:
    job = get_job(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="job not found")
    return JobStatusResponse(**job)


@app.get("/api/jobs/{job_id}/logs", response_class=PlainTextResponse)
def get_job_logs_endpoint(job_id: int) -> str:
    logs = get_logs(job_id)
    if logs is None:
        raise HTTPException(status_code=404, detail="job not found")
    return logs


@app.get("/api/leaderboard", response_model=list[LeaderboardEntry])
def leaderboard_endpoint() -> list[LeaderboardEntry]:
    rows = latest_leaderboard(limit=50)
    return [LeaderboardEntry(**row) for row in rows]


@app.get("/api/stats", response_model=DashboardStats)
def stats_endpoint() -> DashboardStats:
    return DashboardStats(**dashboard_stats())
