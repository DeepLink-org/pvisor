//! Static guest for sampling runner PSS while PID 1's first command is alive.

use std::io::{Read, Write};

fn main() {
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap();
    print!("{meminfo}BOOT_MEMORY_READY\n");
    std::io::stdout().flush().unwrap();
    let mut release = [0];
    std::io::stdin().read_exact(&mut release).unwrap();
    assert_eq!(&release, b"!");
}
