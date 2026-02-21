//! Starter strategy for the multi-AMM prop-amm challenge.
//!
//! Demonstrates all four new capabilities:
//!   1. Reading competitive context  (competing_spot_prices, flow_captured)
//!   2. Tracking volatility internally across trades within an epoch
//!   3. Responding to epoch boundary: reset vol estimate, note new capital weight
//!   4. Adjusting fees based on estimated vol AND flow capture rate
//!
//! Storage layout (each slot = 8 bytes = f64/u64):
//!   slot 0 : bid_fee_wad     — current bid fee (WAD)
//!   slot 1 : ask_fee_wad     — current ask fee (WAD)
//!   slot 2 : vol_estimate    — exponentially weighted σ estimate (f64 bits)
//!   slot 3 : last_price      — last observed spot price (f64 bits)
//!   slot 4 : flow_ema        — EMA of flow_captured (f64 bits)
//!   slot 5 : trade_count     — number of trades this epoch (u64)
//!   slot 6 : capital_weight  — most recent capital_weight (f64 bits)
//!   slot 7 : epoch_number    — current epoch (u64)

use prop_amm_submission_sdk::{
    AfterSwapContext, EpochContext, Storage, SwapContext,
    bps_to_wad, clamp_fee, cpamm_output_wad, read_f64, read_u64, write_f64, write_u64,
    set_return_data_u64, set_storage, WAD,
};

/// Displayed on the leaderboard.
pub const NAME: &str = "Multi-AMM Vol-Adaptive Starter";
pub const MODEL_USED: &str = "None";

// ─── Parameters ───────────────────────────────────────────────────────────────

/// Base fee (30 bps = competitive with normalizer lower bound)
const BASE_FEE_WAD: u64 = bps_to_wad(30);
/// Max additional fee from vol adjustment (200 bps)
const MAX_VOL_ADD_WAD: u64 = bps_to_wad(200);
/// Min fee (never go below 5 bps to avoid free arb)
const MIN_FEE_WAD: u64 = bps_to_wad(5);
/// Vol EMA decay (α ≈ 0.05 → ~20 trade half-life)
const VOL_ALPHA: f64 = 0.05;
/// Flow EMA decay (α ≈ 0.10)
const FLOW_ALPHA: f64 = 0.10;

// Storage slot indices
const S_BID_FEE:      usize = 0;
const S_ASK_FEE:      usize = 1;
const S_VOL_EST:      usize = 2;
const S_LAST_PRICE:   usize = 3;
const S_FLOW_EMA:     usize = 4;
const S_TRADE_COUNT:  usize = 5;
const S_CAPITAL_WT:   usize = 6;
const S_EPOCH_NUM:    usize = 7;

// ─── Entrypoint ───────────────────────────────────────────────────────────────

#[cfg(not(feature = "no-entrypoint"))]
#[no_mangle]
pub extern "C" fn __prop_amm_compute_swap(data: *const u8, len: usize) -> u64 {
    let bytes = unsafe { core::slice::from_raw_parts(data, len) };
    let ctx = match SwapContext::from_bytes(bytes) {
        Some(c) => c,
        None => return 0,
    };
    compute_swap(&ctx)
}

#[cfg(not(feature = "no-entrypoint"))]
#[no_mangle]
pub extern "C" fn __prop_amm_after_swap(data: *const u8, len: usize, storage_ptr: *mut u8) {
    let bytes   = unsafe { core::slice::from_raw_parts(data, len) };
    let storage = unsafe { &mut *(storage_ptr as *mut Storage) };

    if bytes.is_empty() { return; }
    match bytes[0] {
        2 => {
            if let Some(ctx) = AfterSwapContext::from_bytes(bytes) {
                after_swap(&ctx, storage);
            }
        }
        5 => {
            if let Some(ctx) = EpochContext::from_bytes(bytes) {
                on_epoch_boundary(&ctx, storage);
            }
        }
        _ => {}
    }
}

#[cfg(not(feature = "no-entrypoint"))]
#[no_mangle]
pub extern "C" fn __prop_amm_get_name(buf: *mut u8, max_len: usize) -> usize {
    let bytes = NAME.as_bytes();
    let n = bytes.len().min(max_len);
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n); }
    n
}

// ─── compute_swap ─────────────────────────────────────────────────────────────

/// Quote an output amount for a given input.
/// Reads bid/ask fee from storage and applies standard CPAMM formula.
pub fn compute_swap(ctx: &SwapContext) -> u64 {
    let fee = if ctx.is_buy {
        read_u64(&ctx.storage, S_BID_FEE).max(MIN_FEE_WAD)
    } else {
        read_u64(&ctx.storage, S_ASK_FEE).max(MIN_FEE_WAD)
    };

    // is_buy=true: Y→X (reserve_in=Y, reserve_out=X)
    // is_buy=false: X→Y (reserve_in=X, reserve_out=Y)
    let (reserve_in, reserve_out) = if ctx.is_buy {
        (ctx.reserve_y, ctx.reserve_x)
    } else {
        (ctx.reserve_x, ctx.reserve_y)
    };

    cpamm_output_wad(ctx.input_amount, reserve_in, reserve_out, fee)
}

// ─── after_swap ───────────────────────────────────────────────────────────────

/// Update storage after each trade. Adapts fees based on:
///   - Inferred volatility (from price moves implicit in reserve changes)
///   - Flow capture rate (are we winning routing competition?)
///   - Trade direction (widen the side we're being hit on)
pub fn after_swap(ctx: &AfterSwapContext, storage: &mut Storage) {
    // ── Current state ─────────────────────────────────────────────────────────
    let mut vol_est    = read_f64(storage, S_VOL_EST);
    let mut last_price = read_f64(storage, S_LAST_PRICE);
    let mut flow_ema   = read_f64(storage, S_FLOW_EMA);
    let mut trade_cnt  = read_u64(storage, S_TRADE_COUNT);

    // ── Initialize on first trade ─────────────────────────────────────────────
    if last_price == 0.0 {
        last_price = ctx.spot_price();
        vol_est    = 0.003; // 30 bps prior
        flow_ema   = 0.5;   // neutral prior
    }

    // ── Update vol estimate ───────────────────────────────────────────────────
    let current_spot = ctx.spot_price();
    if last_price > 0.0 {
        let ret = (current_spot / last_price).ln().abs();
        vol_est = VOL_ALPHA * ret + (1.0 - VOL_ALPHA) * vol_est;
    }
    last_price = current_spot;

    // ── Update flow EMA ───────────────────────────────────────────────────────
    // flow_captured = 0 on arb trades (treat as negative signal)
    let effective_flow = if ctx.flow_captured == 0.0 { 0.0 } else { ctx.flow_captured as f64 };
    flow_ema = FLOW_ALPHA * effective_flow + (1.0 - FLOW_ALPHA) * flow_ema;

    trade_cnt += 1;

    // ── Competitive context ───────────────────────────────────────────────────
    // Check if we are priced worse than competitors.
    // If spot prices of others are meaningfully different from ours, adjust.
    let n_competing = ctx.n_strategies.saturating_sub(1) as usize;
    let mut mean_comp_spot = 0.0_f64;
    let mut valid_comps = 0u32;
    for i in 0..n_competing.min(8) {
        let sp = ctx.competing_spot_prices[i];
        if sp.is_finite() && sp > 0.0 {
            mean_comp_spot += sp as f64;
            valid_comps += 1;
        }
    }
    let mean_comp_spot = if valid_comps > 0 { mean_comp_spot / valid_comps as f64 } else { current_spot };

    // Spread vs. competitor spot (positive = we're cheaper, attracting more flow)
    let rel_spread_vs_comp = (mean_comp_spot - current_spot) / mean_comp_spot.max(1e-12);

    // ── Fee computation ───────────────────────────────────────────────────────
    //
    // Target fee = BASE + vol_premium - flow_adjustment
    //
    // Vol premium: higher vol → widen spread (protect against arb)
    //   vol_premium = clamp(vol_est * 10_000 bps, 0, 200 bps)
    //
    // Flow adjustment: if we're losing flow (flow_ema < 0.3), reduce fees slightly
    //   if we're dominant (flow_ema > 0.7), raise fees slightly
    //
    // Directional adjustment: if last trade was a buy (trader bought X),
    //   widen ask slightly (we sold X, may be adversely selected)

    let vol_premium_bps = (vol_est * 10_000.0 * 100.0).min(200.0) as u64;
    let vol_premium_wad = bps_to_wad(vol_premium_bps);

    // Flow pressure adjustment (±10 bps)
    let flow_adj_wad: i64 = if flow_ema < 0.25 {
        -(bps_to_wad(10) as i64)  // losing flow → lower fees to attract retail
    } else if flow_ema > 0.70 {
        bps_to_wad(10) as i64     // dominant → can raise fees
    } else {
        0
    };

    // Directional side adjustment (±5 bps)
    let dir_adj_wad: i64 = if ctx.is_buy { bps_to_wad(5) as i64 } else { -(bps_to_wad(5) as i64) };

    let base_fee = BASE_FEE_WAD + vol_premium_wad;
    let bid_fee = clamp_fee(
        (base_fee as i64 + flow_adj_wad - dir_adj_wad).max(MIN_FEE_WAD as i64) as u64
    );
    let ask_fee = clamp_fee(
        (base_fee as i64 + flow_adj_wad + dir_adj_wad).max(MIN_FEE_WAD as i64) as u64
    );

    // ── Persist ───────────────────────────────────────────────────────────────
    write_u64(storage, S_BID_FEE, bid_fee);
    write_u64(storage, S_ASK_FEE, ask_fee);
    write_f64(storage, S_VOL_EST, vol_est);
    write_f64(storage, S_LAST_PRICE, last_price);
    write_f64(storage, S_FLOW_EMA, flow_ema);
    write_u64(storage, S_TRADE_COUNT, trade_cnt);
}

// ─── on_epoch_boundary ────────────────────────────────────────────────────────

/// Called at the start of each new epoch. Strategy can:
///   - Recalibrate based on epoch performance (received edge)
///   - Reset short-term state (vol estimate, trade count)
///   - Adjust aggressiveness based on new capital weight
pub fn on_epoch_boundary(ctx: &EpochContext, storage: &mut Storage) {
    // Reset vol estimate (partial — don't throw away everything)
    let old_vol = read_f64(storage, S_VOL_EST);
    let reset_vol = old_vol * 0.5 + 0.003 * 0.5;  // regress to prior

    // If we lost significant capital, become more aggressive (lower fees) to win flow back
    let cw = ctx.capital_weight as f64;
    let aggression_adj: i64 = if cw < 0.15 {
        -(bps_to_wad(5) as i64)  // lost capital → lower fees
    } else if cw > 0.50 {
        bps_to_wad(5) as i64      // dominant → raise fees
    } else {
        0
    };

    let old_bid = read_u64(storage, S_BID_FEE);
    let old_ask = read_u64(storage, S_ASK_FEE);
    let new_bid = clamp_fee((old_bid as i64 + aggression_adj).max(bps_to_wad(5) as i64) as u64);
    let new_ask = clamp_fee((old_ask as i64 + aggression_adj).max(bps_to_wad(5) as i64) as u64);

    write_f64(storage, S_VOL_EST, reset_vol);
    write_u64(storage, S_TRADE_COUNT, 0);
    write_u64(storage, S_BID_FEE, new_bid);
    write_u64(storage, S_ASK_FEE, new_ask);
    write_f64(storage, S_CAPITAL_WT, cw);
    write_u64(storage, S_EPOCH_NUM, ctx.epoch_number as u64);
}

pub fn get_model_used() -> &'static str { MODEL_USED }
