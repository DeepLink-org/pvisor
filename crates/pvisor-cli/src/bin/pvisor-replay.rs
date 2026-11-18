use pvisor_cli::api::{Application, CommandLine};

fn main() -> anyhow::Result<()> {
    Application::replay()
}
