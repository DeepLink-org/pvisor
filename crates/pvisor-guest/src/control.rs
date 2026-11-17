//! Host-only guest control messages; process policy belongs to the launch contract.
#![deny(missing_docs)]
#[cfg(test)]
use crate::api::Request;
use crate::api::{GuestFrameRead, GuestFrameWrite, GuestSignal, MAX_FRAME};
use serde::Serialize;
use std::io::{self, Read, Write};

impl GuestSignal for str {
    fn parse_signal(&self) -> io::Result<i32> {
        let value = self;
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported image stop signal {value}"),
            )
        };
        if let Ok(number) = value.parse::<i32>() {
            return if (1..=64).contains(&number) {
                Ok(number)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid signal",
                ))
            };
        }
        Ok(match value.strip_prefix("SIG").unwrap_or(value) {
            "TERM" | "" => 15,
            "KILL" => 9,
            "INT" => 2,
            "QUIT" => 3,
            "HUP" => 1,
            "USR1" => 10,
            "USR2" => 12,
            "WINCH" => 28,
            _ => return Err(invalid()),
        })
    }
}

/// Write one bounded frame. Failure may follow a partial write; do not retry it.
impl<W: Write + ?Sized> GuestFrameWrite for W {
    fn write_frame<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "guest control frame exceeds 1 MiB",
            ));
        }
        self.write_all(&(bytes.len() as u32).to_be_bytes())?;
        self.write_all(&bytes)
    }
}

/// Read one frame without allocating beyond the protocol bound.
impl<R: Read + ?Sized> GuestFrameRead for R {
    fn read_frame<T: serde::de::DeserializeOwned>(&mut self) -> io::Result<T> {
        let mut size = [0; 4];
        self.read_exact(&mut size)?;
        let size = u32::from_be_bytes(size) as usize;
        if size > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guest control frame exceeds 1 MiB",
            ));
        }
        let mut bytes = vec![0; size];
        self.read_exact(&mut bytes)?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_stop_signals_have_one_host_independent_parser() {
        for (spelling, expected) in [
            ("", 15),
            ("TERM", 15),
            ("SIGTERM", 15),
            ("KILL", 9),
            ("SIGINT", 2),
            ("QUIT", 3),
            ("HUP", 1),
            ("USR1", 10),
            ("SIGUSR2", 12),
            ("WINCH", 28),
            ("1", 1),
            ("64", 64),
        ] {
            assert_eq!(GuestSignal::parse_signal(spelling).unwrap(), expected);
        }
        for invalid in ["0", "65", "-1", "term", "SIGUNKNOWN", "15x"] {
            assert!(GuestSignal::parse_signal(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn frames_reject_oversize_and_unknown_operations() {
        assert!(
            GuestFrameRead::read_frame::<Request>(&mut &((MAX_FRAME + 1) as u32).to_be_bytes()[..])
                .is_err()
        );
        let mut bytes = Vec::new();
        GuestFrameWrite::write_frame(&mut bytes, &Request::Stop { signal: 15 }).unwrap();
        assert!(matches!(
            GuestFrameRead::read_frame::<Request>(&mut &bytes[..]).unwrap(),
            Request::Stop { signal: 15 }
        ));
        assert!(
            serde_json::from_str::<Request>(r#"{"operation":"Ping","host_path":"/"}"#).is_err()
        );
    }

    #[test]
    fn allocator_request_has_no_machine_code_or_address_input() {
        let request = Request::ReclaimAllocator {
            pid: 23,
            generation: 4,
            trim: true,
        };
        let mut frame = Vec::new();
        GuestFrameWrite::write_frame(&mut frame, &request).unwrap();
        assert!(matches!(
            GuestFrameRead::read_frame::<Request>(&mut &frame[..]).unwrap(),
            Request::ReclaimAllocator {
                pid: 23,
                generation: 4,
                trim: true
            }
        ));
        assert!(serde_json::from_str::<Request>(
            r#"{"operation":"ReclaimAllocator","pid":23,"generation":4,"trim":true,"shellcode":[204]}"#
        ).is_err());
    }
}
