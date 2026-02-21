const NAME: &str = "submission_4_fixed_70bps";
const FEE_BPS: u128 = 70;

#[no_mangle]
pub extern "C" fn __prop_amm_compute_swap(data: *const u8, len: usize) -> u64 {
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    if bytes.len() < 25 { return 0; }

    let input = u64::from_le_bytes(bytes[1..9].try_into().unwrap_or([0; 8]));
    let rx = u64::from_le_bytes(bytes[9..17].try_into().unwrap_or([0; 8]));
    let ry = u64::from_le_bytes(bytes[17..25].try_into().unwrap_or([0; 8]));
    let is_buy = bytes[0] == 0;

    if is_buy { cpamm_output(input, ry, rx, FEE_BPS) } else { cpamm_output(input, rx, ry, FEE_BPS) }
}

#[no_mangle]
pub extern "C" fn __prop_amm_after_swap(_data: *const u8, _len: usize, _storage_ptr: *mut u8) {}

#[no_mangle]
pub extern "C" fn __prop_amm_get_name(buf: *mut u8, max_len: usize) -> usize {
    let bytes = NAME.as_bytes();
    let n = bytes.len().min(max_len);
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n) };
    n
}

fn cpamm_output(input: u64, reserve_in: u64, reserve_out: u64, fee_bps: u128) -> u64 {
    if input == 0 || reserve_in == 0 || reserve_out == 0 { return 0; }
    let fee_den = 10_000u128;
    let input_eff = (input as u128) * (fee_den - fee_bps) / fee_den;
    let denom = reserve_in as u128 + input_eff;
    if denom == 0 { return 0; }
    ((reserve_out as u128) * input_eff / denom) as u64
}
