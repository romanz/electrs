use serde_json::Value;

error_chain! {
    types {
        Error, ErrorKind, ResultExt, Result;
    }

    errors {
        Daemon(method: String, err: Value) {
            description("RPC error")
            display("{} RPC error: {}", method, err)
        }

        Connection(msg: String) {
            description("Connection error")
            display("Connection error: {}", msg)
        }

        Interrupt(sig: i32) {
            description("Interruption by external signal")
            display("Interrupted by signal {}", sig)
        }

        MethodNotFound(method: String) {
            description("method not found")
            display("method not found '{}'", method)
        }

        InvalidRequest(message: &'static str) {
            description("invalid request")
            display("invalid request: {}", message)
        }

        ParseError {
            description("parse error")
            display("parse error")
        }
    }
}
