//! B-FS-DIAG only: the host delays A; B is submitted in a later close batch.
use std::{fs::{File, OpenOptions}, io::{Read, Write}, path::PathBuf, time::Duration};
fn main() -> std::io::Result<()> {
    let root = PathBuf::from(std::env::args_os().nth(2).expect("workspace"));
    // Prepare both handles first: isolate guest submission from host namespace
    // admission, which may itself wait for in-flight metadata writeback.
    let files = ["pipeline-a", "pipeline-b"].map(|name|
        OpenOptions::new().write(true).create_new(true).open(root.join(name)).unwrap());
    let started = std::time::Instant::now();
    for ((name, byte), mut file) in [("pipeline-a", 0x41), ("pipeline-b", 0x42)].into_iter().zip(files) {
        file.write_all(&vec![byte; 1024 * 1024])?;
        drop(file);
        println!("CLOSED {name} elapsed_ns={}", started.elapsed().as_nanos());
        std::thread::sleep(Duration::from_millis(200));
    }
    for (name, byte) in [("pipeline-a", 0x41), ("pipeline-b", 0x42)] {
        let mut file = File::open(root.join(name))?;
        file.sync_all()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        assert_eq!(bytes, vec![byte; 1024 * 1024]);
    }
    File::open(&root)?.sync_all()?;
    println!("PIPELINE_BYTES_OK");
    Ok(())
}
