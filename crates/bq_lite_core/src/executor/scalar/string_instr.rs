use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub(super) fn eval_instr(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 4 {
        return Err(BqError::query("INSTR expects 2 to 4 arguments"));
    }
    let mut it = args.into_iter();
    let hay = it.next().unwrap_or(BqValue::Null);
    let needle = it.next().unwrap_or(BqValue::Null);
    let pos_v = it.next();
    let occ_v = it.next();
    if hay.is_null()
        || needle.is_null()
        || pos_v.as_ref().is_some_and(BqValue::is_null)
        || occ_v.as_ref().is_some_and(BqValue::is_null)
    {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(h), BqValue::String(n)) = (hay, needle) else {
        return Err(BqError::query("INSTR expects STRING"));
    };
    let pos = match pos_v {
        None => 1_i64,
        Some(BqValue::Int64(p)) => p,
        Some(_) => return Err(BqError::query("INSTR position must be INT64")),
    };
    let occ = match occ_v {
        None => 1_i64,
        Some(BqValue::Int64(o)) => o,
        Some(_) => return Err(BqError::query("INSTR occurrence must be INT64")),
    };
    if pos == 0 {
        return Err(BqError::semantic("INSTR position must be non-zero"));
    }
    if occ < 1 {
        return Err(BqError::semantic("INSTR occurrence must be >= 1"));
    }
    Ok(BqValue::Int64(find_instr_pos(&h, &n, pos, occ)))
}

fn find_instr_pos(hay: &str, needle: &str, pos: i64, occ: i64) -> i64 {
    let h_chars: Vec<char> = hay.chars().collect();
    let n_chars: Vec<char> = needle.chars().collect();
    let h_len = h_chars.len();
    let n_len = n_chars.len();
    if n_len > h_len {
        return 0;
    }
    let max_idx = h_len - n_len;
    let mut seen = 0_i64;
    if pos > 0 {
        let Ok(start) = usize::try_from(pos - 1) else {
            return 0;
        };
        if start > max_idx {
            return 0;
        }
        for idx in start..=max_idx {
            if h_chars.get(idx..idx + n_len) == Some(n_chars.as_slice()) {
                seen += 1;
                if seen == occ {
                    return i64::try_from(idx + 1).unwrap_or(i64::MAX);
                }
            }
        }
        return 0;
    }
    let abs_pos = usize::try_from(pos.unsigned_abs()).unwrap_or(usize::MAX);
    if abs_pos > h_len {
        return 0;
    }
    let start_rev = (h_len - abs_pos).min(max_idx);
    for idx in (0..=start_rev).rev() {
        if h_chars.get(idx..idx + n_len) == Some(n_chars.as_slice()) {
            seen += 1;
            if seen == occ {
                return i64::try_from(idx + 1).unwrap_or(i64::MAX);
            }
        }
    }
    0
}
