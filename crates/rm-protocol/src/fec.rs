//! Forward error correction for UDP video: systematic Reed-Solomon over GF(2^8) with a Cauchy
//! parity matrix (the kind of FEC Moonlight/Sunshine use). `k` data shards get `m` parity
//! shards; any `k` of the `k + m` shards rebuild the data, so up to `m` lost packets per block
//! cost nothing, with no retransmission round trip.
//!
//! Field: polynomial x^8 + x^4 + x^3 + x^2 + 1 (0x11D), generator 2. Parity row `i`, data column
//! `j`: 1 / ((k + i) XOR j). The Swift agent (agent/macos/Fec.swift) computes exactly the same
//! bytes; both check the same test vector.

use std::sync::OnceLock;

struct Tables {
    exp: [u8; 512],
    log: [u8; 256],
    /// mul[a][b]
    mul: Vec<[u8; 256]>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let (mut exp, mut log) = ([0u8; 512], [0u8; 256]);
        let mut x: u16 = 1;
        for (i, e) in exp.iter_mut().take(255).enumerate() {
            *e = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11D;
            }
        }
        for i in 255..512 {
            exp[i] = exp[i - 255];
        }
        let mut mul = vec![[0u8; 256]; 256];
        for a in 1..256 {
            for b in 1..256 {
                mul[a][b] = exp[log[a] as usize + log[b] as usize];
            }
        }
        Tables { exp, log, mul }
    })
}

fn mul(a: u8, b: u8) -> u8 {
    tables().mul[a as usize][b as usize]
}

fn inv(a: u8) -> u8 {
    let t = tables();
    t.exp[255 - t.log[a as usize] as usize]
}

/// Coefficient of data shard `j` in parity shard `i` of a block with `k` data shards.
pub fn coef(k: usize, i: usize, j: usize) -> u8 {
    inv(((k + i) as u8) ^ (j as u8))
}

/// Largest block: `k + m` must stay within the field.
pub const MAX_SHARDS: usize = 255;

/// Parity shards for equally long data shards.
pub fn encode(data: &[&[u8]], m: usize) -> Vec<Vec<u8>> {
    let k = data.len();
    assert!(k > 0 && k + m <= MAX_SHARDS);
    let len = data[0].len();
    let t = tables();
    (0..m)
        .map(|i| {
            let mut p = vec![0u8; len];
            for (j, d) in data.iter().enumerate() {
                let row = &t.mul[coef(k, i, j) as usize];
                for (pb, db) in p.iter_mut().zip(d.iter()) {
                    *pb ^= row[*db as usize];
                }
            }
            p
        })
        .collect()
}

/// Fill in missing data shards (`shards[..k]`) from any `k` present shards of `k + m`.
/// False when fewer than `k` shards arrived.
pub fn reconstruct(k: usize, m: usize, shards: &mut [Option<Vec<u8>>]) -> bool {
    assert_eq!(shards.len(), k + m);
    let missing: Vec<usize> = (0..k).filter(|&j| shards[j].is_none()).collect();
    if missing.is_empty() {
        return true;
    }
    let rows: Vec<usize> = (0..k + m).filter(|&s| shards[s].is_some()).take(k).collect();
    if rows.len() < k {
        return false;
    }
    let len = shards[rows[0]].as_ref().unwrap().len();
    // A: row r expresses received shard rows[r] in terms of the data shards
    let mut a: Vec<Vec<u8>> = rows.iter().map(|&s| (0..k).map(|j| if s < k { (s == j) as u8 } else { coef(k, s - k, j) }).collect()).collect();
    // invert A (Gauss-Jordan); every square submatrix of [I; Cauchy] is invertible
    let mut b: Vec<Vec<u8>> = (0..k).map(|r| (0..k).map(|c| (r == c) as u8).collect()).collect();
    for col in 0..k {
        let Some(p) = (col..k).find(|&r| a[r][col] != 0) else { return false };
        a.swap(col, p);
        b.swap(col, p);
        let f = inv(a[col][col]);
        for c in 0..k {
            a[col][c] = mul(a[col][c], f);
            b[col][c] = mul(b[col][c], f);
        }
        for r in 0..k {
            if r != col && a[r][col] != 0 {
                let g = a[r][col];
                for c in 0..k {
                    a[r][c] ^= mul(g, a[col][c]);
                    b[r][c] ^= mul(g, b[col][c]);
                }
            }
        }
    }
    let t = tables();
    for &j in &missing {
        let mut out = vec![0u8; len];
        for (r, &s) in rows.iter().enumerate() {
            let c = b[j][r];
            if c == 0 {
                continue;
            }
            let row = &t.mul[c as usize];
            for (o, x) in out.iter_mut().zip(shards[s].as_ref().unwrap().iter()) {
                *o ^= row[*x as usize];
            }
        }
        shards[j] = Some(out);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vector agent/macos/Fec.swift checks at start-up: same input, same parity bytes.
    pub fn vector() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let data: Vec<Vec<u8>> = (0..3).map(|j| (0..4).map(|b| ((j * 7 + b * 13 + 1) & 0xFF) as u8).collect()).collect();
        let refs: Vec<&[u8]> = data.iter().map(|d| d.as_slice()).collect();
        let parity = encode(&refs, 2);
        (data, parity)
    }

    #[test]
    fn known_vector() {
        let (_, parity) = vector();
        assert_eq!(parity, vec![vec![0xff, 0x69, 0x31, 0xb7], vec![0x9a, 0xdf, 0xb3, 0x59]], "{parity:02x?}");
    }

    #[test]
    fn rebuilds_from_any_k_shards() {
        let k = 10;
        let m = 4;
        let data: Vec<Vec<u8>> = (0..k).map(|j| (0..37).map(|b| (j * 31 + b * 7 + 3) as u8).collect()).collect();
        let refs: Vec<&[u8]> = data.iter().map(|d| d.as_slice()).collect();
        let parity = encode(&refs, m);
        // drop every combination of up to m shards from a few patterns
        for lost in [vec![0], vec![3, 9], vec![0, 1, 2, 3], vec![9, 10, 11, 12], vec![5, 11, 13, 2], vec![10, 11, 12, 13]] {
            let mut shards: Vec<Option<Vec<u8>>> = data.iter().cloned().chain(parity.iter().cloned()).map(Some).collect();
            for &l in &lost {
                shards[l] = None;
            }
            assert!(reconstruct(k, m, &mut shards), "lost {lost:?}");
            for j in 0..k {
                assert_eq!(shards[j].as_ref().unwrap(), &data[j], "lost {lost:?} shard {j}");
            }
        }
        let mut too_many: Vec<Option<Vec<u8>>> = data.iter().cloned().chain(parity.iter().cloned()).map(Some).collect();
        for s in too_many.iter_mut().take(m + 1) {
            *s = None;
        }
        assert!(!reconstruct(k, m, &mut too_many));
    }
}
