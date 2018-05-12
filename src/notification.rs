use bitcoin::network::serialize::deserialize;
use bitcoin::util::hash::Sha256dHash;
use std::str;
use zmq;

pub struct Waiter {
    sock: zmq::Socket,
}

pub enum Topic {
    HashBlock(Sha256dHash),
    HashTx(Sha256dHash),
}

impl Waiter {
    pub fn new(endpoint: &str) -> Waiter {
        let ctx = zmq::Context::new();
        let sock = ctx.socket(zmq::SocketType::SUB).unwrap();
        sock.set_subscribe(b"hashblock")
            .expect("failed to subscribe on blocks");
        sock.set_subscribe(b"hashtx")
            .expect("failed to subscribe on transactions");
        sock.connect(endpoint)
            .expect(&format!("failed to connect to {}", endpoint));
        Waiter { sock }
    }

    pub fn wait(&self) -> Topic {
        loop {
            let mut parts = self.sock.recv_multipart(0).unwrap().into_iter();
            let topic = parts.next().expect("missing topic");
            let mut blockhash = parts.next().expect("missing blockhash");
            blockhash.reverse(); // block hash needs to be LSB-first
            let hash: Sha256dHash = deserialize(&blockhash).unwrap();

            match str::from_utf8(&topic).expect("non-string topic") {
                "hashblock" => return Topic::HashBlock(hash),
                "hashtx" => return Topic::HashTx(hash),
                _ => warn!("unknown topic {:?}", topic),
            };
        }
    }
}
