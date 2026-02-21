use std::path::Path;
use libloading::Library;

use crate::types::{
    AfterSwapPayload, EpochBoundaryPayload, STORAGE_SIZE,
    TAG_EPOCH_BOUNDARY,
    TAG_SWAP_BUY, TAG_SWAP_SELL,
};

/// Function signatures exported by compiled strategy shared libraries.
///
/// The CLI compiles each strategy to a native `.so`/`.dylib` with these symbols.
/// We call them directly — no EVM overhead during simulation.
type ComputeSwapFn = unsafe extern "C" fn(data: *const u8, len: usize) -> u64;
type AfterSwapFn   = unsafe extern "C" fn(data: *const u8, len: usize, storage: *mut u8);
type GetNameFn     = unsafe extern "C" fn(buf: *mut u8, max_len: usize) -> usize;

/// A loaded, callable strategy.
pub struct StrategyRunner {
    /// Keep the library alive for the duration of the simulation
    _lib: Library,
    compute_swap: ComputeSwapFn,
    after_swap: AfterSwapFn,
    pub name: String,
}

impl StrategyRunner {
    /// Load a compiled strategy shared library from disk.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let lib = unsafe { Library::new(path)? };

        let compute_swap: ComputeSwapFn = unsafe { *lib.get::<ComputeSwapFn>(b"__prop_amm_compute_swap\0")? };
        let after_swap: AfterSwapFn = unsafe { *lib.get::<AfterSwapFn>(b"__prop_amm_after_swap\0")? };
        let get_name: GetNameFn = unsafe { *lib.get::<GetNameFn>(b"__prop_amm_get_name\0")? };

        // Read strategy name
        let mut name_buf = [0u8; 128];
        let name_len = unsafe { get_name(name_buf.as_mut_ptr(), name_buf.len()) };
        let name = String::from_utf8_lossy(&name_buf[..name_len]).to_string();

        Ok(Self {
            _lib: lib,
            compute_swap,
            after_swap,
            name,
        })
    }

    /// Call compute_swap. Builds the wire payload inline.
    pub fn compute_swap(
        &self,
        is_buy: bool,
        input: u64,
        reserve_x: u64,
        reserve_y: u64,
        storage: &[u8; STORAGE_SIZE],
    ) -> u64 {
        // Wire layout: [tag(1), input(8), rx(8), ry(8), storage(1024)] = 1049 bytes
        let mut buf = [0u8; 1 + 8 + 8 + 8 + STORAGE_SIZE];
        buf[0] = if is_buy { TAG_SWAP_BUY } else { TAG_SWAP_SELL };
        buf[1..9].copy_from_slice(&input.to_le_bytes());
        buf[9..17].copy_from_slice(&reserve_x.to_le_bytes());
        buf[17..25].copy_from_slice(&reserve_y.to_le_bytes());
        buf[25..25 + STORAGE_SIZE].copy_from_slice(storage);

        unsafe { (self.compute_swap)(buf.as_ptr(), buf.len()) }
    }

    /// Call after_swap with the enriched payload. Storage may be mutated.
    pub fn after_swap(
        &self,
        payload: &AfterSwapPayload,
        storage: &mut [u8; STORAGE_SIZE],
    ) {
        // Serialize AfterSwapPayload to bytes.  We use a manual packed layout to match
        // what wincode/pinocchio strategies expect at each byte offset.
        let mut buf = vec![0u8; std::mem::size_of::<AfterSwapPayload>()];
        encode_after_swap_payload(payload, storage, &mut buf);
        unsafe { (self.after_swap)(buf.as_ptr(), buf.len(), storage.as_mut_ptr()) }
    }

    /// Call the epoch boundary hook. Storage may be mutated.
    pub fn epoch_boundary(
        &self,
        payload: &EpochBoundaryPayload,
        storage: &mut [u8; STORAGE_SIZE],
    ) {
        let mut buf = vec![0u8; std::mem::size_of::<EpochBoundaryPayload>()];
        encode_epoch_boundary_payload(payload, storage, &mut buf);
        unsafe { (self.after_swap)(buf.as_ptr(), buf.len(), storage.as_mut_ptr()) }
    }
}

// ─── Payload Serializers ──────────────────────────────────────────────────────
// We hand-encode to guarantee the exact byte offsets documented in types.rs,
// regardless of Rust's struct layout decisions.

fn write_u8(buf: &mut [u8], offset: &mut usize, v: u8) {
    buf[*offset] = v;
    *offset += 1;
}
fn write_u32(buf: &mut [u8], offset: &mut usize, v: u32) {
    buf[*offset..*offset+4].copy_from_slice(&v.to_le_bytes());
    *offset += 4;
}
fn write_u64(buf: &mut [u8], offset: &mut usize, v: u64) {
    buf[*offset..*offset+8].copy_from_slice(&v.to_le_bytes());
    *offset += 8;
}
fn write_f32(buf: &mut [u8], offset: &mut usize, v: f32) {
    buf[*offset..*offset+4].copy_from_slice(&v.to_le_bytes());
    *offset += 4;
}
fn write_f64(buf: &mut [u8], offset: &mut usize, v: f64) {
    buf[*offset..*offset+8].copy_from_slice(&v.to_le_bytes());
    *offset += 8;
}

fn encode_after_swap_payload(p: &AfterSwapPayload, storage: &[u8; STORAGE_SIZE], buf: &mut Vec<u8>) {
    // Ensure capacity: 92 header + 1024 storage = 1116 bytes
    buf.resize(92 + STORAGE_SIZE, 0);
    let mut off = 0;

    write_u8(buf, &mut off, p.tag);                 //  0  tag
    write_u8(buf, &mut off, p.side);                //  1  side
    write_u64(buf, &mut off, p.input_amount);       //  2  input_amount
    write_u64(buf, &mut off, p.output_amount);      // 10  output_amount
    write_u64(buf, &mut off, p.reserve_x);          // 18  reserve_x
    write_u64(buf, &mut off, p.reserve_y);          // 26  reserve_y
    write_u64(buf, &mut off, p.sim_step);           // 34  sim_step
    write_u32(buf, &mut off, p.epoch_step);         // 42  epoch_step
    write_u32(buf, &mut off, p.epoch_number);       // 46  epoch_number
    write_u8(buf, &mut off, p.n_strategies);        // 50  n_strategies
    write_u8(buf, &mut off, p.strategy_index);      // 51  strategy_index
    write_f32(buf, &mut off, p.flow_captured);      // 52  flow_captured
    write_f32(buf, &mut off, p.capital_weight);     // 56  capital_weight
    let competing_spot_prices = p.competing_spot_prices;
    for sp in competing_spot_prices {               // 60..92  competing_spot_prices[8]
        write_f32(buf, &mut off, sp);
    }
    // 92: storage
    buf[92..92 + STORAGE_SIZE].copy_from_slice(storage);
}

fn encode_epoch_boundary_payload(p: &EpochBoundaryPayload, storage: &[u8; STORAGE_SIZE], buf: &mut Vec<u8>) {
    // 41 header bytes + 1024 storage
    buf.resize(41 + STORAGE_SIZE, 0);
    let mut off = 0;

    write_u8(buf, &mut off, TAG_EPOCH_BOUNDARY);    //  0  tag
    write_u32(buf, &mut off, p.epoch_number);       //  1  epoch_number
    write_u64(buf, &mut off, p.new_reserve_x);      //  5  new_reserve_x
    write_u64(buf, &mut off, p.new_reserve_y);      // 13  new_reserve_y
    write_f64(buf, &mut off, p.epoch_edge);         // 21  epoch_edge
    write_f64(buf, &mut off, p.cumulative_edge);    // 29  cumulative_edge
    write_f32(buf, &mut off, p.capital_weight);     // 37  capital_weight
    // 41: storage
    buf[41..41 + STORAGE_SIZE].copy_from_slice(storage);
}

// ─── Normalizer (built-in CPAMM, no external lib) ────────────────────────────

/// The built-in normalizer AMM. Not a dynamic library — runs inline in the engine.
/// Sampled fee and liquidity multiplier, standard CPAMM, no adaptive logic.
pub struct NormalizerRunner {
    pub fee_bps: u32,
}

impl NormalizerRunner {
    pub fn compute_swap(&self, is_buy: bool, input: u64, rx: u64, ry: u64) -> u64 {
        use crate::market::cpamm_output;
        if is_buy { cpamm_output(input, ry, rx, self.fee_bps) }
        else       { cpamm_output(input, rx, ry, self.fee_bps) }
    }
}
