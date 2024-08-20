use bitcoin::{consensus::Decodable, Block};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use electrs::new_index::schema::bench::*;

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("add_blocks", |b| {
        let block_bytes = bitcoin_test_data::blocks::mainnet_702861();
        let block = Block::consensus_decode(&mut &block_bytes[..]).unwrap();
        let data = Data::new(block);
        // TODO use iter_batched to avoid measuring cloning inputs

        b.iter(move || black_box(add_blocks(&data)))
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
