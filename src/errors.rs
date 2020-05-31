error_chain! {
    types {
        Error, ErrorKind, ResultExt, Result;
    }

    errors {
        Connection(msg: String) {
            description("Connection error")
            display("Connection error: {}", msg)
        }

        Interrupt(sig: i32) {
            description("Interruption by external signal")
            display("Interrupted by signal {}", sig)
        }

        DynamoDB(msg: String) {
            description("DynamoDB error")
            display("DynamoDB error: {}", msg)
        }
    }
}
