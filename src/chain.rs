use std::collections::HashMap;

use bitcoin::blockdata::block::Header as BlockHeader;
use bitcoin::{BlockHash, Network};

/// A new header found, to be added to the chain at specific height
pub(crate) struct NewHeader {
    header: BlockHeader,
    hash: BlockHash,
    height: usize,
}

impl NewHeader {
    pub(crate) fn from((header, height): (BlockHeader, usize)) -> Self {
        Self {
            header,
            hash: header.block_hash(),
            height,
        }
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    pub(crate) fn hash(&self) -> BlockHash {
        self.hash
    }
}

/// Current blockchain headers' list
pub struct Chain {
    headers: Vec<(BlockHash, BlockHeader)>,
    heights: HashMap<BlockHash, usize>,
}

impl Chain {
    // create an empty chain
    pub fn new(network: Network) -> Self {
        let genesis = bitcoin::blockdata::constants::genesis_block(network);
        let genesis_hash = genesis.block_hash();
        Self {
            headers: vec![(genesis_hash, genesis.header)],
            heights: std::iter::once((genesis_hash, 0)).collect(), // genesis header @ zero height
        }
    }

    pub(crate) fn drop_last_headers(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let new_height = self.height().saturating_sub(n);
        self.update(vec![NewHeader::from((
            self.headers[new_height].1,
            new_height,
        ))])
    }

    /// Load the chain from a collection of headers, up to the given tip
    pub(crate) fn load(&mut self, headers: Vec<BlockHeader>, tip: BlockHash) {
        let genesis_hash = self.headers[0].0;

        let header_map: HashMap<BlockHash, BlockHeader> =
            headers.into_iter().map(|h| (h.block_hash(), h)).collect();
        let mut blockhash = tip;
        let mut new_headers: Vec<&BlockHeader> = Vec::with_capacity(header_map.len());
        while blockhash != genesis_hash {
            let header = match header_map.get(&blockhash) {
                Some(header) => header,
                None => panic!("missing header {} while loading from DB", blockhash),
            };
            blockhash = header.prev_blockhash;
            new_headers.push(header);
        }
        info!("loading {} headers, tip={}", new_headers.len(), tip);
        let new_headers = new_headers.into_iter().rev().copied(); // order by height
        self.update(new_headers.zip(1..).map(NewHeader::from).collect())
    }

    /// Get the block hash at specified height (if exists)
    pub(crate) fn get_block_hash(&self, height: usize) -> Option<BlockHash> {
        self.headers.get(height).map(|(hash, _header)| *hash)
    }

    /// Get the block header at specified height (if exists)
    pub(crate) fn get_block_header(&self, height: usize) -> Option<&BlockHeader> {
        self.headers.get(height).map(|(_hash, header)| header)
    }

    /// Get the block height given the specified hash (if exists)
    pub(crate) fn get_block_height(&self, blockhash: &BlockHash) -> Option<usize> {
        self.heights.get(blockhash).copied()
    }

    /// Update the chain with a list of new headers (possibly a reorg)
    pub(crate) fn update(&mut self, headers: Vec<NewHeader>) {
        if let Some(first_height) = headers.first().map(|h| h.height) {
            for (hash, _header) in self.headers.drain(first_height..) {
                assert!(self.heights.remove(&hash).is_some());
            }
            for (h, height) in headers.into_iter().zip(first_height..) {
                assert_eq!(h.height, height);
                assert_eq!(h.hash, h.header.block_hash());
                assert!(self.heights.insert(h.hash, h.height).is_none());
                self.headers.push((h.hash, h.header));
            }
            info!(
                "chain updated: tip={}, height={}",
                self.headers.last().unwrap().0,
                self.headers.len() - 1
            );
        }
    }

    /// Best block hash
    pub(crate) fn tip(&self) -> BlockHash {
        self.headers.last().expect("empty chain").0
    }

    /// Number of blocks (excluding genesis block)
    pub(crate) fn height(&self) -> usize {
        self.headers.len() - 1
    }

    /// List of block hashes for efficient fork detection and block/header sync
    /// see https://en.bitcoin.it/wiki/Protocol_documentation#getblocks
    pub(crate) fn locator(&self) -> Vec<BlockHash> {
        let mut result = vec![];
        let mut index = self.headers.len() - 1;
        let mut step = 1;
        loop {
            if result.len() >= 10 {
                step *= 2;
            }
            result.push(self.headers[index].0);
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
    use super::{Chain, NewHeader};
    use bitcoin::blockdata::block::Header as BlockHeader;
    use bitcoin::consensus::deserialize;
    use bitcoin::Network::Regtest;
    use hex_lit::hex;

    #[test]
    fn test_genesis() {
        let regtest = Chain::new(Regtest);
        assert_eq!(regtest.height(), 0);
        assert_eq!(
            regtest.tip(),
            "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206"
                .parse()
                .unwrap()
        );
    }

    #[test]
    fn test_updates() {
        let byte_headers = [
hex!("0000002006226e46111a0b59caaf126043eb5bbf28c34f3a5e332a1fc7b2b73cf188910f1d14d3c7ff12d6adf494ebbcfba69baa915a066358b68a2b8c37126f74de396b1d61cc60ffff7f2000000000"),
hex!("00000020d700ae5d3c705702e0a5d9ababd22ded079f8a63b880b1866321d6bfcb028c3fc816efcf0e84ccafa1dda26be337f58d41b438170c357cda33a68af5550590bc1e61cc60ffff7f2004000000"),
hex!("00000020d13731bc59bc0989e06a5e7cab9843a4e17ad65c7ca47cd77f50dfd24f1f55793f7f342526aca9adb6ce8f33d8a07662c97d29d83b9e18117fb3eceecb2ab99b1e61cc60ffff7f2001000000"),
hex!("00000020a603def3e1255cadfb6df072946327c58b344f9bfb133e8e3e280d1c2d55b31c731a68f70219472864a7cb010cd53dc7e0f67e57f7d08b97e5e092b0c3942ad51f61cc60ffff7f2001000000"),
hex!("0000002041dd202b3b2edcdd3c8582117376347d48ff79ff97c95e5ac814820462012e785142dc360975b982ca43eecd14b4ba6f019041819d4fc5936255d7a2c45a96651f61cc60ffff7f2000000000"),
hex!("0000002072e297a2d6b633c44f3c9b1a340d06f3ce4e6bcd79ebd4c4ff1c249a77e1e37c59c7be1ca0964452e1735c0d2740f0d98a11445a6140c36b55770b5c0bcf801f1f61cc60ffff7f2000000000"),
hex!("000000200c9eb5889a8e924d1c4e8e79a716514579e41114ef37d72295df8869d6718e4ac5840f28de43ff25c7b9200aaf7873b20587c92827eaa61943484ca828bdd2e11f61cc60ffff7f2000000000"),
hex!("000000205873f322b333933e656b07881bb399dae61a6c0fa74188b5fb0e3dd71c9e2442f9e2f433f54466900407cf6a9f676913dd54aad977f7b05afcd6dcd81e98ee752061cc60ffff7f2004000000"),
hex!("00000020fd1120713506267f1dba2e1856ca1d4490077d261cde8d3e182677880df0d856bf94cfa5e189c85462813751ab4059643759ed319a81e0617113758f8adf67bc2061cc60ffff7f2000000000"),
hex!("000000200030d7f9c11ef35b89a0eefb9a5e449909339b5e7854d99804ea8d6a49bf900a0304d2e55fe0b6415949cff9bca0f88c0717884a5e5797509f89f856af93624a2061cc60ffff7f2002000000"),
        ];
        let headers: Vec<BlockHeader> = byte_headers
            .iter()
            .map(|byte_header| deserialize(byte_header).unwrap())
            .collect();

        for chunk_size in 1..headers.len() {
            let mut regtest = Chain::new(Regtest);
            let mut height = 0;
            let mut tip = regtest.tip();
            for chunk in headers.chunks(chunk_size) {
                let mut update = vec![];
                for header in chunk {
                    height += 1;
                    tip = header.block_hash();
                    update.push(NewHeader::from((*header, height)))
                }
                regtest.update(update);
                assert_eq!(regtest.tip(), tip);
                assert_eq!(regtest.height(), height);
            }
            assert_eq!(regtest.tip(), headers.last().unwrap().block_hash());
            assert_eq!(regtest.height(), headers.len());
        }

        // test loading from a list of headers and tip
        let mut regtest = Chain::new(Regtest);
        regtest.load(headers.clone(), headers.last().unwrap().block_hash());
        assert_eq!(regtest.height(), headers.len());

        // test getters
        for (header, height) in headers.iter().zip(1usize..) {
            assert_eq!(regtest.get_block_header(height), Some(header));
            assert_eq!(regtest.get_block_hash(height), Some(header.block_hash()));
            assert_eq!(regtest.get_block_height(&header.block_hash()), Some(height));
        }

        // test chain shortening
        for i in (0..=headers.len()).rev() {
            let hash = regtest.get_block_hash(i).unwrap();
            assert_eq!(regtest.get_block_height(&hash), Some(i));
            assert_eq!(regtest.height(), i);
            assert_eq!(regtest.tip(), hash);
            regtest.drop_last_headers(1);
        }
        assert_eq!(regtest.height(), 0);
        assert_eq!(
            regtest.tip(),
            "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206"
                .parse()
                .unwrap()
        );

        regtest.drop_last_headers(1);
        assert_eq!(regtest.height(), 0);
        assert_eq!(
            regtest.tip(),
            "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206"
                .parse()
                .unwrap()
        );

        // test reorg
        let mut regtest = Chain::new(Regtest);
        regtest.load(headers.clone(), headers.last().unwrap().block_hash());
        let height = regtest.height();

        let new_header: BlockHeader = deserialize(&hex!("000000200030d7f9c11ef35b89a0eefb9a5e449909339b5e7854d99804ea8d6a49bf900a0304d2e55fe0b6415949cff9bca0f88c0717884a5e5797509f89f856af93624a7a6bcc60ffff7f2000000000")).unwrap();
        regtest.update(vec![NewHeader::from((new_header, height))]);
        assert_eq!(regtest.height(), height);
        assert_eq!(
            regtest.tip(),
            "0e16637fe0700a7c52e9a6eaa58bd6ac7202652103be8f778680c66f51ad2e9b"
                .parse()
                .unwrap()
        );
    }
}
