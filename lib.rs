//! `prop_amm_submission_sdk` — Strategy author interface.
//!
//! This crate provides everything a strategy needs:
//!  - Typed decoders for `ComputeSwap`, `AfterSwap`, and `EpochBoundary` payloads
//!  - `set_return_data_u64` / `set_storage` helpers
//!  - Fixed-point math utilities (wmul, wdiv, sqrt, bps_to_wad)
//!
//! Strategies only need to implement:
//!   `fn compute_swap(ctx: &SwapContext) -> u64`
//!   `fn after_swap(ctx: &AfterSwapContext, storage: &mut Storage)`   [optional]
//!   `fn on_epoch_boundary(ctx: &EpochContext, storage: &mut Storage)` [optional]

#![no_std]

// ─── Scale constants ──────────────────────────────────────────────────────────

/// Token amounts use 1e9 scale (1 unit = 1_000_000_000)
pub const SCALE: u64 = 1_000_000_000;

/// WAD = 1e18, used for fee arithmetic
pub const WAD: u64 = 1_000_000_000_000_000_000;

pub const MAX_FEE_WAD: u64 = WAD / 10;  // 10% max fee
pub const MIN_FEE_WAD: u64 = 0;

// ─── Storage ──────────────────────────────────────────────────────────────────

pub const STORAGE_SIZE: usize = 1024;

/// Strategy persistent storage: 1024 bytes, zero-initialized, persists across
/// all trades within a simulation AND across epoch boundaries.
pub type Storage = [u8; STORAGE_SIZE];

// ─── Swap context ─────────────────────────────────────────────────────────────

/// Context passed to `compute_swap`.
/// Decoded from the wire payload at byte offsets [0..1049].
pub struct SwapContext {
    /// true = buy X (Y is input), false = sell X (X is input)
    pub is_buy: bool,
    /// Input amount (1e9 scale)
    pub input_amount: u64,
    /// Current X reserve (1e9 scale)
    pub reserve_x: u64,
    /// Current Y reserve (1e9 scale)
    pub reserve_y: u64,
    /// Read-only view of strategy storage
    pub storage: Storage,
}

impl SwapContext {
    /// Parse from raw instruction bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 25 + STORAGE_SIZE { return None; }
        Some(Self {
            is_buy: data[0] == 0,
            input_amount: u64::from_le_bytes(data[1..9].try_into().ok()?),
            reserve_x:    u64::from_le_bytes(data[9..17].try_into().ok()?),
            reserve_y:    u64::from_le_bytes(data[17..25].try_into().ok()?),
            storage: data[25..25 + STORAGE_SIZE].try_into().ok()?,
        })
    }

    /// Spot price (Y per X), as f64.
    #[inline]
    pub fn spot_price(&self) -> f64 {
        self.reserve_y as f64 / self.reserve_x as f64
    }
}

// ─── AfterSwap context ────────────────────────────────────────────────────────

/// Enriched context passed to `after_swap` after every real trade.
///
/// Byte offsets mirror the `AfterSwapPayload` layout in the engine's `types.rs`.
pub struct AfterSwapContext {
    pub is_buy:        bool,
    pub input_amount:  u64,
    pub output_amount: u64,
    pub reserve_x:     u64,    // post-trade
    pub reserve_y:     u64,
    pub sim_step:      u64,

    /// Step within the current epoch (0-based, resets each epoch)
    pub epoch_step:    u32,
    /// Current epoch index (0-based)
    pub epoch_number:  u32,
    /// Total number of competing AMMs (strategies + normalizer)
    pub n_strategies:  u8,
    /// This strategy's index in the routing pool
    pub strategy_index: u8,

    /// Fraction of this retail order routed to this AMM (0.0 = arb trade, 0.0-1.0 = retail split)
    pub flow_captured: f32,
    /// This strategy's current fraction of total protocol capital
    pub capital_weight: f32,

    /// Spot prices of the other AMMs (NaN for unused slots).
    /// Slots 0..n_strategies-2 are other strategies; last slot is the normalizer.
    pub competing_spot_prices: [f32; 8],
}

impl AfterSwapContext {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 92 { return None; }
        Some(Self {
            is_buy:         data[1] == 0,
            input_amount:   u64::from_le_bytes(data[2..10].try_into().ok()?),
            output_amount:  u64::from_le_bytes(data[10..18].try_into().ok()?),
            reserve_x:      u64::from_le_bytes(data[18..26].try_into().ok()?),
            reserve_y:      u64::from_le_bytes(data[26..34].try_into().ok()?),
            sim_step:       u64::from_le_bytes(data[34..42].try_into().ok()?),
            epoch_step:     u32::from_le_bytes(data[42..46].try_into().ok()?),
            epoch_number:   u32::from_le_bytes(data[46..50].try_into().ok()?),
            n_strategies:   data[50],
            strategy_index: data[51],
            flow_captured:  f32::from_le_bytes(data[52..56].try_into().ok()?),
            capital_weight: f32::from_le_bytes(data[56..60].try_into().ok()?),
            competing_spot_prices: {
                let mut arr = [f32::NAN; 8];
                for i in 0..8 {
                    let off = 60 + i * 4;
                    arr[i] = f32::from_le_bytes(data[off..off+4].try_into().ok()?);
                }
                arr
            },
        })
    }

    /// Spot price from post-trade reserves.
    #[inline]
    pub fn spot_price(&self) -> f64 {
        self.reserve_y as f64 / self.reserve_x as f64
    }

    /// Implied effective fee from this trade (approximate).
    /// Uses: effective_fee ≈ 1 - (output * reserve_in) / (input * reserve_out)
    pub fn implied_effective_fee(&self) -> f64 {
        let (in_f, out_f, ri, ro) = if self.is_buy {
            // Y in, X out.  price = Y/X → output * ri(Y) / input(Y) / ro(X) ... 
            // Simpler: eff_price = output / input; fair = spot
            (self.input_amount as f64, self.output_amount as f64,
             self.reserve_y as f64, self.reserve_x as f64)
        } else {
            (self.input_amount as f64, self.output_amount as f64,
             self.reserve_x as f64, self.reserve_y as f64)
        };
        if in_f == 0.0 || ro == 0.0 { return 0.0; }
        let eff = out_f * ri / (in_f * ro);
        (1.0 - eff).max(0.0)
    }
}

// ─── Epoch boundary context ───────────────────────────────────────────────────

/// Context passed to `on_epoch_boundary`. Called once per epoch transition.
/// This is the strategic adaptation point: strategies learn their new capital
/// allocation and can reinitialize internal state accordingly.
pub struct EpochContext {
    pub epoch_number:     u32,
    pub new_reserve_x:    u64,
    pub new_reserve_y:    u64,
    /// Edge earned during the epoch that just ended
    pub epoch_edge:       f64,
    /// Total cumulative edge across all epochs so far
    pub cumulative_edge:  f64,
    /// New capital allocation fraction (0.0-1.0)
    pub capital_weight:   f32,
}

impl EpochContext {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 41 { return None; }
        Some(Self {
            epoch_number:    u32::from_le_bytes(data[1..5].try_into().ok()?),
            new_reserve_x:   u64::from_le_bytes(data[5..13].try_into().ok()?),
            new_reserve_y:   u64::from_le_bytes(data[13..21].try_into().ok()?),
            epoch_edge:      f64::from_le_bytes(data[21..29].try_into().ok()?),
            cumulative_edge: f64::from_le_bytes(data[29..37].try_into().ok()?),
            capital_weight:  f32::from_le_bytes(data[37..41].try_into().ok()?),
        })
    }
}

// ─── Storage typed accessors ──────────────────────────────────────────────────

/// Read a u64 from storage at byte offset `slot * 8`.
/// Strategies have 128 u64 slots (8 bytes each × 128 = 1024 bytes).
#[inline]
pub fn read_u64(storage: &Storage, slot: usize) -> u64 {
    let off = slot * 8;
    u64::from_le_bytes(storage[off..off + 8].try_into().unwrap_or([0u8; 8]))
}

/// Write a u64 into storage at slot.
#[inline]
pub fn write_u64(storage: &mut Storage, slot: usize, val: u64) {
    let off = slot * 8;
    storage[off..off + 8].copy_from_slice(&val.to_le_bytes());
}

/// Read an f64 from storage at slot (f64 occupies 8 bytes = 1 slot).
#[inline]
pub fn read_f64(storage: &Storage, slot: usize) -> f64 {
    f64::from_bits(read_u64(storage, slot))
}

/// Write an f64 into storage at slot.
#[inline]
pub fn write_f64(storage: &mut Storage, slot: usize, val: f64) {
    write_u64(storage, slot, val.to_bits());
}

// ─── Fixed-point math (WAD = 1e18) ───────────────────────────────────────────

/// WAD-precision multiply: (a * b) / WAD
#[inline]
pub fn wmul(a: u64, b: u64) -> u64 {
    ((a as u128 * b as u128) / WAD as u128) as u64
}

/// WAD-precision divide: (a * WAD) / b
#[inline]
pub fn wdiv(a: u64, b: u64) -> u64 {
    if b == 0 { return 0; }
    ((a as u128 * WAD as u128) / b as u128) as u64
}

/// Integer square root (Newton's method).
#[inline]
pub fn sqrt(x: u64) -> u64 {
    if x == 0 { return 0; }
    let mut z = x;
    let mut y = (x + 1) / 2;
    while y < z {
        z = y;
        y = (y + x / y) / 2;
    }
    z
}

/// Convert basis points to WAD. E.g. 30 bps → 30 * 1e14.
#[inline]
pub fn bps_to_wad(bps: u64) -> u64 {
    bps * (WAD / 10_000)
}

/// Clamp a fee to [0, MAX_FEE_WAD].
#[inline]
pub fn clamp_fee(fee: u64) -> u64 {
    fee.min(MAX_FEE_WAD)
}

/// Standard CPAMM output given WAD fee.
/// output = reserve_out * input * (1 - fee_wad/WAD) / (reserve_in + input * (1 - fee_wad/WAD))
pub fn cpamm_output_wad(input: u64, reserve_in: u64, reserve_out: u64, fee_wad: u64) -> u64 {
    let input_u128   = input as u128;
    let ri           = reserve_in as u128;
    let ro           = reserve_out as u128;
    let gamma        = (WAD - fee_wad) as u128; // 1 - fee
    // input_eff = input * gamma / WAD
    let input_eff    = input_u128 * gamma / WAD as u128;
    if ri + input_eff == 0 { return 0; }
    (ro * input_eff / (ri + input_eff)) as u64
}

// ─── Return data helpers (native FFI stubs — real ones use Solana syscalls) ───

/// In native mode: write the u64 result to a thread-local so the engine can read it.
/// In BPF mode (pinocchio): would use sol_set_return_data syscall.
#[cfg(not(target_os = "solana"))]
pub fn set_return_data_u64(val: u64) {
    RETURN_DATA_U64.with(|cell| *cell.borrow_mut() = val);
}

#[cfg(not(target_os = "solana"))]
pub fn set_storage(storage: &Storage) {
    PENDING_STORAGE.with(|cell| *cell.borrow_mut() = *storage);
}

#[cfg(not(target_os = "solana"))]
use core::cell::RefCell;

#[cfg(not(target_os = "solana"))]
std::thread_local! {
    pub static RETURN_DATA_U64: RefCell<u64> = RefCell::new(0);
    pub static PENDING_STORAGE: RefCell<Storage> = RefCell::new([0u8; STORAGE_SIZE]);
}
