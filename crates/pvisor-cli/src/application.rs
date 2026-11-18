use crate::api::{Application, CommandLine};

impl CommandLine for Application {
    fn run() -> anyhow::Result<()> {
        crate::cli::main()
    }

    fn replay() -> anyhow::Result<()> {
        #[cfg(unix)]
        {
            crate::replay::main()
        }
        #[cfg(not(unix))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "pvisor-replay is unsupported on this platform",
            )
            .into())
        }
    }
}
