use pvisor_shim::api::{RUNTIME_TYPE, Shim, ShimRuntime};

fn main() {
    if let Err(error) = Shim::run() {
        eprintln!("{RUNTIME_TYPE}: {error:#}");
        std::process::exit(1);
    }
}
