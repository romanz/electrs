extern crate electrs;

extern crate hex;
extern crate log;

use electrs::{config::Config, store::DBStore};

fn max_collision(store: DBStore, prefix: &[u8]) {
    let prefix_len = prefix.len();
    let mut prev: Option<Vec<u8>> = None;
    let mut collision_max = 0;

    for row in store.iter_scan(prefix) {
        assert!(row.key.starts_with(prefix));
        if let Some(prev) = prev {
            let collision_len = prev
                .iter()
                .zip(row.key.iter())
                .take_while(|(a, b)| a == b)
                .count();
            if collision_len > collision_max {
                eprintln!(
                    "{} bytes collision found:\n{:?}\n{:?}\n",
                    collision_len - prefix_len,
                    revhex(&prev[prefix_len..]),
                    revhex(&row.key[prefix_len..]),
                );
                collision_max = collision_len;
            }
        }
        prev = Some(row.key.to_vec());
    }
}

fn revhex(value: &[u8]) -> String {
    hex::encode(&value.iter().cloned().rev().collect::<Vec<u8>>())
}

fn run(config: Config) {
    if !config.db_path.exists() {
        panic!("DB {:?} must exist when running this tool!", config.db_path);
    }
    let store = DBStore::open(&config.db_path, /*low_memory=*/ false);
    max_collision(store, b"T");
}

fn main() {
    run(Config::from_args());
}
