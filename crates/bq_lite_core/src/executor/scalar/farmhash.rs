/// Exact `FarmHash` `Fingerprint64` implementation matching Google `BigQuery` `FARM_FINGERPRINT`.
const K0: u64 = 0xc3a5_c85c_97cb_3127;
const K1: u64 = 0xb492_b66f_be98_f273;
const K2: u64 = 0x9ae1_6a3b_2f90_404f;

#[expect(
    clippy::cast_possible_wrap,
    reason = "BigQuery FARM_FINGERPRINT returns signed INT64 bit-cast of u64 Fingerprint64"
)]
#[must_use]
pub(super) fn farm_fingerprint64(bytes: &[u8]) -> i64 {
    fingerprint64_u64(bytes) as i64
}

fn fingerprint64_u64(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    if len <= 16 {
        return hash_len_0_to_16(bytes);
    }
    if len <= 32 {
        return hash_len_17_to_32(bytes);
    }
    if len <= 64 {
        return hash_len_33_to_64(bytes);
    }
    hash_len_gt_64(bytes)
}

fn fetch64(bytes: &[u8], off: usize) -> u64 {
    let slice = bytes.get(off..off + 8).unwrap_or(&[0; 8]);
    let arr: [u8; 8] = slice.try_into().unwrap_or([0; 8]);
    u64::from_le_bytes(arr)
}

fn fetch32(bytes: &[u8], off: usize) -> u32 {
    let slice = bytes.get(off..off + 4).unwrap_or(&[0; 4]);
    let arr: [u8; 4] = slice.try_into().unwrap_or([0; 4]);
    u32::from_le_bytes(arr)
}

const fn shift_mix(val: u64) -> u64 {
    val ^ (val >> 47)
}

const fn hash_len_16(lo: u64, hi: u64, mul: u64) -> u64 {
    let mut aa = (lo ^ hi).wrapping_mul(mul);
    aa ^= aa >> 47;
    let mut bb = (hi ^ aa).wrapping_mul(mul);
    bb ^= bb >> 47;
    bb = bb.wrapping_mul(mul);
    bb
}

fn hash_len_0_to_16(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    if len >= 8 {
        let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
        let lo = fetch64(bytes, 0).wrapping_add(K2);
        let hi = fetch64(bytes, len - 8);
        let cc = hi.rotate_right(37).wrapping_mul(mul).wrapping_add(lo);
        let dd = lo.rotate_right(25).wrapping_add(hi).wrapping_mul(mul);
        return hash_len_16(cc, dd, mul);
    }
    if len >= 4 {
        let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
        let lo = u64::from(fetch32(bytes, 0));
        return hash_len_16(
            (len as u64).wrapping_add(lo << 3),
            u64::from(fetch32(bytes, len - 4)),
            mul,
        );
    }
    if len > 0 {
        let b0 = u32::from(bytes.first().copied().unwrap_or(0));
        let b1 = u32::from(bytes.get(len >> 1).copied().unwrap_or(0));
        let b2 = u32::from(bytes.get(len - 1).copied().unwrap_or(0));
        let lo = b0.wrapping_add(b1 << 8);
        let hi = (u32::try_from(len).unwrap_or(0)).wrapping_add(b2 << 2);
        return shift_mix(u64::from(lo).wrapping_mul(K2) ^ u64::from(hi).wrapping_mul(K0))
            .wrapping_mul(K2);
    }
    K2
}

fn hash_len_17_to_32(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
    let w0 = fetch64(bytes, 0).wrapping_mul(K1);
    let w1 = fetch64(bytes, 8);
    let w2 = fetch64(bytes, len - 8).wrapping_mul(mul);
    let w3 = fetch64(bytes, len - 16).wrapping_mul(K2);
    hash_len_16(
        w0.wrapping_add(w1)
            .rotate_right(43)
            .wrapping_add(w2.rotate_right(30))
            .wrapping_add(w3),
        w0.wrapping_add(w1.wrapping_add(K2).rotate_right(18))
            .wrapping_add(w2),
        mul,
    )
}

fn hash_len_33_to_64(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
    let w0 = fetch64(bytes, 0).wrapping_mul(K2);
    let w1 = fetch64(bytes, 8);
    let w2 = fetch64(bytes, len - 8).wrapping_mul(mul);
    let w3 = fetch64(bytes, len - 16).wrapping_mul(K2);
    let yy = w0
        .wrapping_add(w1)
        .rotate_right(43)
        .wrapping_add(w2.rotate_right(30))
        .wrapping_add(w3);
    let zz = hash_len_16(
        yy,
        w0.wrapping_add(w1.wrapping_add(K2).rotate_right(18))
            .wrapping_add(w2),
        mul,
    );
    let w4 = fetch64(bytes, 16).wrapping_mul(mul);
    let w5 = fetch64(bytes, 24);
    let w6 = yy.wrapping_add(fetch64(bytes, len - 32)).wrapping_mul(mul);
    let w7 = zz.wrapping_add(fetch64(bytes, len - 24)).wrapping_mul(mul);
    let aa = w4
        .wrapping_add(w5)
        .rotate_right(43)
        .wrapping_add(w6.rotate_right(30))
        .wrapping_add(w7);
    hash_len_16(
        aa,
        w4.wrapping_add(w5.wrapping_add(K2).rotate_right(18))
            .wrapping_add(w6),
        mul,
    )
}

const fn weak_hash_len_32_with_seeds(
    w0: u64,
    w1: u64,
    w2: u64,
    w3: u64,
    mut acc_a: u64,
    mut acc_b: u64,
) -> (u64, u64) {
    acc_a = acc_a.wrapping_add(w0);
    acc_b = acc_b.wrapping_add(acc_a).wrapping_add(w3).rotate_right(21);
    let saved = acc_a;
    acc_a = acc_a.wrapping_add(w1);
    acc_a = acc_a.wrapping_add(w2);
    acc_b = acc_b.wrapping_add(acc_a.rotate_right(44));
    (acc_a.wrapping_add(w3), acc_b.wrapping_add(saved))
}

fn weak_hash_32_at(bytes: &[u8], off: usize, acc_a: u64, acc_b: u64) -> (u64, u64) {
    weak_hash_len_32_with_seeds(
        fetch64(bytes, off),
        fetch64(bytes, off + 8),
        fetch64(bytes, off + 16),
        fetch64(bytes, off + 24),
        acc_a,
        acc_b,
    )
}

fn hash_len_gt_64(bytes: &[u8]) -> u64 {
    let seed = 81_u64;
    let mut acc_x = seed;
    let mut acc_y = seed.wrapping_mul(K1).wrapping_add(113);
    let mut acc_z = shift_mix(acc_y.wrapping_mul(K2).wrapping_add(113)).wrapping_mul(K2);
    let mut pair_v = (0_u64, 0_u64);
    let mut pair_w = (0_u64, 0_u64);
    acc_x = acc_x.wrapping_mul(K2).wrapping_add(fetch64(bytes, 0));

    let mut off = 0_usize;
    let end = ((bytes.len() - 1) / 64) * 64;
    let last64 = end + ((bytes.len() - 1) & 63) - 63;

    while off < end {
        acc_x = acc_x
            .wrapping_add(acc_y)
            .wrapping_add(pair_v.0)
            .wrapping_add(fetch64(bytes, off + 8))
            .rotate_right(37)
            .wrapping_mul(K1);
        acc_y = acc_y
            .wrapping_add(pair_v.1)
            .wrapping_add(fetch64(bytes, off + 48))
            .rotate_right(42)
            .wrapping_mul(K1);
        acc_x ^= pair_w.1;
        acc_y = acc_y.wrapping_add(pair_v.0.wrapping_add(fetch64(bytes, off + 40)));
        acc_z = acc_z
            .wrapping_add(pair_w.0)
            .rotate_right(33)
            .wrapping_mul(K1);
        pair_v = weak_hash_32_at(
            bytes,
            off,
            pair_v.1.wrapping_mul(K1),
            acc_x.wrapping_add(pair_w.0),
        );
        pair_w = weak_hash_32_at(
            bytes,
            off + 32,
            acc_z.wrapping_add(pair_w.1),
            acc_y.wrapping_add(fetch64(bytes, off + 16)),
        );
        std::mem::swap(&mut acc_z, &mut acc_x);
        off += 64;
    }

    finalize_gt_64(bytes, last64, (acc_x, acc_y, acc_z), pair_v, pair_w)
}

fn finalize_gt_64(
    bytes: &[u8],
    last64: usize,
    accs: (u64, u64, u64),
    mut pair_v: (u64, u64),
    mut pair_w: (u64, u64),
) -> u64 {
    let (mut acc_x, mut acc_y, mut acc_z) = accs;
    let mul = K1.wrapping_add((acc_z & 0xff) << 1);
    pair_w.0 = pair_w.0.wrapping_add(((bytes.len() - 1) & 63) as u64);
    pair_v.0 = pair_v.0.wrapping_add(pair_w.0);
    pair_w.0 = pair_w.0.wrapping_add(pair_v.0);
    acc_x = acc_x
        .wrapping_add(acc_y)
        .wrapping_add(pair_v.0)
        .wrapping_add(fetch64(bytes, last64 + 8))
        .rotate_right(37)
        .wrapping_mul(mul);
    acc_y = acc_y
        .wrapping_add(pair_v.1)
        .wrapping_add(fetch64(bytes, last64 + 48))
        .rotate_right(42)
        .wrapping_mul(mul);
    acc_x ^= pair_w.1.wrapping_mul(9);
    acc_y = acc_y.wrapping_add(
        pair_v
            .0
            .wrapping_mul(9)
            .wrapping_add(fetch64(bytes, last64 + 40)),
    );
    acc_z = acc_z
        .wrapping_add(pair_w.0)
        .rotate_right(33)
        .wrapping_mul(mul);
    pair_v = weak_hash_32_at(
        bytes,
        last64,
        pair_v.1.wrapping_mul(mul),
        acc_x.wrapping_add(pair_w.0),
    );
    pair_w = weak_hash_32_at(
        bytes,
        last64 + 32,
        acc_z.wrapping_add(pair_w.1),
        acc_y.wrapping_add(fetch64(bytes, last64 + 16)),
    );
    std::mem::swap(&mut acc_z, &mut acc_x);
    hash_len_16(
        hash_len_16(pair_v.0, pair_w.0, mul)
            .wrapping_add(shift_mix(acc_y).wrapping_mul(K0))
            .wrapping_add(acc_z),
        hash_len_16(pair_v.1, pair_w.1, mul).wrapping_add(acc_x),
        mul,
    )
}
