use crossbeam_channel::{unbounded, Receiver};
use signal_hook::consts::signal::*;
use signal_hook::iterator::Signals;

use std::thread;

pub(crate) enum Signal {
    Exit,
    Trigger,
}

pub(crate) fn register() -> Receiver<Signal> {
    let ids = [
        SIGINT, SIGTERM,
        SIGUSR1, // allow external triggering (e.g. via bitcoind `blocknotify`)
    ];
    let (tx, rx) = unbounded();
    let mut signals = Signals::new(&ids).expect("failed to register signal hook");
    thread::spawn(move || {
        for id in &mut signals {
            info!("notified via SIG{}", id);
            let signal = match id {
                SIGUSR1 => Signal::Trigger,
                _ => Signal::Exit,
            };
            tx.send(signal).expect("failed to send signal");
        }
    });
    rx
}
