pub mod common;
use common::Result;

use bitcoind::bitcoincore_rpc::RpcApi;
use electrumd::jsonrpc::serde_json::json;
use electrumd::ElectrumD;

use electrs::chain::Address;

#[cfg(not(feature = "liquid"))]
use bitcoin::address;

/// Test the Electrum RPC server using an headless Electrum wallet
/// This only runs on Bitcoin (non-Liquid) mode.
#[cfg_attr(not(feature = "liquid"), test)]
#[cfg_attr(feature = "liquid", allow(dead_code))]
fn test_electrum() -> Result<()> {
    // Spawn an Electrs Electrum RPC server
    let (electrum_server, electrum_addr, mut tester) = common::init_electrum_tester().unwrap();

    // Spawn an headless Electrum wallet RPC daemon, connected to Electrs
    let mut electrum_wallet_conf = electrumd::Conf::default();
    let server_arg = format!("{}:t", electrum_addr.to_string());
    electrum_wallet_conf.args = vec!["-v", "--server", &server_arg];
    electrum_wallet_conf.view_stdout = true;
    let electrum_wallet =
        ElectrumD::with_conf(electrumd::downloaded_exe_path()?, &electrum_wallet_conf)?;

    let notify_wallet = || {
        electrum_server.notify();
        std::thread::sleep(std::time::Duration::from_millis(200));
    };

    let assert_balance = |confirmed: f64, unconfirmed: f64| {
        let balance = electrum_wallet.call("getbalance", &json!([])).unwrap();
        log::info!("balance: {}", balance);

        assert_eq!(
            balance["confirmed"].as_str(),
            Some(confirmed.to_string().as_str())
        );
        if unconfirmed != 0.0 {
            assert_eq!(
                balance["unconfirmed"].as_str(),
                Some(unconfirmed.to_string().as_str())
            );
        } else {
            assert!(balance["unconfirmed"].is_null())
        }
    };

    let newaddress = || -> Address {
        #[cfg(not(feature = "liquid"))]
        type ParseAddrType = Address<address::NetworkUnchecked>;
        #[cfg(feature = "liquid")]
        type ParseAddrType = Address;

        let addr = electrum_wallet
            .call("createnewaddress", &json!([]))
            .unwrap()
            .as_str()
            .expect("missing address")
            .parse::<ParseAddrType>()
            .expect("invalid address");

        #[cfg(not(feature = "liquid"))]
        let addr = addr.assume_checked();

        addr
    };

    log::info!(
        "Electrum wallet version: {:?}",
        electrum_wallet.call("version", &json!([]))?
    );

    // Send some funds and verify that the balance checks out
    let addr1 = newaddress();
    let addr2 = newaddress();

    assert_balance(0.0, 0.0);

    let txid1 = tester.send(&addr1, "0.1 BTC".parse().unwrap())?;
    notify_wallet();
    assert_balance(0.0, 0.1);

    tester.mine()?;
    notify_wallet();
    assert_balance(0.1, 0.0);

    let txid2 = tester.send(&addr2, "0.2 BTC".parse().unwrap())?;
    notify_wallet();
    assert_balance(0.1, 0.2);

    tester.mine()?;
    notify_wallet();
    assert_balance(0.3, 0.0);

    // Verify that the transaction history checks out
    let history = electrum_wallet.call("onchain_history", &json!([]))?;
    log::debug!("history = {:#?}", history);
    assert_eq!(
        history["transactions"][0]["txid"].as_str(),
        Some(txid1.to_string().as_str())
    );
    assert_eq!(history["transactions"][0]["height"].as_u64(), Some(102));
    assert_eq!(history["transactions"][0]["bc_value"].as_str(), Some("0.1"));

    assert_eq!(
        history["transactions"][1]["txid"].as_str(),
        Some(txid2.to_string().as_str())
    );
    assert_eq!(history["transactions"][1]["height"].as_u64(), Some(103));
    assert_eq!(history["transactions"][1]["bc_value"].as_str(), Some("0.2"));

    // Send an outgoing payment
    electrum_wallet.call(
        "broadcast",
        &json!([electrum_wallet.call(
            "payto",
            &json!({
                "destination": tester.node_client().get_new_address(None, None)?,
                "amount": 0.16,
                "fee": 0.001,
            }),
        )?]),
    )?;
    notify_wallet();
    assert_balance(0.3, -0.161);

    tester.mine()?;
    notify_wallet();
    assert_balance(0.139, 0.0);

    Ok(())
}
