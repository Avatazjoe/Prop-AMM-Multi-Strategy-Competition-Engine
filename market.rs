use rand::Rng;
use rand_chacha::ChaCha8Rng;
use rand_distr::{Distribution, LogNormal, Poisson};

use crate::types::{AmmState, SCALE_F};

// ─── GBM Price Process ────────────────────────────────────────────────────────

/// Advance fair price by one GBM step.
///
/// S(t+1) = S(t) * exp(-σ²/2 + σ·Z),  Z ~ N(0,1)
#[inline]
pub fn gbm_step(price: f64, sigma: f64, rng: &mut ChaCha8Rng) -> f64 {
    let z: f64 = rng.sample(rand_distr::StandardNormal);
    price * (-0.5 * sigma * sigma + sigma * z).exp()
}

// ─── Market Parameters (sampled once per simulation) ─────────────────────────

#[derive(Clone, Debug)]
pub struct MarketParams {
    /// Per-step volatility
    pub sigma: f64,
    /// Retail Poisson arrival rate (orders per step)
    pub lambda: f64,
    /// Log-normal mean order size (in Y, unscaled)
    pub order_size_mean: f64,
    /// Normalizer fee in bps
    pub norm_fee_bps: u32,
    /// Normalizer liquidity multiplier (scales initial reserves)
    pub norm_liquidity_mult: f64,
}

impl MarketParams {
    /// Sample fresh parameters for a new simulation using the provided RNG.
    pub fn sample(rng: &mut ChaCha8Rng) -> Self {
        let sigma = rng.gen_range(0.0001f64..=0.0070);   // U[0.01%, 0.70%]
        let lambda = rng.gen_range(0.4f64..=1.2);
        let order_size_mean = rng.gen_range(12.0f64..=28.0);
        let norm_fee_bps = rng.gen_range(30u32..=80);
        let norm_liquidity_mult = rng.gen_range(0.4f64..=2.0);

        Self { sigma, lambda, order_size_mean, norm_fee_bps, norm_liquidity_mult }
    }
}

// ─── Retail Order Generation ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct RetailOrder {
    /// true = buy X (Y is input), false = sell X (X is input)
    pub is_buy: bool,
    /// Order size in Y terms (unscaled, use directly as f64)
    pub size_y: f64,
}

/// Generate retail orders for one step.
/// Returns 0 or more orders (Poisson count), each with LogNormal size.
pub fn generate_retail_orders(params: &MarketParams, rng: &mut ChaCha8Rng) -> Vec<RetailOrder> {
    // Poisson arrival count
    let count = {
        let pois = Poisson::new(params.lambda).unwrap();
        pois.sample(rng) as usize
    };

    // LogNormal parameters: want E[X] = order_size_mean, σ_ln = 1.2
    // E[X] = exp(μ + σ²/2) → μ = ln(E[X]) - σ²/2
    let sigma_ln = 1.2_f64;
    let mu_ln = params.order_size_mean.ln() - 0.5 * sigma_ln * sigma_ln;
    let ln_dist = LogNormal::new(mu_ln, sigma_ln).unwrap();

    (0..count)
        .map(|_| RetailOrder {
            is_buy: rng.gen_bool(0.5),
            size_y: ln_dist.sample(rng),
        })
        .collect()
}

// ─── Arbitrage ────────────────────────────────────────────────────────────────

/// Compute the optimal arb trade size for a CPAMM-like AMM using golden-section search.
///
/// The arb profit function for a given input trade size Δ is:
///   profit(Δ) = output(Δ) * fair_price_reciprocal - Δ   (buy X case)
/// which is concave → golden-section search finds the max.
///
/// Returns (optimal_input_in_Y, expected_profit_in_Y) for a buy-X arb,
/// or (optimal_input_in_X, expected_profit_in_X) for a sell-X arb.
///
/// `compute_swap_fn`: takes (is_buy: bool, input_scaled: u64, rx: u64, ry: u64) → output_scaled: u64
pub fn optimal_arb_trade<F>(
    amm: &AmmState,
    fair_price: f64,
    arb_profit_floor: f64,
    compute_swap: F,
) -> Option<(bool, u64, u64)>  // (is_buy, input_scaled, output_scaled)
where
    F: Fn(bool, u64, u64, u64) -> u64,
{
    let rx = amm.reserve_x as f64;
    let ry = amm.reserve_y as f64;
    let spot = ry / rx;

    // Determine arb direction
    // Spot = Y/X price of X in Y terms.
    // If spot > fair_price: AMM charges too much Y per X → arb sells X to AMM (buys X cheap externally)
    //   Wait. Spot = ry/rx = "how many Y you get per X from AMM".
    //   If spot > fair: AMM gives more Y per X than fair → arb BUYS X from AMM (is_buy=true, Y→X)
    // If spot < fair: AMM gives less Y per X → arb SELLS X to AMM (is_buy=false, X→Y)
    let is_buy_x = spot > fair_price;

    // Golden-section search for max profit
    let max_input = if is_buy_x {
        ry * 0.9  // can't drain more than 90% of reserves
    } else {
        rx * 0.9
    };

    let profit_fn = |input_f: f64| -> f64 {
        let input_scaled = (input_f * SCALE_F) as u64;
        if input_scaled == 0 { return 0.0; }
        let output_scaled = compute_swap(is_buy_x, input_scaled, amm.reserve_x, amm.reserve_y);
        let output_f = output_scaled as f64 / SCALE_F;
        if is_buy_x {
            // Pay Y, receive X. Profit in Y = output_x * fair_price - input_y
            output_f * fair_price - input_f
        } else {
            // Pay X, receive Y. Profit in Y = output_y - input_x * fair_price
            output_f - input_f * fair_price
        }
    };

    let (best_input, best_profit) = golden_section_max(profit_fn, 0.0, max_input, 50);

    if best_profit < arb_profit_floor || best_input < 1.0 / SCALE_F {
        return None;
    }

    let input_scaled = (best_input * SCALE_F) as u64;
    let output_scaled = compute_swap(is_buy_x, input_scaled, amm.reserve_x, amm.reserve_y);
    Some((is_buy_x, input_scaled, output_scaled))
}

// ─── N-way Optimal Router ─────────────────────────────────────────────────────

/// Result of routing one retail order across N AMMs.
#[derive(Clone, Debug)]
pub struct RoutingResult {
    /// For each AMM: (input_scaled, output_scaled), 0 if no allocation
    pub allocations: Vec<(u64, u64)>,
    /// Total output across all AMMs (in output token, scaled)
    pub total_output: u64,
}

/// Route a retail order of `total_input_y` (unscaled f64) optimally across N AMMs.
///
/// Uses the **equimarginal principle**: at the optimum, marginal output per unit input
/// is equal across all AMMs receiving flow. We implement this via bisection on the
/// shadow price λ*, where each AMM's allocation satisfies marginal_output(x_i) = λ*.
///
/// For each AMM i, we find x_i(λ) = argmax{output_i(x) : marginal_i(x) >= λ}.
/// Binary search on λ until Σ x_i(λ) ≈ total_input.
///
/// This is O(N · K · log(1/ε)) where K=50 bisection iterations.
pub fn route_order_n_amms<F>(
    amms: &[AmmState],
    is_buy: bool,   // true = Y→X (buy X), false = X→Y (sell X)
    total_input: f64,  // unscaled Y (if is_buy) or X (if !is_buy)
    compute_swap: F,   // (amm_idx, is_buy, input_scaled, rx, ry) → output_scaled
) -> RoutingResult
where
    F: Fn(usize, bool, u64, u64, u64) -> u64,
{
    let n = amms.len();
    if n == 0 { return RoutingResult { allocations: vec![], total_output: 0 }; }
    if n == 1 {
        let input_scaled = (total_input * SCALE_F) as u64;
        let out = compute_swap(0, is_buy, input_scaled, amms[0].reserve_x, amms[0].reserve_y);
        return RoutingResult {
            allocations: vec![(input_scaled, out)],
            total_output: out,
        };
    }

    // Marginal output function for AMM i at input x (unscaled f64)
    // m_i(x) = (f_i(x+δ) - f_i(x)) / δ  — numerical derivative
    let marginal = |i: usize, x: f64| -> f64 {
        let delta = x * 0.001 + 1.0 / SCALE_F;
        let o1 = compute_swap(i, is_buy, (x * SCALE_F) as u64, amms[i].reserve_x, amms[i].reserve_y) as f64 / SCALE_F;
        let o2 = compute_swap(i, is_buy, ((x + delta) * SCALE_F) as u64, amms[i].reserve_x, amms[i].reserve_y) as f64 / SCALE_F;
        (o2 - o1) / delta
    };

    // For a given shadow price λ, find how much input AMM i would absorb
    // x_i(λ) = largest x such that marginal_i(x) >= λ
    // Uses bisection: marginal is decreasing (concavity requirement).
    let allocation_at_shadow = |i: usize, lambda: f64| -> f64 {
        let max_in = if is_buy { amms[i].reserve_y as f64 * 0.9 / SCALE_F }
                     else      { amms[i].reserve_x as f64 * 0.9 / SCALE_F };

        // If even marginal at 0 is below lambda, this AMM gets no flow
        if marginal(i, 1.0 / SCALE_F) < lambda { return 0.0; }
        // If even at max_in marginal is above lambda, give it the full remaining
        if marginal(i, max_in) >= lambda { return max_in; }

        // Binary search for x where marginal(x) = lambda
        let mut lo = 0.0_f64;
        let mut hi = max_in;
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if marginal(i, mid) >= lambda { lo = mid; } else { hi = mid; }
            if (hi - lo) / (hi + lo + 1e-12) < 1e-6 { break; }
        }
        0.5 * (lo + hi)
    };

    // Binary search on λ: find λ* such that Σ x_i(λ*) = total_input
    // λ range: [0, max_marginal_at_zero] where max_marginal is the best initial marginal
    let lambda_max = (0..n)
        .map(|i| marginal(i, 1.0 / SCALE_F))
        .fold(0.0_f64, f64::max);

    let mut lo_lambda = 0.0_f64;
    let mut hi_lambda = lambda_max * 1.5;

    for _ in 0..80 {
        let mid = 0.5 * (lo_lambda + hi_lambda);
        let total: f64 = (0..n).map(|i| allocation_at_shadow(i, mid)).sum();
        if total > total_input { hi_lambda = mid; } else { lo_lambda = mid; }
        if (hi_lambda - lo_lambda) / (hi_lambda + lo_lambda + 1e-12) < 1e-6 { break; }
    }

    let lambda_star = 0.5 * (lo_lambda + hi_lambda);
    let raw_allocs: Vec<f64> = (0..n).map(|i| allocation_at_shadow(i, lambda_star)).collect();

    // Normalize to ensure total_input constraint is satisfied exactly
    let raw_sum: f64 = raw_allocs.iter().sum();
    let scale = if raw_sum > 1e-12 { total_input / raw_sum } else { 0.0 };

    let mut total_output: u64 = 0;
    let allocations: Vec<(u64, u64)> = (0..n).map(|i| {
        let input_f = raw_allocs[i] * scale;
        let input_scaled = (input_f * SCALE_F) as u64;
        if input_scaled == 0 {
            return (0, 0);
        }
        let out = compute_swap(i, is_buy, input_scaled, amms[i].reserve_x, amms[i].reserve_y);
        total_output += out;
        (input_scaled, out)
    }).collect();

    RoutingResult { allocations, total_output }
}

// ─── Utilities ────────────────────────────────────────────────────────────────

/// Golden-section search for maximum of a unimodal function on [lo, hi].
/// Returns (arg_max, max_value).
pub fn golden_section_max<F>(f: F, lo: f64, hi: f64, iters: usize) -> (f64, f64)
where
    F: Fn(f64) -> f64,
{
    const PHI: f64 = 1.618033988749895;
    let resphi = 2.0 - PHI;

    let mut a = lo;
    let mut b = hi;
    let mut c = b - resphi * (b - a);
    let mut d = a + resphi * (b - a);
    let mut fc = f(c);
    let mut fd = f(d);

    for _ in 0..iters {
        if fc < fd {
            a = c;
            c = d;
            fc = fd;
            d = a + resphi * (b - a);
            fd = f(d);
        } else {
            b = d;
            d = c;
            fd = fc;
            c = b - resphi * (b - a);
            fc = f(c);
        }
        if (b - a) / (b + a + 1e-14) < 1e-8 { break; }
    }

    let x = 0.5 * (a + b);
    (x, f(x))
}

/// Standard CPAMM output with fee: input_eff = input * (1-fee_bps/10000)
/// output = reserve_out * input_eff / (reserve_in + input_eff)
#[inline]
pub fn cpamm_output(input: u64, reserve_in: u64, reserve_out: u64, fee_bps: u32) -> u64 {
    let input_u128 = input as u128;
    let ri = reserve_in as u128;
    let ro = reserve_out as u128;
    let gamma_num = (10_000 - fee_bps) as u128;
    // input_eff = input * gamma_num / 10_000
    let input_eff = input_u128 * gamma_num / 10_000;
    if ri + input_eff == 0 { return 0; }
    (ro * input_eff / (ri + input_eff)) as u64
}

/// Apply a trade to CPAMM reserves in-place.
/// is_buy=true: Y is input, X is output.
/// Updates reserves according to x*y=k with fee.
pub fn apply_cpamm_trade(
    reserve_x: &mut u64,
    reserve_y: &mut u64,
    is_buy: bool,
    input: u64,
    output: u64,
) {
    if is_buy {
        // Y in, X out
        *reserve_y = reserve_y.saturating_add(input);
        *reserve_x = reserve_x.saturating_sub(output);
    } else {
        // X in, Y out
        *reserve_x = reserve_x.saturating_add(input);
        *reserve_y = reserve_y.saturating_sub(output);
    }
}
