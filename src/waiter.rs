use bitcoin::network::serialize::deserialize;
use bitcoin::util::hash::Sha256dHash;
use zmq;

pub struct Waiter {
    sock: zmq::Socket,
}

impl Waiter {
    pub fn new(endpoint: &str) -> Waiter {
        let ctx = zmq::Context::new();
        let sock = ctx.socket(zmq::SocketType::SUB).unwrap();
        sock.set_subscribe(b"hashblock").unwrap();
        sock.connect(endpoint).unwrap();
        Waiter { sock }
    }

    pub fn wait(&self) -> Sha256dHash {
        let mut blockhash = self.sock.recv_multipart(0).unwrap().remove(1);
        blockhash.reverse(); // block hash needs to be LSB-first
        deserialize(&blockhash).unwrap()
    }
}
