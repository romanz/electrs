use extfmt::Hexlify;
use std::fs::File;
use std::io::Result;
use std::io::Read;
use std::path::Path;

pub fn hexlify(blob: &[u8]) -> String {
    format!("{}", Hexlify(blob))
}

pub fn read_contents<P: AsRef<Path>>(path: P) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    File::open(path)?.read_to_end(&mut buf)?;
    Ok(buf)
}
