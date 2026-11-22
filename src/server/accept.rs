use std::{io, time::Duration};

const RETRY_DELAY: Duration = Duration::from_secs(1);

// Incoming does not end on an accept error. Keep the listener alive, but avoid
// spinning on resource exhaustion and report only the first error in a streak.
pub(crate) fn retry_accept<T>(
    mut incoming: impl Iterator<Item = io::Result<T>>,
    mut report: impl FnMut(&io::Error),
    mut wait: impl FnMut(Duration),
) -> impl Iterator<Item = T> {
    let mut failed = false;
    std::iter::from_fn(move || loop {
        match incoming.next()? {
            Ok(stream) => {
                failed = false;
                return Some(stream);
            }
            Err(error) => {
                if !failed {
                    report(&error);
                    failed = true;
                }
                wait(RETRY_DELAY);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, net::TcpListener, net::TcpStream};

    #[test]
    fn errors_back_off_and_success_resets_reporting() {
        let events = RefCell::new(Vec::new());
        let incoming = [
            Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
            Err(io::Error::from(io::ErrorKind::OutOfMemory)),
            Ok(7),
            Err(io::Error::from(io::ErrorKind::Interrupted)),
            Ok(9),
        ];
        let mut accepted = retry_accept(
            incoming.into_iter(),
            |_| events.borrow_mut().push("report"),
            |delay| {
                assert_eq!(delay, Duration::from_secs(1));
                events.borrow_mut().push("wait");
            },
        );
        assert_eq!(accepted.next(), Some(7));
        assert_eq!(*events.borrow(), ["report", "wait", "wait"]);
        assert_eq!(accepted.next(), Some(9));
        assert_eq!(accepted.next(), None);
        assert_eq!(
            *events.borrow(),
            ["report", "wait", "wait", "report", "wait"]
        );
    }

    #[test]
    fn persistent_errors_wait_each_time_without_repeated_reports() {
        let mut reports = 0;
        let mut waits = 0;
        let incoming = (0..1000).map(|_| Err::<(), _>(io::Error::from_raw_os_error(24)));
        let accepted: Vec<_> = retry_accept(
            incoming,
            |_| reports += 1,
            |delay| {
                assert_eq!(delay, RETRY_DELAY);
                waits += 1;
            },
        )
        .collect();
        assert!(accepted.is_empty());
        assert_eq!(reports, 1);
        assert_eq!(waits, 1000);
    }

    #[test]
    fn accepts_real_loopback_connection_after_synthetic_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let incoming = std::iter::once(Err(io::Error::from_raw_os_error(24)))
            .chain(listener.incoming().take(1));
        let mut waits = 0;
        let streams: Vec<_> = retry_accept(incoming, |_| {}, |_| waits += 1).collect();
        assert_eq!(waits, 1);
        assert_eq!(streams.len(), 1);
        assert_eq!(
            streams[0].peer_addr().unwrap(),
            client.local_addr().unwrap()
        );
    }

    #[test]
    fn healthy_connections_have_no_delay_or_report() {
        let incoming = [Ok(1), Ok(2)];
        let accepted: Vec<_> = retry_accept(
            incoming.into_iter(),
            |_| panic!("healthy listener reported an error"),
            |_| panic!("healthy listener delayed"),
        )
        .enumerate()
        .collect();
        assert_eq!(accepted, [(0, 1), (1, 2)]);
    }
}
