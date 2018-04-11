use std::char::from_digit;

pub fn revhex(data: &[u8]) -> String {
    let mut ret = String::with_capacity(data.len() * 2);
    for item in data.iter().rev() {
        ret.push(from_digit((*item / 0x10) as u32, 16).unwrap());
        ret.push(from_digit((*item & 0x0f) as u32, 16).unwrap());
    }
    ret
}
