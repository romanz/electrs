use extfmt::Hexlify;

pub fn hexlify(blob: &[u8]) -> String {
    format!("{}", Hexlify(blob))
}
