extern crate bitcoin;
extern crate electrs;
extern crate error_chain;

use bitcoin::network::constants::Network;
use bitcoin::network::message::NetworkMessage;
use bitcoin::network::socket::Socket;

use electrs::errors::*;

fn run() -> Result<()> {
    // Open socket
    let mut sock = Socket::new(Network::Bitcoin);
    sock.connect("127.0.0.1", 8333)
        .chain_err(|| "failed to connect to node")?;

    let mut outgoing = vec![sock.version_message(0).unwrap()];
    loop {
        for msg in outgoing.split_off(0) {
            eprintln!("send {:?}", msg);
            sock.send_message(msg.clone())
                .chain_err(|| format!("failed to send {:?}", msg))?;
        }
        // Receive new message
        let msg = sock
            .receive_message()
            .chain_err(|| "failed to receive p2p message")?;

        match msg {
            NetworkMessage::Alert(_) => continue, // deprecated
            NetworkMessage::Version(_) => outgoing.push(NetworkMessage::Verack),
            NetworkMessage::Ping(nonce) => outgoing.push(NetworkMessage::Pong(nonce)),
            _ => (),
        };
        eprintln!("recv {:?}", msg);
    }
}

fn main() {
    run().expect("p2p listener failed");
}
