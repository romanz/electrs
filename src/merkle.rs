use bitcoin::{hash_types::TxMerkleNode, hashes::Hash, Txid};

pub(crate) struct Proof {
    proof: Vec<TxMerkleNode>,
    position: usize,
}

impl Proof {
    pub(crate) fn create(txids: &[Txid], position: usize) -> Self {
        assert!(position < txids.len());
        let mut offset = position;
        let mut hashes: Vec<TxMerkleNode> = txids
            .iter()
            .map(|txid| TxMerkleNode::from_raw_hash(txid.to_raw_hash()))
            .collect();

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
        Self { proof, position }
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
}

#[cfg(test)]
mod tests {
    use bitcoin::{consensus::encode::deserialize, Block, Txid};
    use std::path::Path;

    use super::Proof;

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
        block.txdata.iter().map(|tx| tx.txid()).collect()
    }
}
