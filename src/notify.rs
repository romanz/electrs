use bitcoin::network::constants::Network;
use bitcoin::network::message::NetworkMessage;
use bitcoin::network::socket::Socket;

use util;

pub fn run() -> util::Channel<NetworkMessage> {
    let chan = util::Channel::new();
    let tx = chan.sender();

    util::spawn_thread("p2p", move || loop {
        // TODO: support testnet and regtest as well.
        let mut sock = Socket::new(Network::Bitcoin);
        if let Err(e) = sock.connect("127.0.0.1", 8333) {
            warn!("failed to connect to node: {}", e);
            continue;
        }
        let mut outgoing = vec![sock.version_message(0).unwrap()];
        loop {
            for msg in outgoing.split_off(0) {
                debug!("send {:?}", msg);
                if let Err(e) = sock.send_message(msg.clone()) {
                    warn!("failed to connect to node: {}", e);
                    break;
                }
            }
            // Receive new message
            let msg = match sock.receive_message() {
                Ok(msg) => msg,
                Err(e) => {
                    warn!("failed to receive p2p message: {}", e);
                    break;
                }
            };
            match msg {
                NetworkMessage::Alert(_) => continue, // deprecated
                NetworkMessage::Version(_) => outgoing.push(NetworkMessage::Verack),
                NetworkMessage::Ping(nonce) => outgoing.push(NetworkMessage::Pong(nonce)),
                _ => (),
            };
            debug!("recv {:?}", msg);
            if tx.send(msg).is_err() {
                warn!("failed to connect to node");
                return;
            }
        }
    });

    chan
}
