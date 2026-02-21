from enum import Enum
from pydantic import BaseModel, Field


class JobType(str, Enum):
    validate = "validate"
    run = "run"
    submit = "submit"


class JobCreateRequest(BaseModel):
    job_type: JobType
    strategy_files: list[str] = Field(default_factory=list)
    source_code: str | None = None
    source_filename: str | None = None
    submitter_handle: str | None = None
    simulations: int = 1000
    steps: int = 2000
    epoch_len: int = 500
    seed_start: int = 0


class JobCreateResponse(BaseModel):
    job_id: int
    status: str


class JobStatusResponse(BaseModel):
    id: int
    status: str
    job_type: str
    strategy_files: list[str]
    simulations: int
    steps: int
    epoch_len: int
    seed_start: int
    submitter_handle: str | None
    created_at: str
    updated_at: str
    exit_code: int | None
    error_message: str | None


class LeaderboardEntry(BaseModel):
    author: str | None
    strategy_name: str
    mean_edge: float
    std_edge: float
    edge_vs_normalizer: float
    sharpe: float
    mean_final_capital_weight: float
    attempts: int = 1
    receipt_path: str | None


class DashboardStats(BaseModel):
    strategies: int
    simulations: int
    steps: int
    epoch_len: int
    normalizer: str = "DYNAMIC"
