use bitcoind::bitcoincore_rpc::RpcApi;
use serde_json::Value;
use std::collections::HashSet;

use electrs::chain::Txid;

pub mod common;

use common::Result;

#[test]
fn test_rest() -> Result<()> {
    let (rest_handle, rest_addr, mut tester) = common::init_rest_tester().unwrap();

    let get_json = |path: &str| -> Result<Value> {
        Ok(ureq::get(&format!("http://{}{}", rest_addr, path))
            .call()?
            .into_json::<Value>()?)
    };

    let get_plain = |path: &str| -> Result<String> {
        Ok(ureq::get(&format!("http://{}{}", rest_addr, path))
            .call()?
            .into_string()?)
    };

    // Send transaction and confirm it
    let addr1 = tester.newaddress()?;
    let txid1_confirmed = tester.send(&addr1, "1.19123 BTC".parse().unwrap())?;
    tester.mine()?;

    // Send transaction and leave it unconfirmed
    let txid2_mempool = tester.send(&addr1, "0.7113 BTC".parse().unwrap())?;

    // Test GET /tx/:txid
    let res = get_json(&format!("/tx/{}", txid1_confirmed))?;
    log::debug!("tx: {:#?}", res);
    let outs = res["vout"].as_array().expect("array of outs");
    assert!(outs.iter().any(|vout| {
        vout["scriptpubkey_address"].as_str() == Some(&addr1.to_string())
            && vout["value"].as_u64() == Some(119123000)
    }));

    // Test GET /tx/:txid/status
    let res = get_json(&format!("/tx/{}/status", txid1_confirmed))?;
    assert_eq!(res["confirmed"].as_bool(), Some(true));
    assert_eq!(res["block_height"].as_u64(), Some(102));

    let res = get_json(&format!("/tx/{}/status", txid2_mempool))?;
    assert_eq!(res["confirmed"].as_bool(), Some(false));
    assert_eq!(res["block_height"].as_u64(), None);

    // Test GET /address/:address
    let res = get_json(&format!("/address/{}", addr1))?;
    assert_eq!(res["chain_stats"]["funded_txo_count"].as_u64(), Some(1));
    #[cfg(not(feature = "liquid"))]
    assert_eq!(
        res["chain_stats"]["funded_txo_sum"].as_u64(),
        Some(119123000)
    );
    assert_eq!(res["mempool_stats"]["funded_txo_count"].as_u64(), Some(1));
    #[cfg(not(feature = "liquid"))]
    assert_eq!(
        res["mempool_stats"]["funded_txo_sum"].as_u64(),
        Some(71130000)
    );

    // Test GET /address/:address/txs
    let res = get_json(&format!("/address/{}/txs", addr1))?;
    let txs = res.as_array().expect("array of transactions");
    let mut txids = txs
        .iter()
        .map(|tx| tx["txid"].as_str().unwrap().parse().unwrap())
        .collect::<HashSet<Txid>>();
    assert!(txids.remove(&txid1_confirmed));
    assert!(txids.remove(&txid2_mempool));
    assert!(txids.is_empty());

    // Test GET /address-prefix/:prefix
    let addr1_prefix = &addr1.to_string()[0..8];
    let res = get_json(&format!("/address-prefix/{}", addr1_prefix))?;
    let found = res.as_array().expect("array of matching addresses");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].as_str(), Some(addr1.to_string().as_str()));

    // Test GET /blocks/tip/hash
    let bestblockhash = tester.node_client().get_best_block_hash()?;
    let res = get_plain("/blocks/tip/hash")?;
    assert_eq!(res, bestblockhash.to_string());

    let bestblockhash = tester.mine()?;
    let res = get_plain("/blocks/tip/hash")?;
    assert_eq!(res, bestblockhash.to_string());

    // Test GET /blocks/tip/height
    let bestblockheight = tester.node_client().get_block_count()?;
    let res = get_plain("/blocks/tip/height")?;
    assert_eq!(
        res.parse::<u64>().expect("tip block height as an int"),
        bestblockheight
    );

    // Test GET /block-height/:height
    let res = get_plain(&format!("/block-height/{}", bestblockheight))?;
    assert_eq!(res, bestblockhash.to_string());

    // Test GET /blocks
    let res = get_json("/blocks")?;
    let last_blocks = res.as_array().unwrap();
    assert_eq!(last_blocks.len(), 10); // limited to 10 per page
    assert_eq!(
        last_blocks[0]["id"].as_str(),
        Some(bestblockhash.to_string().as_str())
    );

    let bestblockhash = tester.mine()?;
    let res = get_json("/blocks")?;
    let last_blocks = res.as_array().unwrap();
    assert_eq!(
        last_blocks[0]["id"].as_str(),
        Some(bestblockhash.to_string().as_str())
    );

    // Test GET /block/:hash
    let txid = tester.send(&addr1, "0.98765432 BTC".parse().unwrap())?;
    let blockhash = tester.mine()?;

    let res = get_json(&format!("/block/{}", blockhash))?;
    assert_eq!(res["id"].as_str(), Some(blockhash.to_string().as_str()));
    assert_eq!(
        res["height"].as_u64(),
        Some(tester.node_client().get_block_count()?)
    );
    assert_eq!(res["tx_count"].as_u64(), Some(2));

    // Test GET /block/:hash/txs
    let res = get_json(&format!("/block/{}/txs", blockhash))?;
    let block_txs = res.as_array().expect("list of txs");
    assert_eq!(block_txs.len(), 2);
    assert_eq!(block_txs[0]["vin"][0]["is_coinbase"].as_bool(), Some(true));
    assert_eq!(
        block_txs[1]["txid"].as_str(),
        Some(txid.to_string().as_str())
    );

    // Test GET /block/:hash/txid/:index
    let res = get_plain(&format!("/block/{}/txid/1", blockhash))?;
    assert_eq!(res, txid.to_string());

    // Test GET /mempool/txids
    let txid = tester.send(&addr1, "3.21 BTC".parse().unwrap())?;
    let res = get_json("/mempool/txids")?;
    let mempool_txids = res.as_array().expect("list of txids");
    assert_eq!(mempool_txids.len(), 1);
    assert_eq!(mempool_txids[0].as_str(), Some(txid.to_string().as_str()));

    tester.send(&addr1, "0.0001 BTC".parse().unwrap())?;
    let res = get_json("/mempool/txids")?;
    let mempool_txids = res.as_array().expect("list of txids");
    assert_eq!(mempool_txids.len(), 2);

    // Test GET /mempool
    assert_eq!(get_json("/mempool")?["count"].as_u64(), Some(2));

    tester.send(&addr1, "0.00022 BTC".parse().unwrap())?;
    assert_eq!(get_json("/mempool")?["count"].as_u64(), Some(3));

    tester.mine()?;
    assert_eq!(get_json("/mempool")?["count"].as_u64(), Some(0));

    // Elements-only tests
    #[cfg(feature = "liquid")]
    {
        // Test confidential transactions
        {
            let (c_addr, uc_addr) = tester.ct_newaddress()?;
            let txid = tester.send(&c_addr, "3.5 BTC".parse().unwrap())?;
            tester.mine()?;

            let tx = get_json(&format!("/tx/{}", txid))?;
            log::debug!("blinded tx = {:#?}", tx);
            assert_eq!(tx["status"]["confirmed"].as_bool(), Some(true));
            let outs = tx["vout"].as_array().expect("array of outs");
            let vout = outs
                .iter()
                .find(|vout| vout["scriptpubkey_address"].as_str() == Some(&uc_addr.to_string()))
                .expect("our output");
            assert!(vout["value"].is_null());
            assert!(vout["valuecommitment"].is_string());
            assert!(vout["assetcommitment"].is_string());
        }

        // Test blinded asset issuance
        {
            use bitcoin::hashes::{sha256, Hash};
            let contract_hash = sha256::Hash::hash(&[0x11, 0x22, 0x33, 0x44]).to_string();
            let contract_hash = contract_hash.as_str();
            let issuance = tester.node_client().call::<Value>(
                "issueasset",
                &[1.5.into(), 0.into(), true.into(), contract_hash.into()],
            )?;
            tester.mine()?;

            let assetid = issuance["asset"].as_str().expect("asset id");
            let issuance_txid = issuance["txid"].as_str().expect("issuance txid");

            // Test GET /asset/:assetid
            let asset = get_json(&format!("/asset/{}", assetid))?;
            let stats = &asset["chain_stats"];
            assert_eq!(asset["asset_id"].as_str(), Some(assetid));
            assert_eq!(asset["issuance_txin"]["txid"].as_str(), Some(issuance_txid));
            assert_eq!(asset["contract_hash"].as_str(), Some(contract_hash));
            assert_eq!(asset["status"]["confirmed"].as_bool(), Some(true));
            assert_eq!(stats["issuance_count"].as_u64(), Some(1));
            assert_eq!(stats["has_blinded_issuances"].as_bool(), Some(true));
            assert_eq!(stats["issued_amount"].as_u64(), Some(0));

            // Test GET /tx/:txid for issuance tx
            let issuance_tx = get_json(&format!("/tx/{}", issuance_txid))?;
            let issuance_in_index = asset["issuance_txin"]["vin"].as_u64().unwrap();
            let issuance_in = &issuance_tx["vin"][issuance_in_index as usize];
            let issuance_data = &issuance_in["issuance"];
            assert_eq!(issuance_data["asset_id"].as_str(), Some(assetid));
            assert_eq!(issuance_data["is_reissuance"].as_bool(), Some(false));
            assert_eq!(issuance_data["contract_hash"].as_str(), Some(contract_hash));
            assert!(issuance_data["assetamount"].is_null());
            assert!(issuance_data["assetamountcommitment"].is_string());
        }

        // Test unblinded asset issuance
        {
            let issuance = tester
                .node_client()
                .call::<Value>("issueasset", &[1.5.into(), 0.into(), false.into()])?;
            tester.mine()?;
            let assetid = issuance["asset"].as_str().expect("asset id");
            let issuance_txid = issuance["txid"].as_str().expect("issuance txid");

            // Test GET /asset/:assetid
            let asset = get_json(&format!("/asset/{}", assetid))?;
            let stats = &asset["chain_stats"];
            assert_eq!(stats["has_blinded_issuances"].as_bool(), Some(false));
            assert_eq!(stats["issued_amount"].as_u64(), Some(150000000));

            // Test GET /tx/:txid for issuance tx
            let issuance_tx = get_json(&format!("/tx/{}", issuance_txid))?;
            let issuance_in_index = asset["issuance_txin"]["vin"].as_u64().unwrap();
            let issuance_in = &issuance_tx["vin"][issuance_in_index as usize];
            let issuance_data = &issuance_in["issuance"];
            assert_eq!(issuance_data["assetamount"].as_u64(), Some(150000000));
            assert!(issuance_data["assetamountcommitment"].is_null());
        }

        // Test a regular (non-issuance) transaction sending an issued asset
        {
            let issuance = tester
                .node_client()
                .call::<Value>("issueasset", &[1.5.into(), 0.into(), false.into()])?;
            let assetid = issuance["asset"].as_str().expect("asset id");
            tester.mine()?;

            let (c_addr, uc_addr) = tester.ct_newaddress()?;

            // With blinding off
            let txid = tester.send_asset(
                &uc_addr,
                "0.3 BTC".parse().unwrap(), // not actually BTC, but this is what Amount expects
                assetid.parse().unwrap(),
            )?;
            let tx = get_json(&format!("/tx/{}", txid))?;
            let outs = tx["vout"].as_array().expect("array of outs");
            let vout = outs
                .iter()
                .find(|vout| vout["scriptpubkey_address"].as_str() == Some(&uc_addr.to_string()))
                .expect("our output");
            assert_eq!(vout["asset"].as_str(), Some(assetid));
            assert_eq!(vout["value"].as_u64(), Some(30000000));

            // With blinding on
            let txid = tester.send_asset(
                &c_addr,
                "0.3 BTC".parse().unwrap(),
                assetid.parse().unwrap(),
            )?;
            let tx = get_json(&format!("/tx/{}", txid))?;
            let outs = tx["vout"].as_array().expect("array of outs");
            let vout = outs
                .iter()
                .find(|vout| vout["scriptpubkey_address"].as_str() == Some(&uc_addr.to_string()))
                .expect("our output");
            assert!(vout["asset"].is_null());
            assert!(vout["value"].is_null());
            assert!(vout["assetcommitment"].is_string());
            assert!(vout["valuecommitment"].is_string());
        }

        // Test GET /block/:hash
        {
            let bestblockhash = get_plain("/blocks/tip/hash")?;
            let block = get_json(&format!("/block/{}", bestblockhash))?;

            // No PoW-related stuff
            assert!(block["bits"].is_null());
            assert!(block["nonce"].is_null());
            assert!(block["difficulty"].is_null());

            // Dynamic Federations (dynafed) fields
            assert!(block["ext"]["current"]["signblockscript"].is_string());
            assert!(block["ext"]["current"]["fedpegscript"].is_string());
            assert!(block["ext"]["current"]["fedpeg_program"].is_string());
            assert!(block["ext"]["current"]["signblock_witness_limit"].is_u64());
            assert!(block["ext"]["current"]["extension_space"].is_array());
            assert!(block["ext"]["proposed"].is_object());
            assert!(block["ext"]["signblock_witness"].is_array());
        }
    }

    rest_handle.stop();
    Ok(())
}
