# AMM Multi-Strategy Competition Engine

Extends [prop-amm-challenge](https://github.com/benedictbrady/prop-amm-challenge) with:

1. **N-strategy simultaneous competition** — retail flow splits optimally across all submitted strategies + the normalizer at every step
2. **Epoch-based capital rebalancing** — reserves are redistributed proportional to risk-adjusted edge after each epoch
3. **Strategy state persistence across epochs** — 1024-byte storage carries forward; strategies receive `TAG_EPOCH_BOUNDARY` hook to reinitialize cleanly
4. **Enriched AfterSwap payload** — strategies can now observe `flow_captured`, `competing_spot_prices`, `capital_weight`, `epoch_step`, and `epoch_number`

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                         Simulation Engine (Rust)                              │
│                                                                              │
│  GBM Price Process → per step:                                               │
│                                                                              │
│    ┌─ Arbitrage ─────────────────────────────────────────────────────────┐  │
│    │  Golden-section search for optimal arb size, per AMM               │  │
│    │  Dispatches TAG_AFTER_SWAP to strategy after each arb trade        │  │
│    └─────────────────────────────────────────────────────────────────────┘  │
│                                                                              │
│    ┌─ Retail Routing (N-way equimarginal) ───────────────────────────────┐  │
│    │  Bisection on shadow price λ* such that Σ x_i(λ*) = total_input   │  │
│    │  x_i(λ) = largest input where marginal output rate ≥ λ            │  │
│    │  Each AMM called via compute_swap for quoting (no storage update)  │  │
│    │  After routing: TAG_AFTER_SWAP with flow_captured, competitors     │  │
│    └─────────────────────────────────────────────────────────────────────┘  │
│                                                                              │
│    ┌─ Epoch Boundary (every epoch_len steps) ───────────────────────────┐  │
│    │  Compute risk_adj_score = edge - λ·max(0,-edge)                   │  │
│    │  Softmax capital weights with temperature T and floor w_min        │  │
│    │  Scale reserves: new_ry_i = total_capital * w_i / n               │  │
│    │  Dispatch TAG_EPOCH_BOUNDARY with new reserves, edge, weight       │  │
│    └─────────────────────────────────────────────────────────────────────┘  │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## N-Way Routing Math

For N AMMs with arbitrary concave pricing functions f₁…fₙ, we maximize total output:

```
max_{α₁…αₙ, Σαᵢ=1}  Σᵢ fᵢ(αᵢ · X_total)
```

**Optimality condition** (equimarginal principle):

```
∂fᵢ/∂xᵢ = λ*  for all i with xᵢ > 0
```

**Algorithm**: binary search on the shadow price λ*:
- For each λ, compute `xᵢ(λ)` = largest input where marginal outputᵢ ≥ λ (inner bisection)
- Find λ* such that `Σ xᵢ(λ*) = X_total` (outer bisection)
- Complexity: `O(N · 60 · 60)` evaluations per retail order — fast for N ≤ 16

---

## Capital Allocation

After each epoch of `epoch_len` steps:

**Risk-adjusted score:**
```
score_i = edge_i - λ · max(0, -edge_i)
```
Default λ=2.0: a loss of −X scores as −3X (asymmetric downside penalty).

**Capital weights** (temperature-scaled softmax with floor):
```
w_i = softmax(score_i / T),  clipped to [w_min, 1.0],  renormalized
```
Default T=1.0, w_min=2%.

**Reserve rebalancing:**
- Total Y-denominated capital = Σᵢ 2·reserve_yᵢ (factor 2 for balanced pool)
- New reserve_yᵢ = total_capital · wᵢ / N
- Spot price preserved: new_reserve_xᵢ = new_reserve_yᵢ / spotᵢ

---

## Storage Layout (128 u64 slots, 1024 bytes)

Storage persists across ALL trades within a simulation AND across epoch boundaries.
The starter strategy uses:

| Slot | Field            | Type | Description                        |
|------|------------------|------|------------------------------------|
| 0    | bid_fee_wad      | u64  | Current bid fee (WAD scale)        |
| 1    | ask_fee_wad      | u64  | Current ask fee (WAD scale)        |
| 2    | vol_estimate     | f64  | EMA of |log return|                |
| 3    | last_price       | f64  | Last observed spot price           |
| 4    | flow_ema         | f64  | EMA of flow_captured               |
| 5    | trade_count      | u64  | Trades this epoch                  |
| 6    | capital_weight   | f64  | Most recent capital weight         |
| 7    | epoch_number     | u64  | Current epoch index                |

Slots 8–127 are free for your strategy.

---

## AfterSwap Payload (Tag = 2)  — Enriched vs. Original

| Offset | Type  | Field                 | New? | Description                                      |
|--------|-------|-----------------------|------|--------------------------------------------------|
| 0      | u8    | tag                   |      | Always 2                                         |
| 1      | u8    | side                  |      | 0=buy X, 1=sell X                                |
| 2      | u64   | input_amount          |      | Input (1e9 scale)                                |
| 10     | u64   | output_amount         |      | Output (1e9 scale)                               |
| 18     | u64   | reserve_x             |      | Post-trade X reserve                             |
| 26     | u64   | reserve_y             |      | Post-trade Y reserve                             |
| 34     | u64   | sim_step              |      | Global step (0..10000)                           |
| 42     | u32   | epoch_step            | ★   | Step within current epoch (resets each epoch)    |
| 46     | u32   | epoch_number          | ★   | Epoch index (0-based)                            |
| 50     | u8    | n_strategies          | ★   | Total AMMs competing (incl. normalizer)          |
| 51     | u8    | strategy_index        | ★   | This strategy's index                            |
| 52     | f32   | flow_captured         | ★   | Fraction of this order routed here (0=arb trade) |
| 56     | f32   | capital_weight        | ★   | This AMM's fraction of total capital             |
| 60     | f32×8 | competing_spot_prices | ★   | Other AMMs' spot prices (NaN if unused)          |
| 92     | [u8;1024] | storage           |      | Read-write strategy storage                      |

---

## Epoch Boundary Payload (Tag = 5) — New

| Offset | Type  | Field            | Description                               |
|--------|-------|------------------|-------------------------------------------|
| 0      | u8    | tag              | Always 5                                  |
| 1      | u32   | epoch_number     | Just-completed epoch index                |
| 5      | u64   | new_reserve_x    | Reserves after rebalancing                |
| 13     | u64   | new_reserve_y    |                                           |
| 21     | f64   | epoch_edge       | Edge earned in the epoch that just ended  |
| 29     | f64   | cumulative_edge  | Total edge to date                        |
| 37     | f32   | capital_weight   | New capital allocation weight             |
| 41     | [u8;1024] | storage      | Read-write (persists)                     |

---

## Quick Start

```bash
# Build + test
cargo test

# Validate strategy source files (compiles to local dylibs)
cargo run --bin prop-amm-multi -- validate submission_0.rs

# Run simulations for one or more strategies
cargo run --bin prop-amm-multi -- run submission_0.rs submission_1.rs --simulations 100 --steps 5000 --epoch-len 500

# Create a local submission bundle + receipt.json
cargo run --bin prop-amm-multi -- submit submission_0.rs submission_1.rs --simulations 250 --steps 10000 --epoch-len 1000

# Example output:
# Strategy                         Mean Edge    Std Edge    vs Norm  Sharpe   Final Cap%
# -----------------------------------------------------------------------------------
# Multi-AMM Vol-Adaptive Starter    +487.23      112.45     +201.44   4.331       38.2%
# [Normalizer]                      +285.79      ...
```

## Dashboard + API Quick Start

### Safe Local Process Management (recommended)

Use project-scoped scripts so this repo never kills or hijacks other projects:

```bash
make dev-up
make dev-status
```

This uses dedicated defaults to reduce collisions:

- API: `127.0.0.1:18002`
- Dashboard: `127.0.0.1:15173`

Override ports per run if needed:

```bash
PROP_AMM_API_PORT=19002 PROP_AMM_DASHBOARD_PORT=16173 make dev-up
```

Stop only processes started by this project (via PID files in `.run/`):

```bash
make dev-down
```

Run from the project root:

```bash
# 1) Python env for backend/worker
python -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt

# 2) Ensure Python can import apps.* modules
export PYTHONPATH="$PWD"

# 3) Start API (terminal A)
uvicorn apps.backend.app.main:app --host 0.0.0.0 --port 18002

# 4) Start worker (terminal B)
python -m apps.worker.worker

# 5) Start dashboard (terminal C)
cd apps/dashboard
npm install
npm run dev
```

Dashboard default endpoint is `http://127.0.0.1:18002`.

### One-time Setup (Recommended)

```bash
cp .env.example .env
make setup
```

Then run each service in its own terminal:

```bash
PROP_AMM_API_PORT=18002 make api
make worker
make dashboard
```

### API Endpoints

- `GET /`
- `GET /health`
- `GET /healthz`
- `POST /api/jobs` with `job_type` = `validate | run | submit`
- `GET /api/jobs/{job_id}`
- `GET /api/jobs/{job_id}/logs`
- `GET /api/leaderboard`

## Nohup (VM mode)

```bash
cd deploy/docker
bash run-nohup.sh
```

This starts backend + worker + dashboard safely and writes logs to `.run/`.

## Deployment Tracks

- Docker path: `deploy/docker`
- Render path: `deploy/render`

### Docker (production-like local)

```bash
make docker-up
```

- Backend API: `http://localhost:18002`
- Dashboard: `http://localhost:15173`

### Render (managed)

- Blueprint file: `deploy/render/render.yaml`
- Includes three services: dashboard web, backend web, worker.
- Backend and worker are separate services (queue mode).
- Set dashboard `VITE_API_BASE_URL` to your backend URL.
- Set backend `PROP_AMM_CORS_ORIGINS` to your dashboard URL.

## Architecture Frictions (Important)

- Browser UX cannot execute shell commands directly; all terminal-like actions must go through backend job APIs.
- Current strategy execution uses native dynamic libraries; this is fast for trusted/local use, but requires hard sandboxing before public multi-tenant hosting.
- Render services do not use `nohup`; process lifecycle is managed by Render.
- Current artifacts are local filesystem outputs; for hosted persistence, move receipts/artifacts to durable storage.

## Production Notes

- API is queue-oriented by default (`PROP_AMM_INLINE_JOB_EXECUTION=false`) to avoid duplicate processing when worker service is running.
- If you need single-process local mode, set `PROP_AMM_INLINE_JOB_EXECUTION=true`.
- CORS is env-driven with `PROP_AMM_CORS_ORIGINS`.
- Max inline source size is controlled by `PROP_AMM_MAX_SOURCE_BYTES`.

---

## Strategy Design Hints

**Information you now have that the original challenge didn't expose:**

- `flow_captured`: if this is consistently < 0.2, your fees are too high — you're routing away retail
- `competing_spot_prices`: if competitor spots are far from yours, you're mispriced relative to the market
- `capital_weight`: if this trends down epoch-over-epoch, your risk-adjusted score is negative — check for large arb losses
- `epoch_step`: reset vol estimates at epoch boundaries to avoid stale state contaminating capital allocation

**Key tensions:**
- Higher fees → fewer arb losses, fewer retail trades
- Lower fees → more retail flow, but tighter margin per trade + more capital from winners
- Epoch boundary: a negative epoch permanently reduces your capital, compounding the disadvantage

**Advanced: RL / gradient-free optimization**

The MDP is now:
```
State:  (σ̂, flow_ema, capital_weight, epoch_step, n_strategies)
Action: (bid_fee_wad, ask_fee_wad)
Reward: epoch_edge - λ · max(0, -epoch_edge)
```

The capital feedback loop makes this a **multi-agent competitive MDP** — optimal policy depends on what other strategies are doing.
