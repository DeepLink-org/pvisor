//! A host RLIMIT_FSIZE forces a short virtio-fs WRITE followed by EFBIG.

use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
};

const LIMIT: u64 = 1 << 30;

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(args.len(), 3, "usage: write_ack write|verify FILE");
    let path = &args[2];
    if args[1] == "write" {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        assert_eq!(file.write(b"full").unwrap(), 4);
        file.seek(SeekFrom::Start(LIMIT - 3)).unwrap();
        assert_eq!(file.write(b"partial").unwrap(), 3);
        assert_eq!(file.stream_position().unwrap(), LIMIT);
        assert_eq!(file.write(b"!").unwrap_err().raw_os_error(), Some(27)); // EFBIG
        assert_eq!(file.stream_position().unwrap(), LIMIT);
        file.sync_all().unwrap();
    } else {
        assert_eq!(args[1], "verify");
    }
    let file = File::open(path).unwrap();
    assert_eq!(file.metadata().unwrap().len(), LIMIT);
    let mut bytes = [0xff; 5];
    file.read_exact_at(&mut bytes, 0).unwrap();
    assert_eq!(&bytes, b"full\0");
    assert_eq!(file.read_at(&mut bytes, LIMIT - 4).unwrap(), 4);
    assert_eq!(&bytes[..4], b"\0par");
}
