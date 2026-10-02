// Reed-Solomon FEC over GF(2^8), byte-identical to crates/rm-protocol/src/fec.rs (polynomial
// 0x11D, generator 2, parity row i / data column j = 1 / ((k + i) XOR j)). Only encoding lives
// here: the Mac sends, Windows rebuilds.
import Foundation

enum GF {
    static let tables: (exp: [UInt8], log: [UInt8]) = {
        var exp = [UInt8](repeating: 0, count: 512), log = [UInt8](repeating: 0, count: 256)
        var x = 1
        for i in 0..<255 {
            exp[i] = UInt8(x); log[x] = UInt8(i)
            x <<= 1
            if x & 0x100 != 0 { x ^= 0x11D }
        }
        for i in 255..<512 { exp[i] = exp[i - 255] }
        return (exp, log)
    }()

    /// mul[a * 256 + b]
    static let mul: [UInt8] = {
        let (exp, log) = tables
        var m = [UInt8](repeating: 0, count: 65536)
        for a in 1..<256 { for b in 1..<256 { m[a * 256 + b] = exp[Int(log[a]) + Int(log[b])] } }
        return m
    }()

    static func inv(_ a: UInt8) -> UInt8 { tables.exp[255 - Int(tables.log[Int(a)])] }
    static func coef(k: Int, i: Int, j: Int) -> UInt8 { inv(UInt8(truncatingIfNeeded: k + i) ^ UInt8(truncatingIfNeeded: j)) }
}

/// `m` parity shards for equally long data shards.
func fecEncode(_ data: [[UInt8]], m: Int) -> [[UInt8]] {
    let k = data.count
    guard k > 0, m > 0 else { return [] }
    let len = data[0].count
    var parity = [[UInt8]](repeating: [UInt8](repeating: 0, count: len), count: m)
    GF.mul.withUnsafeBufferPointer { mul in
        for i in 0..<m {
            parity[i].withUnsafeMutableBufferPointer { p in
                for j in 0..<k {
                    let row = Int(GF.coef(k: k, i: i, j: j)) * 256
                    data[j].withUnsafeBufferPointer { d in
                        for b in 0..<len { p[b] ^= mul[row + Int(d[b])] }
                    }
                }
            }
        }
    }
    return parity
}

/// The vector rm-protocol's `fec::tests::known_vector` checks: the two sides must agree.
func fecSelfTest() -> Bool {
    let data = (0..<3).map { j in (0..<4).map { b in UInt8((j * 7 + b * 13 + 1) & 0xFF) } }
    return fecEncode(data, m: 2) == [[0xff, 0x69, 0x31, 0xb7], [0x9a, 0xdf, 0xb3, 0x59]]
}
