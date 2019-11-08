extern crate electrs;

use bincode;
use electrs::{
    config::Config,
    new_index::{Store, TxHistoryKey},
};

fn main() {
    let config = Config::from_args();
    let store = Store::open(&config.db_path.join("newindex"));

    let mut iter = store.history_db().raw_iterator();
    iter.seek(b"H");

    let mut curr_scripthash = [0u8; 32];
    let mut total_entries = 0;

    while iter.valid() {
        let key = iter.key().unwrap();

        if !key.starts_with(b"H") {
            break;
        }

        let entry: TxHistoryKey = bincode::config()
            .big_endian()
            .deserialize(&key)
            .expect("failed to deserialize TxHistoryKey");

        if curr_scripthash != entry.hash {
            if total_entries > 100 {
                println!("{} {}", hex::encode(&curr_scripthash), total_entries);
            }

            curr_scripthash = entry.hash;
            total_entries = 0;
        }

        total_entries = total_entries + 1;

        iter.next();
    }

    if total_entries >= 4000 {
        println!(
            "scripthash,{},{}",
            hex::encode(&curr_scripthash),
            total_entries
        );
    }
}
