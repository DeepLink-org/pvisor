//! Exact controlled 64 MiB payload, shared by observer and Rust guest fixture.
pub const SIZE: usize = 64 * 1024 * 1024;
pub fn page(pattern: &str, id: u64, index: usize, percent: usize) -> [u8; 4096] {
    let mutated = index < 16384 * percent / 100;
    let instance = if mutated {
        id | (1 << 32)
    } else if pattern == "random-unique" {
        id
    } else {
        0
    };
    let mut out = [0; 4096];
    if !mutated && pattern == "repeated" {
        for (i, v) in out.iter_mut().enumerate() {
            *v = i as u8;
        }
    } else {
        let mut x = 20261006u64
            ^ instance.wrapping_mul(0xd1b54a32d192ed03)
            ^ (index as u64).wrapping_mul(0x9e3779b97f4a7c15);
        for word in out.chunks_exact_mut(8) {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            word.copy_from_slice(&x.to_le_bytes());
        }
    }
    out
}
