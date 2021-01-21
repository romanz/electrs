use crate::chain::{BlockHash, Txid};
use crate::errors::*;
use crate::new_index::ChainQuery;
use bitcoin::hashes::{sha256d::Hash as Sha256dHash, Hash};

pub fn get_tx_merkle_proof(
    chain: &ChainQuery,
    tx_hash: &Txid,
    block_hash: &BlockHash,
) -> Result<(Vec<Sha256dHash>, usize)> {
    let txids = chain
        .get_block_txids(&block_hash)
        .chain_err(|| format!("missing block txids for #{}", block_hash))?;
    let pos = txids
        .iter()
        .position(|txid| txid == tx_hash)
        .chain_err(|| format!("missing txid {}", tx_hash))?;
    let txids = txids.into_iter().map(Sha256dHash::from).collect();

    let (branch, _root) = create_merkle_branch_and_root(txids, pos);
    Ok((branch, pos))
}

pub fn get_header_merkle_proof(
    chain: &ChainQuery,
    height: usize,
    cp_height: usize,
) -> Result<(Vec<Sha256dHash>, Sha256dHash)> {
    if cp_height < height {
        bail!("cp_height #{} < height #{}", cp_height, height);
    }

    let best_height = chain.best_height();
    if best_height < cp_height {
        bail!(
            "cp_height #{} above best block height #{}",
            cp_height,
            best_height
        );
    }

    let heights: Vec<usize> = (0..=cp_height).collect();
    let header_hashes: Vec<BlockHash> = heights
        .into_iter()
        .map(|height| chain.hash_by_height(height))
        .collect::<Option<Vec<BlockHash>>>()
        .chain_err(|| "missing block headers")?;

    let header_hashes = header_hashes.into_iter().map(Sha256dHash::from).collect();
    Ok(create_merkle_branch_and_root(header_hashes, height))
}

pub fn get_id_from_pos(
    chain: &ChainQuery,
    height: usize,
    tx_pos: usize,
    want_merkle: bool,
) -> Result<(Txid, Vec<Sha256dHash>)> {
    let header_hash = chain
        .hash_by_height(height)
        .chain_err(|| format!("missing block #{}", height))?;

    let txids = chain
        .get_block_txids(&header_hash)
        .chain_err(|| format!("missing block txids #{}", height))?;

    let txid = *txids
        .get(tx_pos)
        .chain_err(|| format!("No tx in position #{} in block #{}", tx_pos, height))?;

    let txids = txids.into_iter().map(Sha256dHash::from).collect();

    let branch = if want_merkle {
        create_merkle_branch_and_root(txids, tx_pos).0
    } else {
        vec![]
    };
    Ok((txid, branch))
}

fn merklize(left: Sha256dHash, right: Sha256dHash) -> Sha256dHash {
    let data = [&left[..], &right[..]].concat();
    Sha256dHash::hash(&data)
}

fn create_merkle_branch_and_root(
    mut hashes: Vec<Sha256dHash>,
    mut index: usize,
) -> (Vec<Sha256dHash>, Sha256dHash) {
    let mut merkle = vec![];
    while hashes.len() > 1 {
        if hashes.len() % 2 != 0 {
            let last = *hashes.last().unwrap();
            hashes.push(last);
        }
        index = if index % 2 == 0 { index + 1 } else { index - 1 };
        merkle.push(hashes[index]);
        index /= 2;
        hashes = hashes
            .chunks(2)
            .map(|pair| merklize(pair[0], pair[1]))
            .collect()
    }
    (merkle, hashes[0])
}
