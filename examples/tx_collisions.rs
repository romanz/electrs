use anyhow::{Context, Result};
use electrs_rocksdb::{ColumnFamilyDescriptor, IteratorMode, Options, DB};

fn main() -> Result<()> {
    let path = std::env::args().skip(1).next().context("missing DB path")?;
    let cf_names = DB::list_cf(&Options::default(), &path)?;
    let cfs: Vec<_> = cf_names
        .iter()
        .map(|name| ColumnFamilyDescriptor::new(name, Options::default()))
        .collect();
    let db = DB::open_cf_descriptors(&Options::default(), &path, cfs)?;
    let cf = db.cf_handle("txid").context("missing column family")?;

    let mut state: Option<(u64, u32)> = None;
    for row in db.iterator_cf(cf, IteratorMode::Start) {
        let (curr, _value) = row?;
        let curr_prefix = u64::from_le_bytes(curr[..8].try_into()?);
        let curr_height = u32::from_le_bytes(curr[8..].try_into()?);

        if let Some((prev_prefix, prev_height)) = state {
            if prev_prefix == curr_prefix {
                eprintln!(
                    "prefix={:x} heights: {} {}",
                    curr_prefix, prev_height, curr_height
                );
            };
        }
        state = Some((curr_prefix, curr_height));
    }
    Ok(())
}
