//! Lowercase hex encode (for debug/fingerprint output).

pub fn encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(hex_digit(x >> 4) as char);
        s.push(hex_digit(x & 0x0f) as char);
    }
    s
}

fn hex_digit(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'a' + (n - 10) }
}
