use chan_signal::Signal;

error_chain! {
    types {
        Error, ErrorKind, ResultExt, Result;
    }

    errors {
        Connection(msg: String) {
            description("Connection error")
            display("Connection error: {}", msg)
        }

        Interrupt(signal: Signal) {
            description("Interruption by external signal")
            display("Iterrupted by SIG{:?}", signal)
        }
    }
}
