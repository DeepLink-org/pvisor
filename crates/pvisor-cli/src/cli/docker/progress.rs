//! Terminal presentation of OCI observations; never writes workload stdout.
use pvisor::docker::PullProgress;
use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

pub(super) struct PullDisplay<W: Write> {
    output: W,
    image: String,
    terminal: bool,
    begun: bool,
    active_line: bool,
    download: Option<(String, Instant)>,
    last_draw: Instant,
}

impl PullDisplay<io::Stderr> {
    pub(super) fn new(image: &str) -> Self {
        Self::with_output(
            image,
            io::stderr(),
            io::stderr().is_terminal() && std::env::var("TERM").ok().as_deref() != Some("dumb"),
        )
    }
}

impl<W: Write> PullDisplay<W> {
    fn with_output(image: &str, output: W, terminal: bool) -> Self {
        Self {
            output,
            image: image.escape_debug().to_string(),
            terminal,
            begun: false,
            active_line: false,
            download: None,
            last_draw: Instant::now(),
        }
    }

    pub(super) fn update(&mut self, event: PullProgress) {
        if !self.begun {
            let _ = writeln!(self.output, "Pulling {}", self.image);
            self.begun = true;
        }
        match event {
            PullProgress::Waiting => self.status("Waiting for image preparation..."),
            PullProgress::Resolving => self.status("Resolving image metadata..."),
            PullProgress::Downloading {
                digest,
                downloaded,
                total,
            } => {
                let first = self.download.as_ref().is_none_or(|(id, _)| id != &digest);
                if first {
                    self.download = Some((digest.clone(), Instant::now()));
                }
                let interval = if self.terminal {
                    Duration::from_millis(100)
                } else {
                    Duration::from_secs(5)
                };
                if !first && total != Some(downloaded) && self.last_draw.elapsed() < interval {
                    return;
                }
                let elapsed = self
                    .download
                    .as_ref()
                    .unwrap()
                    .1
                    .elapsed()
                    .as_secs_f64()
                    .max(0.1);
                let speed = bytes((downloaded as f64 / elapsed) as u64);
                let amount = match total {
                    Some(total) => {
                        let filled = if total == 0 {
                            16
                        } else {
                            (u128::from(downloaded.min(total)) * 16 / u128::from(total)) as usize
                        };
                        format!(
                            "[{}{}] {}/{}",
                            "=".repeat(filled),
                            " ".repeat(16 - filled),
                            bytes(downloaded),
                            bytes(total)
                        )
                    }
                    None => bytes(downloaded),
                };
                self.status(&format!(
                    "{}: Downloading {amount} {speed}/s",
                    short_id(&digest)
                ));
                self.last_draw = Instant::now();
            }
            PullProgress::Verifying { digest } => {
                self.status(&format!("{}: Verifying checksum...", short_id(&digest)))
            }
            PullProgress::Extracting { digest } => {
                self.status(&format!("{}: Extracting...", short_id(&digest)))
            }
            PullProgress::LayerReady { digest, cached } => {
                self.status(&format!(
                    "{}: {}",
                    short_id(&digest),
                    if cached {
                        "Already exists"
                    } else {
                        "Pull complete"
                    }
                ));
                self.finish_line();
            }
        }
    }

    fn status(&mut self, message: &str) {
        if self.terminal {
            let _ = write!(self.output, "\r\x1b[2K{message}");
            self.active_line = true;
        } else {
            let _ = writeln!(self.output, "{message}");
        }
        let _ = self.output.flush();
    }

    fn finish_line(&mut self) {
        if self.active_line {
            let _ = writeln!(self.output);
            let _ = self.output.flush();
            self.active_line = false;
        }
    }
}

impl<W: Write> Drop for PullDisplay<W> {
    fn drop(&mut self) {
        self.finish_line();
    }
}

fn short_id(digest: &str) -> String {
    digest
        .strip_prefix("sha256:")
        .unwrap_or(digest)
        .chars()
        .take(12)
        .collect::<String>()
        .escape_debug()
        .to_string()
}

fn bytes(value: u64) -> String {
    let mut size = value as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if size < 1024.0 || unit == "TiB" {
            return if unit == "B" {
                format!("{value} B")
            } else {
                format!("{size:.1} {unit}")
            };
        }
        size /= 1024.0;
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    const DIGEST: &str = "sha256:123456789abc0000000000000000000000000000000000000000000000000000";

    #[test]
    fn terminal_shows_byte_bar_and_closes_the_line_on_failure() {
        let mut output = Vec::new();
        {
            let mut display = PullDisplay::with_output("ubuntu:latest", &mut output, true);
            display.update(PullProgress::Resolving);
            display.update(PullProgress::Downloading {
                digest: DIGEST.into(),
                downloaded: 1024 * 1024,
                total: Some(2 * 1024 * 1024),
            });
            display.update(PullProgress::Verifying {
                digest: DIGEST.into(),
            });
        }
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Pulling ubuntu:latest\n"));
        assert!(output.contains("123456789abc: Downloading [========        ] 1.0 MiB/2.0 MiB"));
        assert!(output.contains("/s"));
        assert!(output.contains("\r\x1b[2K"));
        assert!(output.ends_with("Verifying checksum...\n"));
        assert!(!output.contains("Pull complete"));
    }

    #[test]
    fn redirected_output_is_plain_throttled_and_marks_cache_hits() {
        let mut output = Vec::new();
        {
            let mut display = PullDisplay::with_output("image", &mut output, false);
            for downloaded in 0..=100 {
                display.update(PullProgress::Downloading {
                    digest: DIGEST.into(),
                    downloaded,
                    total: Some(100),
                });
            }
            display.update(PullProgress::Extracting {
                digest: DIGEST.into(),
            });
            display.update(PullProgress::LayerReady {
                digest: DIGEST.into(),
                cached: false,
            });
            display.update(PullProgress::LayerReady {
                digest: DIGEST.into(),
                cached: true,
            });
        }
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains(['\r', '\x1b']));
        assert_eq!(output.matches("Downloading").count(), 2);
        assert!(output.contains("100 B/100 B"));
        assert!(output.contains("Extracting...\n"));
        assert!(output.contains("Pull complete\n"));
        assert!(output.ends_with("Already exists\n"));
    }

    #[test]
    fn unknown_size_and_broken_output_do_not_invent_completion_or_abort() {
        let mut output = Vec::new();
        {
            let mut display = PullDisplay::with_output("image", &mut output, false);
            display.update(PullProgress::Downloading {
                digest: DIGEST.into(),
                downloaded: 2048,
                total: None,
            });
        }
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Downloading 2.0 KiB"));
        assert!(!output.contains("Pull complete"));
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        PullDisplay::with_output("image", Broken, true).update(PullProgress::Resolving);
    }
}
