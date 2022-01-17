use bitcoind::bitcoincore_rpc::RpcApi;

pub mod common;

use common::Result;

#[test]
fn test() -> Result<()> {
    let (rest_handle, rest_addr, mut tester) = common::init_rest_server().unwrap();
    let bitcoind = tester.bitcoind();

    let addr = bitcoind.get_new_address(None, None)?;
    bitcoind.send_to_address(
        &addr,
        "1.19123 BTC".parse().unwrap(),
        None,
        None,
        None,
        None,
        None,
        None,
    )?;

    tester.sync()?;

    rest_handle.stop();
    Ok(())
}
