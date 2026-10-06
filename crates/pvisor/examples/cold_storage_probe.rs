// Benchmark: B-COLD-STORAGE-DIAG (benchmark/README.md#b-cold-storage-diag), role diagnostic.
// Motivation: distinguish encoded storage payload from real VM residency benefits.
// Conclusion sought: payload counts, complete integrity and put/restore costs for 64 MiB.
// Design: fresh process/exclusive pool, 64 KiB blocks, unique non-fill blocks, two restores.
// No VM, pager, automatic reclaim, KSM changes or net physical-memory claims.
use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use pvisor::ram_backing::resident::CompressedPool;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, time::Instant};

const BLOCK: usize = 64 * 1024;
const INPUT: usize = 64 * 1024 * 1024;
const BLOCKS: usize = INPUT / BLOCK;
const SEED: u64 = 0x2026_1006_c01d_5eed;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Pattern {
    Fill,
    Patterned,
    Random,
}

#[derive(Parser)]
struct Args {
    #[arg(long, value_enum, default_value = "fill")]
    pattern: Pattern,
    #[arg(long, default_value_t = 1)]
    trial: u32,
}

fn generate(pattern: Pattern, blocks: usize) -> Vec<u8> {
    let mut bytes = vec![0; blocks * BLOCK];
    let mut state = SEED;
    for (index, block) in bytes.chunks_exact_mut(BLOCK).enumerate() {
        match pattern {
            Pattern::Fill => block.fill(0x5a),
            Pattern::Patterned => {
                for (offset, byte) in block.iter_mut().enumerate() {
                    *byte = (offset % 251) as u8;
                }
                block[..8].copy_from_slice(&(index as u64).to_le_bytes());
            }
            Pattern::Random => {
                for word in block.chunks_exact_mut(8) {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    word.copy_from_slice(&state.to_le_bytes());
                }
                // Guarantee block uniqueness independently of PRNG assumptions.
                block[..8].copy_from_slice(&(index as u64).to_le_bytes());
            }
        }
    }
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn cpu_seconds() -> Result<f64> {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // getrusage initializes the entire output on success.
        ensure!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0,
            "getrusage: {}",
            std::io::Error::last_os_error()
        );
        let usage = unsafe { usage.assume_init() };
        Ok(usage.ru_utime.tv_sec as f64
            + usage.ru_utime.tv_usec as f64 / 1e6
            + usage.ru_stime.tv_sec as f64
            + usage.ru_stime.tv_usec as f64 / 1e6)
    }
    #[cfg(not(unix))]
    anyhow::bail!("CPU measurement requires getrusage")
}

fn run(args: Args) -> Result<serde_json::Value> {
    ensure!(args.trial > 0, "trial must be positive");
    let input = generate(args.pattern, BLOCKS);
    let input_sha256 = hash(&input);
    let mut pool = CompressedPool::new(INPUT, BLOCKS);
    let mut objects = Vec::with_capacity(BLOCKS);
    let cpu = cpu_seconds()?;
    let start = Instant::now();
    for block in input.chunks_exact(BLOCK) {
        objects.push(pool.intern(block).context("intern block")?);
    }
    let put_wall_seconds = start.elapsed().as_secs_f64();
    let put_cpu_seconds = cpu_seconds()? - cpu;
    let expected_objects = if matches!(args.pattern, Pattern::Fill) {
        1
    } else {
        BLOCKS
    };
    ensure!(
        pool.object_count() == expected_objects,
        "unexpected object count: {}",
        pool.object_count()
    );
    let logical_encoded_bytes: usize = objects.iter().map(|o| o.encoded_bytes()).sum();

    let mut passes = Vec::new();
    let mut restored = vec![0; INPUT];
    for pass in 1..=2 {
        let cpu = cpu_seconds()?;
        let start = Instant::now();
        for (object, output) in objects.iter().zip(restored.chunks_exact_mut(BLOCK)) {
            object.restore(output).context("restore block")?;
        }
        let wall_seconds = start.elapsed().as_secs_f64();
        let cpu_seconds = cpu_seconds()? - cpu;
        ensure!(
            restored == input,
            "full-byte equality failed on pass {pass}"
        );
        let restored_sha256 = hash(&restored);
        ensure!(
            restored_sha256 == input_sha256,
            "SHA256 failed on pass {pass}"
        );
        passes.push(json!({"pass": pass, "restored_bytes": restored.len(),
            "full_byte_equality": true, "sha256": restored_sha256,
            "wall_seconds": wall_seconds, "cpu_seconds": cpu_seconds}));
        // Force the next pass to actually overwrite the full destination.
        restored.fill(0xa5);
    }

    // Private payload bytes have no public accessor. Reconstruct the frozen codec
    // recipe outside timers, checking sizes; never label this as stored-byte access.
    let mut seen = BTreeSet::new();
    let mut encoded = Sha256::new();
    encoded.update(b"cold-storage-reconstructed-payload-v1\0");
    for (block, object) in input.chunks_exact(BLOCK).zip(&objects) {
        if !seen.insert(object.id()) {
            continue;
        }
        let (tag, payload) = if block.iter().all(|b| *b == block[0]) {
            (0u8, vec![block[0]])
        } else {
            let compressed = zstd::bulk::compress(block, 1)?;
            if compressed.len() < block.len() {
                (2, compressed)
            } else {
                (1, block.to_vec())
            }
        };
        ensure!(
            payload.len() == object.encoded_bytes(),
            "reconstructed encoding size mismatch"
        );
        encoded.update(object.id());
        encoded.update([tag]);
        encoded.update((payload.len() as u64).to_le_bytes());
        encoded.update(payload);
    }
    Ok(json!({"benchmark": "B-COLD-STORAGE-DIAG", "status": "ok",
        "pattern": format!("{:?}", args.pattern).to_lowercase(), "trial": args.trial,
        "pid": std::process::id(), "input_bytes": INPUT, "block_bytes": BLOCK,
        "blocks": BLOCKS, "generator_seed": SEED, "input_sha256": input_sha256,
        "logical_encoded_bytes": logical_encoded_bytes,
        "pool_physical_encoded_unique_bytes": pool.encoded_bytes(),
        "objects": pool.object_count(), "put_wall_seconds": put_wall_seconds,
        "put_cpu_seconds": put_cpu_seconds, "restore_passes": passes,
        "reconstructed_encoding_sha256": hex(&encoded.finalize()),
        "stored_encoding_sha256": null,
        "encoding_hash_scope": "reconstructed first-reference-order unique payload manifest: domain, object id, tag, LE u64 payload length, payload; not private stored-byte access",
        "timing_scope": "put includes intern/hash/codec/dedup; restore includes object checksum; full equality and whole-input SHA256 outside timers",
        "payload_scope": "encoded payload only; excludes indexes, allocator, scratch, retained input and output; NOT physical RAM savings",
        "instance_local_pager": "not implemented by this diagnostic",
        "vm_automatic_reclaim": "unsupported/not implemented by this diagnostic",
        "vms": 0}))
}

fn main() {
    match run(Args::parse()) {
        Ok(value) => println!("{value}"),
        Err(error) => {
            println!(
                "{}",
                json!({"benchmark": "B-COLD-STORAGE-DIAG", "status": "failed", "error": format!("{error:#}")})
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generator_integrity() {
        for pattern in [Pattern::Patterned, Pattern::Random] {
            let bytes = generate(pattern, BLOCKS);
            assert_eq!(bytes, generate(pattern, BLOCKS));
            let hashes: BTreeSet<_> = bytes.chunks_exact(BLOCK).map(hash).collect();
            assert_eq!(hashes.len(), BLOCKS);
            assert!(
                bytes
                    .chunks_exact(BLOCK)
                    .all(|b| b.iter().any(|v| *v != b[0]))
            );
        }
        assert!(generate(Pattern::Fill, 2).iter().all(|b| *b == 0x5a));
    }
    #[test]
    fn generator_codec_integrity() -> Result<()> {
        let mut pool = CompressedPool::new(2 * BLOCK, 2);
        for pattern in [Pattern::Patterned, Pattern::Random] {
            let bytes = generate(pattern, 1);
            let object = pool.intern(&bytes)?;
            if matches!(pattern, Pattern::Patterned) {
                assert!(object.encoded_bytes() < BLOCK);
            } else {
                assert_eq!(object.encoded_bytes(), BLOCK);
            }
            let mut output = vec![0; BLOCK];
            object.restore(&mut output)?;
            assert_eq!(output, bytes);
        }
        Ok(())
    }
}
