//! Multi-AMM simulation engine.
//!
//! Extends the prop-amm-challenge framework to support:
//!   1. N competing strategies sharing retail order flow simultaneously
//!   2. Epoch-based capital rebalancing driven by risk-adjusted edge scores
//!   3. Strategy state persistence across epoch boundaries (TAG_EPOCH_BOUNDARY hook)
//!   4. Enriched AfterSwap payload exposing competitive context to each strategy

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use crate::capital::rebalance_capital;
use crate::market::{
    gbm_step, generate_retail_orders, optimal_arb_trade, route_order_n_amms,
    apply_cpamm_trade,
};
use crate::runner::{NormalizerRunner, StrategyRunner};
use crate::types::{
    AfterSwapPayload, AmmState, EpochBoundaryPayload, EpochSummary, SimConfig,
    SCALE_F, TAG_AFTER_SWAP, TAG_EPOCH_BOUNDARY,
};
use crate::market::MarketParams;

// ─── Simulation Result ────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct StrategyResult {
    pub name: String,
    pub final_edge: f64,
    pub epoch_summaries: Vec<EpochSummary>,
    pub final_capital_weight: f64,
}

#[derive(Clone, Debug)]
pub struct SimResult {
    pub strategies: Vec<StrategyResult>,
    pub normalizer_edge: f64,
    pub market_params: MarketParams,
}

// ─── Core Simulation ──────────────────────────────────────────────────────────

/// Run one complete multi-epoch simulation with N strategies + 1 normalizer.
///
/// `runners` — one compiled StrategyRunner per strategy (loaded before calling).
/// The normalizer is always appended as the last AMM internally.
pub fn run_simulation(
    runners: &[StrategyRunner],
    config: &SimConfig,
    seed: u64,
) -> SimResult {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);

    // ── 1. Sample market parameters ────────────────────────────────────────────
    let params = MarketParams::sample(&mut rng);
    let norm = NormalizerRunner { fee_bps: params.norm_fee_bps };

    // ── 2. Initialise AMM states ───────────────────────────────────────────────
    // Strategies share equal initial capital; normalizer gets its sampled multiplier.
    let n_strat = runners.len();

    let mut strat_amms: Vec<AmmState> = runners.iter().enumerate().map(|(i, r)| {
        let mut s = AmmState::new(config.base_reserve_x, config.base_reserve_y, i as u8, &r.name);
        s.capital_weight = 1.0 / n_strat as f64;
        s
    }).collect();

    let norm_rx = ((config.base_reserve_x as f64) * params.norm_liquidity_mult) as u64;
    let norm_ry = ((config.base_reserve_y as f64) * params.norm_liquidity_mult) as u64;
    let mut norm_amm = AmmState::new(norm_rx, norm_ry, n_strat as u8, "Normalizer");

    // ── 3. Epoch tracking ──────────────────────────────────────────────────────
    let mut all_epoch_summaries: Vec<Vec<EpochSummary>> = vec![vec![]; n_strat];

    let mut fair_price = config.base_reserve_y as f64 / config.base_reserve_x as f64;

    // ── 4. Main simulation loop ────────────────────────────────────────────────
    for step in 0..config.total_steps {
        // ── 4a. Price step ────────────────────────────────────────────────────
        fair_price = gbm_step(fair_price, params.sigma, &mut rng);

        // ── 4b. Arbitrage each strategy AMM ───────────────────────────────────
        for idx in 0..n_strat {
            let strat_snapshot = strat_amms.to_vec();
            let runner = &runners[idx];
            let amm = &mut strat_amms[idx];
            let cs = |is_buy: bool, input: u64, rx: u64, ry: u64| -> u64 {
                runner.compute_swap(is_buy, input, rx, ry, &amm.storage)
            };

            if let Some((is_buy, arb_in, arb_out)) =
                optimal_arb_trade(amm, fair_price, config.arb_profit_floor, cs)
            {
                amm.accrue_edge(
                    if is_buy { arb_out } else { arb_in },
                    if is_buy { arb_in } else { arb_out },
                    is_buy,
                    fair_price,
                );
                apply_cpamm_trade(&mut amm.reserve_x, &mut amm.reserve_y, is_buy, arb_in, arb_out);

                // Notify strategy of arb trade
                dispatch_after_swap(
                    runner, amm, is_buy, arb_in, arb_out,
                    step as u64, step as u32 % config.epoch_len as u32,
                    (step / config.epoch_len) as u32,
                    0.0, // arb trade: not a retail split
                    &strat_snapshot, &norm_amm,
                    n_strat,
                );
            }
        }

        // Arbitrage normalizer (plain CPAMM)
        arb_normalizer(&mut norm_amm, &norm, fair_price, config.arb_profit_floor);

        // ── 4c. Retail order routing ──────────────────────────────────────────
        let orders = generate_retail_orders(&params, &mut rng);
        for order in &orders {
            route_retail_order(
                order.is_buy,
                order.size_y,
                &mut strat_amms,
                &mut norm_amm,
                &norm,
                runners,
                fair_price,
                step,
                config,
            );
        }

        // ── 4d. Epoch boundary ────────────────────────────────────────────────
        let at_epoch_end = (step + 1) % config.epoch_len == 0;
        let last_step = step == config.total_steps - 1;

        if at_epoch_end && !last_step {
            let epoch_number = ((step + 1) / config.epoch_len) as u32;
            let summaries = rebalance_capital(&mut strat_amms, config, epoch_number - 1);

            // Notify each strategy of epoch boundary + new capital
            for (idx, (runner, amm)) in runners.iter().zip(strat_amms.iter_mut()).enumerate() {
                let payload = EpochBoundaryPayload {
                    tag: TAG_EPOCH_BOUNDARY,
                    epoch_number: epoch_number - 1,
                    new_reserve_x: amm.reserve_x,
                    new_reserve_y: amm.reserve_y,
                    epoch_edge: summaries[idx].edge,
                    cumulative_edge: amm.cumulative_edge,
                    capital_weight: amm.capital_weight as f32,
                    storage: amm.storage, // placeholder — real storage passed via runner
                };
                runner.epoch_boundary(&payload, &mut amm.storage);
            }

            for (idx, s) in summaries.into_iter().enumerate() {
                all_epoch_summaries[idx].push(s);
            }
        }
    }

    // ── 5. Build result ────────────────────────────────────────────────────────
    let strategies: Vec<StrategyResult> = strat_amms.iter().enumerate().map(|(i, amm)| {
        StrategyResult {
            name: amm.name.clone(),
            final_edge: amm.cumulative_edge,
            epoch_summaries: all_epoch_summaries[i].clone(),
            final_capital_weight: amm.capital_weight,
        }
    }).collect();

    SimResult {
        strategies,
        normalizer_edge: norm_amm.cumulative_edge,
        market_params: params,
    }
}

// ─── Retail Order Routing (N strategies + normalizer) ────────────────────────

#[allow(clippy::too_many_arguments)]
fn route_retail_order(
    is_buy: bool,
    size_y: f64,       // order size in Y-denomination (unscaled)
    strat_amms: &mut [AmmState],
    norm_amm: &mut AmmState,
    norm: &NormalizerRunner,
    runners: &[StrategyRunner],
    fair_price: f64,
    step: usize,
    config: &SimConfig,
) {
    let n_strat = strat_amms.len();
    // Total N+1 AMMs: strategies + normalizer
    // We route across all of them simultaneously.

    // We need to gather a snapshot of reserves for the router call
    // (immutable view), then apply mutations after.
    let all_amm_refs: Vec<AmmState> = strat_amms
        .iter()
        .cloned()
        .chain(std::iter::once(norm_amm.clone()))
        .collect();

    let total_n = all_amm_refs.len();

    // Unified compute_swap: dispatches to strategy runner or normalizer by index
    // We pass reserves explicitly so the router sees the current state.
    let compute_for_router = |amm_idx: usize, is_b: bool, input: u64, rx: u64, ry: u64| -> u64 {
        if amm_idx < n_strat {
            runners[amm_idx].compute_swap(is_b, input, rx, ry, &strat_amms[amm_idx].storage)
        } else {
            norm.compute_swap(is_b, input, rx, ry)
        }
    };

    // Convert size_y to appropriate input depending on direction.
    // is_buy=true: trader buys X, pays Y → Y is input, size_y is direct
    // is_buy=false: trader sells X for Y → X is input. Approx X size = size_y / fair_price
    let total_input = if is_buy { size_y } else { size_y / fair_price };

    let routing = route_order_n_amms(
        &all_amm_refs,
        is_buy,
        total_input,
        compute_for_router,
    );

    let total_input_scaled = (total_input * SCALE_F) as u64;

    // Apply trades and accounting
    for amm_idx in 0..total_n {
        let (input_scaled, output_scaled) = routing.allocations[amm_idx];
        if input_scaled == 0 { continue; }

            let flow_captured = input_scaled as f32 / total_input_scaled.max(1) as f32;

        if amm_idx < n_strat {
            let strat_snapshot = strat_amms.to_vec();
            let amm = &mut strat_amms[amm_idx];
            amm.accrue_edge(
                if is_buy { output_scaled } else { input_scaled },
                if is_buy { input_scaled }  else { output_scaled },
                is_buy,
                fair_price,
            );
            apply_cpamm_trade(&mut amm.reserve_x, &mut amm.reserve_y, is_buy, input_scaled, output_scaled);

            let epoch_step = step as u32 % config.epoch_len as u32;
            let epoch_number = (step / config.epoch_len) as u32;

            dispatch_after_swap(
                &runners[amm_idx],
                amm,
                is_buy,
                input_scaled,
                output_scaled,
                step as u64,
                epoch_step,
                epoch_number,
                flow_captured,
                &strat_snapshot,
                norm_amm,
                total_n,
            );
        } else {
            // Normalizer accounting
            norm_amm.accrue_edge(
                if is_buy { output_scaled } else { input_scaled },
                if is_buy { input_scaled }  else { output_scaled },
                is_buy,
                fair_price,
            );
            apply_cpamm_trade(&mut norm_amm.reserve_x, &mut norm_amm.reserve_y,
                               is_buy, input_scaled, output_scaled);
        }
    }
}

// ─── AfterSwap Dispatch ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn dispatch_after_swap(
    runner: &StrategyRunner,
    amm: &mut AmmState,
    is_buy: bool,
    input: u64,
    output: u64,
    sim_step: u64,
    epoch_step: u32,
    epoch_number: u32,
    flow_captured: f32,
    all_strat: &[AmmState],
    norm: &AmmState,
    total_n: usize,
) {
    // Build competing spot prices (all other AMMs)
    let mut competing = [f32::NAN; 8];
    let mut slot = 0;
    for s in all_strat {
        if s.strategy_index != amm.strategy_index && slot < 8 {
            competing[slot] = s.spot_price() as f32;
            slot += 1;
        }
    }
    if slot < 8 {
        competing[slot] = norm.spot_price() as f32;
    }

    let payload = AfterSwapPayload {
        tag: TAG_AFTER_SWAP,
        side: if is_buy { 0 } else { 1 },
        input_amount: input,
        output_amount: output,
        reserve_x: amm.reserve_x,
        reserve_y: amm.reserve_y,
        sim_step,
        epoch_step,
        epoch_number,
        n_strategies: total_n as u8,
        strategy_index: amm.strategy_index,
        flow_captured,
        capital_weight: amm.capital_weight as f32,
        competing_spot_prices: competing,
        storage: amm.storage,
    };

    runner.after_swap(&payload, &mut amm.storage);
}

// ─── Normalizer Arb (inline, no library call) ─────────────────────────────────

fn arb_normalizer(norm: &mut AmmState, runner: &NormalizerRunner, fair_price: f64, floor: f64) {
    use crate::market::golden_section_max;

    let spot = norm.spot_price();
    let is_buy = spot > fair_price;

    let max_in = if is_buy {
        norm.reserve_y as f64 * 0.9 / SCALE_F
    } else {
        norm.reserve_x as f64 * 0.9 / SCALE_F
    };

    let profit_fn = |input_f: f64| -> f64 {
        let input_scaled = (input_f * SCALE_F) as u64;
        if input_scaled == 0 { return 0.0; }
        let out = runner.compute_swap(is_buy, input_scaled, norm.reserve_x, norm.reserve_y);
        let out_f = out as f64 / SCALE_F;
        if is_buy { out_f * fair_price - input_f } else { out_f - input_f * fair_price }
    };

    let (best_in, best_profit) = golden_section_max(profit_fn, 0.0, max_in, 50);
    if best_profit < floor || best_in < 1.0 / SCALE_F { return; }

    let input_scaled = (best_in * SCALE_F) as u64;
    let out_scaled = runner.compute_swap(is_buy, input_scaled, norm.reserve_x, norm.reserve_y);

    norm.accrue_edge(
        if is_buy { out_scaled } else { input_scaled },
        if is_buy { input_scaled } else { out_scaled },
        is_buy, fair_price,
    );
    apply_cpamm_trade(&mut norm.reserve_x, &mut norm.reserve_y, is_buy, input_scaled, out_scaled);
}

// ─── Parallel Multi-simulation Runner ────────────────────────────────────────

use rayon::prelude::*;

/// Run `n_sims` simulations in parallel, return aggregated results per strategy.
pub fn run_parallel(
    runner_paths: &[std::path::PathBuf],
    config: &SimConfig,
    n_sims: usize,
    seed_start: u64,
) -> Vec<AggregatedResult> {
    let results: Vec<SimResult> = (0..n_sims)
        .into_par_iter()
        .map(|i| {
            // Each thread loads its own strategy runners (libloading is not Send)
            let runners: Vec<StrategyRunner> = runner_paths
                .iter()
                .map(|p| StrategyRunner::load(p).expect("strategy load failed"))
                .collect();
            run_simulation(&runners, config, seed_start + i as u64)
        })
        .collect();

    aggregate_results(results)
}

#[derive(Clone, Debug)]
pub struct AggregatedResult {
    pub name: String,
    pub mean_edge: f64,
    pub std_edge: f64,
    pub mean_final_capital_weight: f64,
    pub edge_vs_normalizer: f64,   // mean (strategy_edge - normalizer_edge)
    pub sharpe: f64,               // mean_edge / std_edge
}

fn aggregate_results(sims: Vec<SimResult>) -> Vec<AggregatedResult> {
    if sims.is_empty() { return vec![]; }
    let n_strat = sims[0].strategies.len();
    let n = sims.len() as f64;

    (0..n_strat).map(|i| {
        let edges: Vec<f64> = sims.iter().map(|s| s.strategies[i].final_edge).collect();
        let norm_edges: Vec<f64> = sims.iter().map(|s| s.normalizer_edge).collect();
        let weights: Vec<f64> = sims.iter().map(|s| s.strategies[i].final_capital_weight).collect();

        let mean = edges.iter().sum::<f64>() / n;
        let var  = edges.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / n;
        let std  = var.sqrt();
        let mean_norm = norm_edges.iter().sum::<f64>() / n;
        let mean_wt   = weights.iter().sum::<f64>() / n;

        AggregatedResult {
            name: sims[0].strategies[i].name.clone(),
            mean_edge: mean,
            std_edge: std,
            mean_final_capital_weight: mean_wt,
            edge_vs_normalizer: mean - mean_norm,
            sharpe: if std > 0.0 { mean / std } else { 0.0 },
        }
    }).collect()
}
