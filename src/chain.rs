use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;

use bitcoin::{BlockHash, BlockHeader};

use crate::types::{FilePosition, HeaderRow};

/// Current blockchain headers' list
pub(crate) struct Chain {
    rows: Vec<HeaderRow>,
    heights: HashMap<BlockHash, usize>, // map block hash to its height
    positions: BTreeMap<FilePosition, usize>, // map block file position to its height
}

impl Chain {
    // create an empty chain (with only the genesis block)
    pub(crate) fn new(genesis: HeaderRow) -> Self {
        assert_eq!(genesis.header.prev_blockhash, BlockHash::default());
        Self {
            heights: std::iter::once((genesis.hash, 0)).collect(), // genesis block @ zero height
            positions: std::iter::once((genesis.pos, 0)).collect(),
            rows: vec![genesis],
        }
    }

    pub(crate) fn drop_last_headers(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let new_height = self.height().saturating_sub(n);
        let row = self.rows[new_height].clone();
        self.update(vec![row]);
    }

    /// Load the chain from a collecion of headers, up to the given tip
    pub(crate) fn load(&mut self, unordered_rows: Vec<HeaderRow>, tip: BlockHash) {
        let genesis_hash = self.rows[0].header.block_hash();

        let mut rows_map: HashMap<BlockHash, HeaderRow> =
            unordered_rows.into_iter().map(|r| (r.hash, r)).collect();
        let mut blockhash = tip;
        let mut rows = vec![]; // from tip to genesis
        while blockhash != genesis_hash {
            let row = match rows_map.remove(&blockhash) {
                Some(row) => row,
                None => panic!("missing header {} while loading from DB", blockhash),
            };
            blockhash = row.header.prev_blockhash;
            rows.push(row);
        }
        info!("loading {} headers, tip={}", rows.len(), tip);
        self.update(rows.into_iter().rev().collect()); // order by height
    }

    /// Get the block hash at specified height (if exists)
    pub(crate) fn get_block_hash(&self, height: usize) -> Option<BlockHash> {
        self.rows.get(height).map(|r| r.header.block_hash())
    }

    /// Get the block hash at file position
    pub(crate) fn get_header_row_for(&self, pos: FilePosition) -> Option<&HeaderRow> {
        let range = (Bound::Unbounded, Bound::Included(&pos));
        let (_, height) = self.positions.range(range).last()?;
        let row = &self.rows[*height];
        if row.pos.file_id != pos.file_id {
            return None;
        }
        let block_start = row.pos.offset;
        let block_limit = row.pos.offset + row.size;
        if block_start <= pos.offset && pos.offset < block_limit {
            Some(row)
        } else {
            None
        }
    }

    /// Get the block header at specified height (if exists)
    pub(crate) fn get_block_header(&self, height: usize) -> Option<&BlockHeader> {
        self.rows.get(height).map(|info| &info.header)
    }

    /// Get the block height given the specified hash (if exists)
    pub(crate) fn get_block_height(&self, blockhash: BlockHash) -> Option<usize> {
        self.heights.get(&blockhash).copied()
    }

    /// Update the chain with a list of new headers (possibly a reorg)
    pub(crate) fn update(&mut self, rows: Vec<HeaderRow>) {
        if rows.is_empty() {
            return;
        }
        let first_new_height = self.connect_headers(&rows);
        for row in self.rows.drain(first_new_height..) {
            assert!(self.heights.remove(&row.header.block_hash()).is_some());
            assert!(self.positions.remove(&row.pos).is_some());
        }
        for (row, height) in rows.into_iter().zip(first_new_height..) {
            assert!(self.heights.insert(row.hash, height).is_none());
            assert!(self.positions.insert(row.pos, height).is_none());
            self.rows.push(row);
            assert_eq!(height, self.height());
        }
        info!(
            "chain updated: tip={}, height={}",
            self.rows.last().unwrap().hash,
            self.height(),
        );
    }

    fn connect_headers(&self, rows: &[HeaderRow]) -> usize {
        for pair in rows.windows(2) {
            assert_eq!(pair[0].hash, pair[1].header.prev_blockhash);
        }
        let first = rows.first().expect("connect connect empty headers' list");
        if let Some(height) = self.get_block_height(first.header.prev_blockhash) {
            return height + 1;
        }
        if let Some(first_new_height) = self.get_block_height(first.hash) {
            return first_new_height;
        }
        panic!("failed to connect headers to chain: {:?}", rows);
    }

    /// Best block hash
    pub(crate) fn tip(&self) -> BlockHash {
        self.rows.last().expect("empty chain").hash
    }

    /// Number of blocks (excluding genesis block)
    pub(crate) fn height(&self) -> usize {
        self.rows.len() - 1
    }

    /// List of block hashes for efficient fork detection and block/header sync
    /// see https://en.bitcoin.it/wiki/Protocol_documentation#getblocks
    pub(crate) fn locator(&self) -> Vec<BlockHash> {
        let mut result = vec![];
        let mut index = self.height();
        let mut step = 1;
        loop {
            if result.len() >= 10 {
                step *= 2;
            }
            result.push(self.rows[index].hash);
            if index == 0 {
                break;
            }
            index = index.saturating_sub(step);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{Chain, FilePosition, HeaderRow};
    use bitcoin::consensus::deserialize;
    use bitcoin::hashes::hex::{FromHex, ToHex};
    use bitcoin::Block;

    fn regtest_genesis() -> HeaderRow {
        let block_bytes = Vec::from_hex("0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4adae5494dffff7f20020000000101000000010000000000000000000000000000000000000000000000000000000000000000ffffffff4d04ffff001d0104455468652054696d65732030332f4a616e2f32303039204368616e63656c6c6f72206f6e206272696e6b206f66207365636f6e64206261696c6f757420666f722062616e6b73ffffffff0100f2052a01000000434104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000").unwrap();
        let block: Block = deserialize(&block_bytes).unwrap();
        HeaderRow {
            header: block.header,
            hash: block.header.block_hash(),
            pos: FilePosition {
                file_id: 0,
                offset: 0,
            },
            size: block_bytes.len() as u32,
        }
    }

    #[test]
    fn test_genesis() {
        let regtest = Chain::new(regtest_genesis());
        assert_eq!(regtest.height(), 0);
        assert_eq!(
            regtest.tip().to_hex(),
            "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206"
        );
    }

    // created with `generateblock ADDR 10`
    const HEX_BLOCKS: &[&str] = &[
        "0000002006226e46111a0b59caaf126043eb5bbf28c34f3a5e332a1fc7b2b73cf188910fc98e8631211374711ec913c13da27c0ee394227e4bedcd959e80c027da9207afe2fbe361ffff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03510101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000202765a8bf8b681c0c458ac9b0a88751aee2786b22ff5993c223e745cd6340751d1e52eaf9a6127c213ecf28177d090ce7430e92c6ac1db9fe5a78465abdcb24c0e3fbe361ffff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03520101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000200b44e582bba99a1c5dcd6bc4873aa64892eacce78ef01260daeeaff64f26e1100305c258b05c9089330cf35b50ec5550fff0b1f9c7bddbdcc3fe38e08b0664cae3fbe361ffff7f200200000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03530101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "00000020718bcf9f20342fdb3802e1bc62e001b6a0489be9f313ab1f73c4a90772aa343d323e25b2d9b0df77a9bd6051a61d11b0c2733499d2c2ec98cc9e16c71704e500e4fbe361ffff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03540101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "00000020bfcf1eb57e7b8605291755a19cd14b8e3c9e43a35092f5256679683d8c087d4789c375dbf882d8a08e39cda75319592c1f0ee0390e0a8459f0a1c33fc658dc31e4fbe361ffff7f200100000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03550101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000206d72323928ebef586a2ed015d90ce3031ecbac80fda6efe5c17ae40ac3ad0631395bff9ef467b8d84f24934b3ff7ae360fe68b9ad30e61446930180c7f2b4f26e4fbe361ffff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03560101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000208fa24b82c36f9c96900e963a7ba6ab02cea9db79c79ca9657d171dab89e9650cfc94a57dbc4f7ae23d72732479f7eb5608f1db5d82cf1b209f70be7ff8c84820e4fbe361ffff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03570101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "00000020e99dfc425c3023fcbd33d2904136e84c889f09aa0e4d7c73d5e3c99b648a461c884dea2fc958de0030fe6ac8f121787b7af4b12952b48046c248dc3285d54099e5fbe361ffff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03580101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "00000020e6f833e0062b64ac3e8f0ec1bc2f10a69c8e8022c13b90aed62f58792ac6085fb61f3d95caf732fbd1afa12f5d0d49d4d22f263741a1a16292c7cb00f865097de5fbe361ffff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff03590101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000205f93098ed406ce8e423d8fce238c8bd7a5007f37b6e855b21cc144a26414873fd03879408e9336e958b3549936da1d63e68d8b42381a9715c08094ae6c560e77e5fbe361ffff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff035a0101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000",
    ];

    #[test]
    fn test_lookups() {
        fn file_pos(offset: u32) -> FilePosition {
            FilePosition { file_id: 1, offset }
        }

        let mut regtest = Chain::new(regtest_genesis());

        let mut offset = 0;
        let mut limits = vec![];
        let mut rows = vec![];
        for hex_block in HEX_BLOCKS {
            offset += 100; // "separate" blocks within the file
            let block_bytes = Vec::from_hex(hex_block).unwrap();
            let block: Block = deserialize(&block_bytes).unwrap();
            let size = block_bytes.len() as u32;
            let row = HeaderRow {
                header: block.header,
                hash: block.block_hash(),
                pos: file_pos(offset),
                size,
            };
            let end = offset + size;
            limits.push((offset, end));
            offset = end;
            rows.push(row.clone());
            regtest.update(vec![row]);
        }
        for ((offset, end), row) in limits.into_iter().zip(rows) {
            assert_eq!(None, regtest.get_header_row_for(file_pos(offset - 9)));
            assert_eq!(None, regtest.get_header_row_for(file_pos(offset - 4)));
            assert_eq!(None, regtest.get_header_row_for(file_pos(offset - 1)));
            assert_eq!(Some(&row), regtest.get_header_row_for(file_pos(offset)));
            assert_eq!(Some(&row), regtest.get_header_row_for(file_pos(offset + 1)));
            assert_eq!(Some(&row), regtest.get_header_row_for(file_pos(offset + 4)));
            assert_eq!(Some(&row), regtest.get_header_row_for(file_pos(offset + 9)));
            assert_eq!(Some(&row), regtest.get_header_row_for(file_pos(end - 9)));
            assert_eq!(Some(&row), regtest.get_header_row_for(file_pos(end - 4)));
            assert_eq!(Some(&row), regtest.get_header_row_for(file_pos(end - 1)));
            assert_eq!(None, regtest.get_header_row_for(file_pos(end)));
            assert_eq!(None, regtest.get_header_row_for(file_pos(end + 1)));
            assert_eq!(None, regtest.get_header_row_for(file_pos(end + 4)));
            assert_eq!(None, regtest.get_header_row_for(file_pos(end + 9)));
        }
    }

    // created with `invalidateblock` and then `generateblock ADDR 1`
    const REORG_HEX_BLOCK: &str = "000000205f93098ed406ce8e423d8fce238c8bd7a5007f37b6e855b21cc144a26414873fd03879408e9336e958b3549936da1d63e68d8b42381a9715c08094ae6c560e77e9fce361ffff7f200400000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff035a0101ffffffff0200f2052a010000001976a9147f95f4c31a3a70f2c3661573a7d2926b451d760d88ac0000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn test_updates() {
        let rows: Vec<HeaderRow> = HEX_BLOCKS
            .iter()
            .zip(1u16..) // genesis block is not part of this list
            .map(|(hex_header, i)| {
                let block_bytes = Vec::from_hex(hex_header).unwrap();
                let block: Block = deserialize(&block_bytes).unwrap();
                let header = block.header;
                let pos = FilePosition {
                    file_id: i,
                    offset: 0,
                };
                HeaderRow {
                    header,
                    hash: header.block_hash(),
                    pos,
                    size: block_bytes.len() as u32,
                }
            })
            .collect();

        for chunk_size in 1..rows.len() {
            let mut regtest = Chain::new(regtest_genesis());
            let mut height = 0;
            let mut tip = regtest.tip();
            for chunk in rows.chunks(chunk_size) {
                let mut update = vec![];
                for row in chunk {
                    height += 1;
                    tip = row.hash;
                    update.push(row.clone())
                }
                regtest.update(update);
                assert_eq!(regtest.tip(), tip);
                assert_eq!(regtest.height(), height);
            }
            assert_eq!(regtest.tip(), rows.last().unwrap().header.block_hash());
            assert_eq!(regtest.height(), rows.len());
        }

        // test loading from a list of rows and tip
        let mut regtest = Chain::new(regtest_genesis());
        regtest.load(rows.clone(), rows.last().unwrap().header.block_hash());
        assert_eq!(regtest.height(), rows.len());

        // test getters
        for (row, height) in rows.iter().zip(1usize..) {
            assert_eq!(regtest.get_block_header(height), Some(&row.header));
            assert_eq!(
                regtest.get_block_hash(height),
                Some(row.header.block_hash())
            );
            assert_eq!(
                regtest.get_block_height(row.header.block_hash()),
                Some(height)
            );
        }

        // test chain shortening
        for i in (0..=regtest.height()).rev() {
            let hash = regtest.get_block_hash(i).unwrap();
            assert_eq!(regtest.get_block_height(hash), Some(i));
            assert_eq!(regtest.tip(), hash);
            assert_eq!(regtest.height(), i);
            regtest.drop_last_headers(1);
        }
        assert_eq!(regtest.height(), 0);
        assert_eq!(
            regtest.tip().to_hex(),
            "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206" // genesis
        );

        regtest.drop_last_headers(1);
        assert_eq!(regtest.height(), 0);
        assert_eq!(
            regtest.tip().to_hex(),
            "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206" // still genesis
        );

        // test block locations' lookup
        for height in 0..=regtest.height() {
            let hash = regtest.get_block_hash(height).unwrap();
            for offset in vec![0, 1, 2, 4, 8, 16] {
                let pos = FilePosition {
                    file_id: height as u16,
                    offset,
                };
                let row = regtest.get_header_row_for(pos).unwrap();
                assert_eq!(row.hash, hash);
                assert_eq!(row.pos, pos.with_offset(0));
            }
        }

        // test reorg
        let mut regtest = Chain::new(regtest_genesis());
        regtest.load(rows.clone(), rows.last().unwrap().hash);
        let height = regtest.height();

        let reorg_block_bytes = Vec::from_hex(REORG_HEX_BLOCK).unwrap();
        let reorg_block: Block = deserialize(&reorg_block_bytes).unwrap();
        let hash = reorg_block.block_hash();
        assert!(regtest.tip() != hash);

        let row = HeaderRow {
            header: reorg_block.header,
            hash,
            size: reorg_block_bytes.len() as u32,
            pos: FilePosition {
                file_id: 9999,
                offset: 99,
            },
        };
        regtest.update(vec![row]);
        assert_eq!(regtest.height(), height);
        assert_eq!(regtest.tip(), hash);
    }
}
