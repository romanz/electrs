extern crate configure_me_codegen;

fn main() -> Result<(), configure_me_codegen::Error> {
    configure_me_codegen::build_script_with_man("config_spec.toml")
}
