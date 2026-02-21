from __future__ import annotations

import json
import os
import re
import sqlite3
import subprocess
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

from .config import DB_PATH, DATA_DIR, JOB_POLL_SECONDS, MAX_SOURCE_BYTES, ROOT_DIR

CLI_CMD = ["cargo", "run", "--bin", "prop-amm-multi", "--"]
STRATEGY_PATTERN = re.compile(r"^submission_[0-5]\.rs$")
SAFE_FILE_PATTERN = re.compile(r"[^a-zA-Z0-9_.-]")


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _conn() -> sqlite3.Connection:
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(DB_PATH)
    connection.row_factory = sqlite3.Row
    return connection


def init_db() -> None:
    with _conn() as conn:
        conn.executescript(
            """
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                status TEXT NOT NULL,
                job_type TEXT NOT NULL,
                strategy_files_json TEXT NOT NULL,
                simulations INTEGER NOT NULL,
                steps INTEGER NOT NULL,
                epoch_len INTEGER NOT NULL,
                seed_start INTEGER NOT NULL,
                submitter_handle TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                exit_code INTEGER,
                error_message TEXT,
                logs TEXT DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS leaderboard (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                author TEXT,
                strategy_name TEXT NOT NULL,
                mean_edge REAL NOT NULL,
                std_edge REAL NOT NULL,
                edge_vs_normalizer REAL NOT NULL,
                sharpe REAL NOT NULL,
                mean_final_capital_weight REAL NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 1,
                receipt_path TEXT
            );
            """
        )
        _ensure_column(conn, "jobs", "submitter_handle", "TEXT")
        _ensure_column(conn, "leaderboard", "author", "TEXT")
        _ensure_column(conn, "leaderboard", "attempts", "INTEGER NOT NULL DEFAULT 1")


def _ensure_column(conn: sqlite3.Connection, table: str, column: str, definition: str) -> None:
    rows = conn.execute(f"PRAGMA table_info({table})").fetchall()
    existing = {str(row[1]) for row in rows}
    if column in existing:
        return
    conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {definition}")


def validate_strategy_files(files: list[str]) -> list[str]:
    validated = []
    for name in files:
        if not STRATEGY_PATTERN.match(name):
            raise ValueError(f"Unsupported strategy filename: {name}")
        full_path = ROOT_DIR / name
        if not full_path.exists():
            raise ValueError(f"Strategy file not found: {name}")
        validated.append(name)
    return validated


def _sanitize_filename(name: str) -> str:
    cleaned = SAFE_FILE_PATTERN.sub("_", name or "strategy.rs")
    if not cleaned.endswith(".rs"):
        cleaned += ".rs"
    return cleaned


def normalize_strategy_inputs(strategy_files: list[str], source_code: str | None, source_filename: str | None) -> list[str]:
    if source_code and source_code.strip():
        if len(source_code.encode("utf-8")) > MAX_SOURCE_BYTES:
            raise ValueError(f"Source exceeds max size ({MAX_SOURCE_BYTES} bytes)")
        uploads_dir = DATA_DIR / "uploads"
        uploads_dir.mkdir(parents=True, exist_ok=True)
        ts = int(time.time() * 1000)
        safe_name = _sanitize_filename(source_filename or "strategy.rs")
        path = uploads_dir / f"job_{ts}_{safe_name}"
        path.write_text(source_code, encoding="utf-8")
        return [str(path)]

    if not strategy_files:
        raise ValueError("Provide strategy_files or source_code")
    return validate_strategy_files(strategy_files)


def create_job(
    job_type: str,
    strategy_files: list[str],
    submitter_handle: str | None,
    simulations: int,
    steps: int,
    epoch_len: int,
    seed_start: int,
) -> int:
    now = utc_now_iso()
    with _conn() as conn:
        cur = conn.execute(
            """
            INSERT INTO jobs (
                status, job_type, strategy_files_json, simulations, steps, epoch_len, seed_start,
                submitter_handle, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                "queued",
                job_type,
                json.dumps(strategy_files),
                simulations,
                steps,
                epoch_len,
                seed_start,
                submitter_handle,
                now,
                now,
            ),
        )
        return int(cur.lastrowid)


def get_job(job_id: int) -> dict | None:
    with _conn() as conn:
        row = conn.execute("SELECT * FROM jobs WHERE id = ?", (job_id,)).fetchone()
    if row is None:
        return None
    payload = dict(row)
    payload["strategy_files"] = json.loads(payload.pop("strategy_files_json"))
    return payload


def append_logs(job_id: int, chunk: str) -> None:
    now = utc_now_iso()
    with _conn() as conn:
        conn.execute(
            "UPDATE jobs SET logs = logs || ?, updated_at = ? WHERE id = ?",
            (chunk, now, job_id),
        )


def _set_job_state(job_id: int, status: str, exit_code: int | None = None, error_message: str | None = None) -> None:
    now = utc_now_iso()
    with _conn() as conn:
        conn.execute(
            "UPDATE jobs SET status = ?, exit_code = ?, error_message = ?, updated_at = ? WHERE id = ?",
            (status, exit_code, error_message, now, job_id),
        )


def _build_command(job: dict) -> list[str]:
    args = [job["job_type"], *job["strategy_files"]]
    if job["job_type"] in {"run", "submit"}:
        args.extend(
            [
                "--simulations",
                str(job["simulations"]),
                "--steps",
                str(job["steps"]),
                "--epoch-len",
                str(job["epoch_len"]),
                "--seed-start",
                str(job["seed_start"]),
            ]
        )
    return CLI_CMD + args


def parse_and_store_leaderboard(log_text: str, author: str | None = None) -> None:
    rows = []
    for line in log_text.splitlines():
        line = line.strip()
        if "submission_" not in line:
            continue
        parts = line.split()
        if len(parts) < 6:
            continue
        name = parts[0]
        if not name.startswith("submission_"):
            continue
        try:
            mean_edge = float(parts[1])
            std_edge = float(parts[2])
            edge_vs_normalizer = float(parts[3])
            sharpe = float(parts[4])
            final_cap_pct = float(parts[5])
        except ValueError:
            continue
        rows.append((name, mean_edge, std_edge, edge_vs_normalizer, sharpe, final_cap_pct / 100.0))

    receipt = None
    for line in log_text.splitlines():
        if "Submission receipt:" in line:
            receipt = line.split("Submission receipt:", 1)[1].strip()

    if not rows:
        return

    now = utc_now_iso()
    with _conn() as conn:
        conn.executemany(
            """
            INSERT INTO leaderboard (
                created_at, author, strategy_name, mean_edge, std_edge, edge_vs_normalizer, sharpe,
                mean_final_capital_weight, attempts, receipt_path
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            [(now, author, r[0], r[1], r[2], r[3], r[4], r[5], 1, receipt) for r in rows],
        )


def process_job(job_id: int, already_claimed: bool = False) -> None:
    job = get_job(job_id) if already_claimed else claim_job(job_id)
    if job is None:
        return

    cmd = _build_command(job)

    env = os.environ.copy()
    cargo_bin = Path.home() / ".cargo" / "bin"
    env["PATH"] = f"{cargo_bin}:{env.get('PATH', '')}"

    process = subprocess.Popen(
        cmd,
        cwd=ROOT_DIR,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        env=env,
    )

    assert process.stdout is not None
    full_log = []
    for line in process.stdout:
        full_log.append(line)
        append_logs(job_id, line)

    process.wait()
    log_text = "".join(full_log)

    if process.returncode == 0:
        _set_job_state(job_id, "completed", exit_code=0)
        if job["job_type"] == "submit":
            parse_and_store_leaderboard(log_text, job.get("submitter_handle"))
    else:
        _set_job_state(job_id, "failed", exit_code=process.returncode, error_message="job execution failed")


def spawn_job(job_id: int) -> None:
    thread = threading.Thread(target=process_job, args=(job_id,), daemon=True)
    thread.start()


def claim_job(job_id: int) -> dict | None:
    now = utc_now_iso()
    with _conn() as conn:
        row = conn.execute("SELECT * FROM jobs WHERE id = ?", (job_id,)).fetchone()
        if row is None or row["status"] != "queued":
            return None
        conn.execute(
            "UPDATE jobs SET status = 'running', updated_at = ? WHERE id = ? AND status = 'queued'",
            (now, job_id),
        )
        row2 = conn.execute("SELECT * FROM jobs WHERE id = ?", (job_id,)).fetchone()

    if row2 is None or row2["status"] != "running":
        return None

    payload = dict(row2)
    payload["strategy_files"] = json.loads(payload.pop("strategy_files_json"))
    return payload


def claim_next_queued_job() -> int | None:
    now = utc_now_iso()
    with _conn() as conn:
        row = conn.execute(
            "SELECT id FROM jobs WHERE status = 'queued' ORDER BY id ASC LIMIT 1"
        ).fetchone()
        if row is None:
            return None
        job_id = int(row["id"])
        conn.execute(
            "UPDATE jobs SET status = 'running', updated_at = ? WHERE id = ? AND status = 'queued'",
            (now, job_id),
        )
        check = conn.execute("SELECT status FROM jobs WHERE id = ?", (job_id,)).fetchone()

    if check is None or check["status"] != "running":
        return None
    return job_id


def latest_leaderboard(limit: int = 20) -> list[dict]:
    with _conn() as conn:
        rows = conn.execute(
            """
            SELECT author, strategy_name, mean_edge, std_edge, edge_vs_normalizer, sharpe,
                   mean_final_capital_weight, attempts, receipt_path
            FROM leaderboard
            ORDER BY id DESC
            LIMIT ?
            """,
            (limit,),
        ).fetchall()
    return [dict(r) for r in rows]


def dashboard_stats() -> dict:
    with _conn() as conn:
        row = conn.execute(
            """
            SELECT COUNT(DISTINCT strategy_name) AS strategies
            FROM leaderboard
            """
        ).fetchone()

        latest_job = conn.execute(
            """
            SELECT simulations, steps, epoch_len
            FROM jobs
            WHERE status = 'completed' AND job_type IN ('run', 'submit')
            ORDER BY id DESC
            LIMIT 1
            """
        ).fetchone()

    return {
        "strategies": int(row["strategies"]) if row and row["strategies"] is not None else 0,
        "simulations": int(latest_job["simulations"]) if latest_job else 1000,
        "steps": int(latest_job["steps"]) if latest_job else 10000,
        "epoch_len": int(latest_job["epoch_len"]) if latest_job else 1000,
        "normalizer": "DYNAMIC",
    }


def get_logs(job_id: int) -> str | None:
    with _conn() as conn:
        row = conn.execute("SELECT logs FROM jobs WHERE id = ?", (job_id,)).fetchone()
    if row is None:
        return None
    return str(row["logs"])


def run_worker_loop(poll_seconds: float = JOB_POLL_SECONDS) -> None:
    init_db()
    while True:
        next_job = claim_next_queued_job()
        if next_job is None:
            time.sleep(poll_seconds)
            continue
        process_job(next_job, already_claimed=True)
