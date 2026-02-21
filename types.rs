/// Scale factor: 1 unit = 1_000_000_000 (1e9)
pub const SCALE: u64 = 1_000_000_000;
pub const SCALE_F: f64 = 1_000_000_000.0;

/// Maximum number of competing strategies (excluding the normalizer)
pub const MAX_STRATEGIES: usize = 16;

/// Per-strategy storage size in bytes (matches prop-amm-challenge)
pub const STORAGE_SIZE: usize = 1024;

// ─── Tag bytes sent to strategy programs ──────────────────────────────────────

/// Compute swap quote (buy X = Y-in)
pub const TAG_SWAP_BUY: u8 = 0;
/// Compute swap quote (sell X = X-in)
pub const TAG_SWAP_SELL: u8 = 1;
/// After-swap hook (real trade executed)
pub const TAG_AFTER_SWAP: u8 = 2;
/// Metadata: return NAME bytes
pub const TAG_GET_NAME: u8 = 3;
/// Metadata: return MODEL_USED bytes
pub const TAG_GET_MODEL: u8 = 4;
/// Epoch boundary: called at the start of every new epoch with capital update
pub const TAG_EPOCH_BOUNDARY: u8 = 5;

// ─── Wire payloads ────────────────────────────────────────────────────────────

/// Payload sent for TAG_SWAP_BUY / TAG_SWAP_SELL  (matches original, extended by storage)
#[repr(C, packed)]
pub struct ComputeSwapPayload {
    pub tag: u8,         // 0 or 1
    pub input_amount: u64,
    pub reserve_x: u64,
    pub reserve_y: u64,
    pub storage: [u8; STORAGE_SIZE],
}

/// Payload sent for TAG_AFTER_SWAP — enriched vs. original to expose competitive context.
///
/// Layout (byte offsets):
///   0   tag             u8
///   1   side            u8   (0=buy X, 1=sell X)
///   2   input_amount    u64
///  10   output_amount   u64
///  18   reserve_x       u64  (post-trade)
///  26   reserve_y       u64
///  34   sim_step        u64  (global step within simulation)
///  42   epoch_step      u32  (step within current epoch, 0-based)
///  46   epoch_number    u32  (epoch index, 0-based)
///  50   n_strategies    u8   (total number of competing strategies incl. normalizer)
///  51   strategy_index  u8   (this strategy's index)
///  52   flow_captured   f32  (fraction of this retail order routed here, 0.0-1.0)
///  56   capital_weight  f32  (this strategy's fraction of total protocol capital)
///  60   [f32; 8]        competing_spot_prices (spot price of each other AMM, NaN if unused)
///  92   storage         [u8; STORAGE_SIZE]
#[repr(C, packed)]
pub struct AfterSwapPayload {
    pub tag: u8,
    pub side: u8,
    pub input_amount: u64,
    pub output_amount: u64,
    pub reserve_x: u64,
    pub reserve_y: u64,
    pub sim_step: u64,
    pub epoch_step: u32,
    pub epoch_number: u32,
    pub n_strategies: u8,
    pub strategy_index: u8,
    pub flow_captured: f32,
    pub capital_weight: f32,
    pub competing_spot_prices: [f32; 8],
    pub storage: [u8; STORAGE_SIZE],
}

/// Payload sent for TAG_EPOCH_BOUNDARY — notifies strategy of new capital allocation.
///
/// Layout:
///   0   tag                u8
///   1   epoch_number       u32
///   5   new_reserve_x      u64
///  13   new_reserve_y      u64
///  21   epoch_edge         f64   (edge earned in just-completed epoch)
///  29   cumulative_edge    f64   (total edge across all epochs so far)
///  37   capital_weight     f32   (new fraction of total protocol capital)
///  41   storage            [u8; STORAGE_SIZE]  (read-write, persists)
#[repr(C, packed)]
pub struct EpochBoundaryPayload {
    pub tag: u8,
    pub epoch_number: u32,
    pub new_reserve_x: u64,
    pub new_reserve_y: u64,
    pub epoch_edge: f64,
    pub cumulative_edge: f64,
    pub capital_weight: f32,
    pub storage: [u8; STORAGE_SIZE],
}

// ─── Engine-side state ────────────────────────────────────────────────────────

/// Live state of a single AMM instance in the engine.
#[derive(Clone, Debug)]
pub struct AmmState {
    pub reserve_x: u64,
    pub reserve_y: u64,
    pub storage: [u8; STORAGE_SIZE],

    // Accounting
    pub cumulative_edge: f64,
    pub epoch_edge: f64,
    pub epoch_trade_count: u64,

    // Capital tracking
    pub capital_weight: f64,   // fraction of total capital allocated here

    // Identity
    pub strategy_index: u8,
    pub name: String,
}

impl AmmState {
    pub fn new(reserve_x: u64, reserve_y: u64, idx: u8, name: &str) -> Self {
        Self {
            reserve_x,
            reserve_y,
            storage: [0u8; STORAGE_SIZE],
            cumulative_edge: 0.0,
            epoch_edge: 0.0,
            epoch_trade_count: 0,
            capital_weight: 1.0, // will be normalized across N strategies after init
            strategy_index: idx,
            name: name.to_string(),
        }
    }

    /// Spot price: Y per X
    #[inline]
    pub fn spot_price(&self) -> f64 {
        self.reserve_y as f64 / self.reserve_x as f64
    }

    /// Accrue edge from a trade, given the fair price at execution time.
    /// For AMM sells X (receives X, pays Y): edge = amountX * fair - amountY
    /// For AMM buys X  (receives Y, pays X): edge = amountY - amountX * fair
    #[inline]
    pub fn accrue_edge(&mut self, amount_x: u64, amount_y: u64, is_buy: bool, fair_price: f64) {
        let ax = amount_x as f64 / SCALE_F;
        let ay = amount_y as f64 / SCALE_F;
        let edge = if is_buy {
            // AMM buys X: receives Y_in, pays X_out → edge = Y_in - X_out * fair
            ay - ax * fair_price
        } else {
            // AMM sells X: receives X_in, pays Y_out → edge = X_in * fair - Y_out
            ax * fair_price - ay
        };
        self.cumulative_edge += edge;
        self.epoch_edge += edge;
        self.epoch_trade_count += 1;
    }
}

/// Per-epoch summary used for capital allocation decisions.
#[derive(Clone, Debug, Default)]
pub struct EpochSummary {
    pub epoch_number: u32,
    pub edge: f64,
    pub trade_count: u64,
    pub arb_losses: f64,
    pub retail_gains: f64,
    /// Risk-adjusted score = edge - lambda * max(0, -edge)
    pub risk_adjusted_score: f64,
}

/// Configuration for a multi-epoch simulation run.
#[derive(Clone, Debug)]
pub struct SimConfig {
    /// Total simulation steps
    pub total_steps: usize,
    /// Steps per epoch (capital rebalanced at epoch boundaries)
    pub epoch_len: usize,
    /// Random seed
    pub seed: u64,
    /// Initial X reserves per AMM (before capital weight scaling)
    pub base_reserve_x: u64,
    /// Initial Y reserves per AMM
    pub base_reserve_y: u64,
    /// Risk-aversion coefficient for capital allocation (CVaR penalty weight)
    pub lambda: f64,
    /// Minimum capital weight any strategy can hold (prevents starvation)
    pub min_capital_weight: f64,
    /// Temperature for softmax capital allocation (higher = more uniform)
    pub softmax_temperature: f64,
    /// Minimum arb profit floor (in Y, unscaled) to trigger an arb trade
    pub arb_profit_floor: f64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            total_steps: 10_000,
            epoch_len: 1_000,
            seed: 0,
            base_reserve_x: 100 * SCALE,  // 100 X
            base_reserve_y: 10_000 * SCALE, // 10,000 Y  → spot = 100
            lambda: 2.0,
            min_capital_weight: 0.02,  // 2% minimum allocation
            softmax_temperature: 1.0,
            arb_profit_floor: 0.01,
        }
    }
}
