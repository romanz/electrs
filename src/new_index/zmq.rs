use bitcoin::{hashes::Hash, BlockHash};
use crossbeam_channel::Sender;

use crate::util::spawn_thread;

pub fn start(url: &str, block_hash_notify: Sender<BlockHash>) {
    log::debug!("Starting ZMQ thread");
    let ctx = zmq::Context::new();
    let subscriber: zmq::Socket = ctx.socket(zmq::SUB).expect("failed creating subscriber");
    subscriber
        .connect(url)
        .expect("failed connecting subscriber");

    // subscriber.set_subscribe(b"rawtx").unwrap();
    subscriber
        .set_subscribe(b"hashblock")
        .expect("failed subscribing to hashblock");

    spawn_thread("zmq", move || loop {
        match subscriber.recv_multipart(0) {
            Ok(data) => match (data.get(0), data.get(1)) {
                (Some(topic), Some(data)) => {
                    if &topic[..] == &[114, 97, 119, 116, 120] {
                        //rawtx
                    } else if &topic[..] == &[104, 97, 115, 104, 98, 108, 111, 99, 107] {
                        //hashblock
                        let mut reversed = data.to_vec();
                        reversed.reverse();
                        if let Ok(block_hash) = BlockHash::from_slice(&reversed[..]) {
                            log::debug!("New block from ZMQ: {block_hash}");
                            let _ = block_hash_notify.send(block_hash);
                        }
                    }
                }
                _ => (),
            },
            Err(e) => log::warn!("recv_multipart error: {e:?}"),
        }
    });
}
