//! Static test executable: LD_PRELOAD must not activate a reclaim worker.
use std::io::{self, Write};
fn main() {
    io::stdout().write_all(b"ready\n").unwrap();
    loop {
        // SAFETY: pause has no pointer arguments; signals simply restart the wait.
        unsafe {
            libc::pause();
        }
    }
}
