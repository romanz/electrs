extern crate configure_me_codegen;

fn main() {
    configure_me_codegen::build_script_with_man("config_spec.toml").unwrap();
}
