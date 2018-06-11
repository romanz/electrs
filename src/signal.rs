use chan;
use chan_signal;
use std::time::Duration;

pub struct Waiter {
    signal: chan::Receiver<chan_signal::Signal>,
}

impl Waiter {
    pub fn new() -> Waiter {
        Waiter {
            signal: chan_signal::notify(&[chan_signal::Signal::INT]),
        }
    }
    pub fn wait(&self, duration: Duration) -> Option<chan_signal::Signal> {
        let signal = &self.signal;
        let timeout = chan::after(duration);
        let result;
        chan_select! {
            signal.recv() -> sig => {
                result = sig;
            },
            timeout.recv() => { result = None; },
        }
        result.map(|sig| info!("received SIG{:?}", sig));
        result
    }
    pub fn poll(&self) -> Option<chan_signal::Signal> {
        self.wait(Duration::from_secs(0))
    }
}
