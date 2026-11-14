use crate::bitcoin::{hash_types::TxMerkleNode, hashes::Hash, BlockHash, Txid};

pub(crate) struct Proof {
    proof: Vec<TxMerkleNode>,
    position: usize,
    root: TxMerkleNode,
}

impl Proof {
    pub(crate) fn create(txids: &[Txid], position: usize) -> Self {
        let hashes = txids
            .iter()
            .map(|txid| TxMerkleNode::from_raw_hash(txid.to_raw_hash()))
            .collect();
        Self::from_hashes(hashes, position)
    }

    /// Prove that the block at `position` is part of the chain made of `block_hashes`,
    /// which must start at the genesis block (Electrum protocol `cp_height` proofs).
    pub(crate) fn create_for_headers(
        block_hashes: impl Iterator<Item = BlockHash>,
        position: usize,
    ) -> Self {
        let hashes = block_hashes
            .map(|hash| TxMerkleNode::from_raw_hash(hash.to_raw_hash()))
            .collect();
        Self::from_hashes(hashes, position)
    }

    fn from_hashes(mut hashes: Vec<TxMerkleNode>, position: usize) -> Self {
        assert!(position < hashes.len());
        let mut offset = position;

        let mut proof = vec![];
        while hashes.len() > 1 {
            if hashes.len() % 2 != 0 {
                let last = *hashes.last().unwrap();
                hashes.push(last);
            }
            offset = if offset % 2 == 0 {
                offset + 1
            } else {
                offset - 1
            };
            proof.push(hashes[offset]);
            offset /= 2;
            hashes = hashes
                .chunks(2)
                .map(|pair| {
                    let left = pair[0];
                    let right = pair[1];
                    let input = [&left[..], &right[..]].concat();
                    TxMerkleNode::hash(&input)
                })
                .collect()
        }
        Self {
            proof,
            position,
            root: hashes[0],
        }
    }

    pub(crate) fn to_hex(&self) -> Vec<String> {
        self.proof
            .iter()
            .map(|node| format!("{:x}", node))
            .collect()
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn root_hex(&self) -> String {
        format!("{:x}", self.root)
    }
}

#[cfg(test)]
mod tests {
    use crate::bitcoin::{consensus::encode::deserialize, Block, BlockHash, Txid};
    use std::path::Path;

    use super::Proof;

    #[test]
    fn test_header_merkle() {
        // Example from the Electrum protocol docs: `blockchain.block.header(5, 8)` on mainnet.
        let block_hashes = [
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
            "00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048",
            "000000006a625f06636b8bb6ac7b960a8d03705d1ace08b1a19da3fdcc99ddbd",
            "0000000082b5015589a3fdf2d4baff403e6f0be035a5d9742c1cae6295464449",
            "000000004ebadb55ee9096c9a2f8880e09da59c0d68b1c228da88e48844a1485",
            "000000009b7262315dbf071787ad3656097b892abffd1f95a1a022f896f533fc",
            "000000003031a0e73735690c5a1ff2a4be82553b2a12b776fbd3a215dc8f778d",
            "0000000071966c2b1d065fd446b1e485b2c9d9594acd2007ccbd5441cfc89444",
            "00000000408c48f847aa786c2268fc3e6ec2af68e8468a34a28c61b7f1de0dc6",
        ]
        .map(|hash| hash.parse::<BlockHash>().unwrap());

        let proof = Proof::create_for_headers(block_hashes.into_iter(), 5);
        assert_eq!(
            proof.to_hex(),
            vec![
                "000000004ebadb55ee9096c9a2f8880e09da59c0d68b1c228da88e48844a1485",
                "96cbbc84783888e4cc971ae8acf86dd3c1a419370336bb3c634c97695a8c5ac9",
                "965ac94082cebbcffe458075651e9cc33ce703ab0115c72d9e8b1a9906b2b636",
                "89e5daa6950b895190716dd26054432b564ccdc2868188ba1da76de8e1dc7591",
            ]
        );
        assert_eq!(
            proof.root_hex(),
            "e347b1c43fd9b5415bf0d92708db8284b78daf4d0e24f9c3405f45feb85e25db"
        );
    }

    #[test]
    fn test_merkle() {
        let proof = Proof::create(
            &load_block_txids("00000000000000001203c1ea455e38612bdf36e9967fdead11935c8e22283ecc"),
            157,
        );
        assert_eq!(
            proof.to_hex(),
            vec![
                "5d8cfb001d9ec17861ad9c158244239cb6e3298a619b2a5f7b176ddd54459c75",
                "06811172e13312f2e496259d2c8a7262f1192be5223fcf4d6a9ed7f58a2175ba",
                "cbcec841dea3294706809d1510c72b4424d141fac89106af65b70399b1d79f3f",
                "a24d6c3601a54d40f4350e6c8887bf82a873fe8619f95c772b573ec0373119d3",
                "2015c1bb133ee2c972e55fdcd205a9aee7b0122fd74c2f5d5d27b24a562c7790",
                "f379496fef2e603c4e1c03e2179ebaf5153d6463b8d61aa16d41db3321a18165",
                "7a798d6529663fd472d26cc90c434b64f78955747ac2f93c8dcd35b8f684946e",
                "ad3811062b8db664f2342cbff1b491865310b74416dd7b901f14d980886821f8"
            ]
        );

        let proof = Proof::create(
            &load_block_txids("000000000000000002d249a3d89f63ef3fee203adcca7c24008c13fd854513f2"),
            6,
        );
        assert_eq!(
            proof.to_hex(),
            vec![
                "d29769df672657689fd6d293b416ee9211c77fbe243ab7820813f327b0e8dd47",
                "d71f0947b47cab0f64948acfe52d41c293f492fe9627690c330d4004f2852ce4",
                "5f36c4330c727d7c8d98cc906cb286f13a61b5b4cab2124c5d041897834b42d8",
                "e77d181f83355ed38d0e6305fdb87c9637373fd90d1dfb911262ac55d260181e",
                "a8f83ca44dc486d9d45c4cff9567839c254bda96e6960d310a5e471c70c6a95b",
                "e9a5ff7f74cb060b451ed2cd27de038efff4df911f4e0f99e2661b46ebcc7e1c",
                "6b0144095e3f0e0d0551cbaa6c5dfc89387024f836528281b6d290e356e196cf",
                "bb0761b0636ffd387e0ce322289a3579e926b6813e090130a88228bd80cff982",
                "ac327124304cccf6739da308a25bb365a6b63e9344bad2be139b0b02c042567c",
                "42e11f2d67050cd31295f85507ebc7706fc4c1fddf1e5a45b98ae3f7c63d2592",
                "52657042fcfc88067524bf6c5f9a66414c7de4f4fcabcb65bca56fa84cf309b4"
            ]
        );
    }

    fn load_block_txids(block_hash_hex: &str) -> Vec<Txid> {
        let path = Path::new("src")
            .join("tests")
            .join("blocks")
            .join(block_hash_hex);
        let data = std::fs::read(path).unwrap();
        let block: Block = deserialize(&data).unwrap();
        block.txdata.iter().map(|tx| tx.compute_txid()).collect()
    }
}
