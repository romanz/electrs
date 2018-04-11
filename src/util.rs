use std::char::from_digit;

pub fn revhex(data: &[u8]) -> String {
    let mut ret = String::with_capacity(data.len() * 2);
    for item in data.iter().rev() {
        ret.push(from_digit((*item / 0x10) as u32, 16).unwrap());
        ret.push(from_digit((*item & 0x0f) as u32, 16).unwrap());
    }
    ret
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let mut b = Vec::with_capacity(s.len() / 2);
    let mut modulus = 0;
    let mut buf = 0;

    for byte in s.bytes() {
        buf <<= 4;

        match byte {
            b'A'...b'F' => buf |= byte - b'A' + 10,
            b'a'...b'f' => buf |= byte - b'a' + 10,
            b'0'...b'9' => buf |= byte - b'0',
            _ => {
                return None;
            }
        }

        modulus += 1;
        if modulus == 2 {
            modulus = 0;
            b.push(buf);
        }
    }

    match modulus {
        0 => Some(b),
        _ => None,
    }
}
