fn main() {
    configure_me_codegen::build_script_auto().unwrap_or_else(|error| error.report_and_exit())
}
