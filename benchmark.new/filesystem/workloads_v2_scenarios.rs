//! Frozen, offline application workflows for the V2 filesystem exploration.
//!
//! Tool subprocesses are real git, C compiler and Python tarfile operations.
//! Row operation counts cover explicit Rust I/O only, not tool internals.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use super::{Row, checked, checked_close, fill_pattern, invalid, sync_dir};

const INPUT_FILES: usize = 1024;
const INPUT_SIZE: usize = 4096;
const BUILD_UNITS: usize = 100;
const SMALL_FILES: usize = 128;
const STREAM_BYTES: usize = 32 * 1024 * 1024;

fn tool_output(command: &mut Command) -> io::Result<Output> {
    let description = format!("{command:?}");
    let output = command.output()?;
    checked(
        output.status.success(),
        format!(
            "{description} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(output)
}

fn expected_text(index: usize, repository: bool) -> Vec<u8> {
    let prefix = if repository {
        format!("file {index:04} V2_NEEDLE\n")
    } else {
        format!("package {index:04}\n")
    };
    let mut bytes = vec![if repository { b'x' } else { b'p' }; INPUT_SIZE];
    bytes[..prefix.len()].copy_from_slice(prefix.as_bytes());
    bytes
}

fn input_name(index: usize, repository: bool) -> String {
    format!(
        "d{:02}/f{:03}.{}",
        index / 32,
        index % 32,
        if repository { "txt" } else { "dat" }
    )
}

fn check_bytes(path: &Path, expected: &[u8], row: &mut Row) -> io::Result<()> {
    let bytes = fs::read(path)?;
    checked(
        bytes == expected,
        format!("content mismatch: {}", path.display()),
    )?;
    row.files_verified += 1;
    row.bytes_verified += bytes.len() as u64;
    Ok(())
}

fn check_pattern(path: &Path, length: usize, seed: u64, row: &mut Row) -> io::Result<()> {
    let mut file = File::open(path)?;
    checked(
        file.metadata()?.len() == length as u64,
        format!("size mismatch: {}", path.display()),
    )?;
    let mut actual = vec![0; 64 * 1024];
    let mut expected = vec![0; actual.len()];
    let mut offset = 0;
    while offset < length {
        let count = actual.len().min(length - offset);
        file.read_exact(&mut actual[..count])?;
        fill_pattern(seed, offset as u64, &mut expected[..count]);
        checked(
            actual[..count] == expected[..count],
            format!("pattern mismatch: {} at {offset}", path.display()),
        )?;
        offset += count;
    }
    checked_close(file)?;
    row.files_verified += 1;
    row.bytes_verified += length as u64;
    Ok(())
}

fn build_dir(root: &Path) -> PathBuf {
    root.join("build")
}

fn exact_names(directory: &Path, expected: BTreeSet<String>) -> io::Result<()> {
    let actual = fs::read_dir(directory)?
        .map(|entry| {
            entry?
                .file_name()
                .into_string()
                .map_err(|_| invalid("non-UTF8 workflow entry"))
        })
        .collect::<io::Result<BTreeSet<_>>>()?;
    checked(
        actual == expected,
        format!("workflow namespace differs: {}", directory.display()),
    )
}

fn verify_build_inventory(root: &Path) -> io::Result<()> {
    exact_names(root, BTreeSet::from(["build".to_owned()]))?;
    let mut expected = BTreeSet::from(["main.c".to_owned(), "main.o".to_owned(), "app".to_owned()]);
    for index in 0..BUILD_UNITS {
        for suffix in ["c", "o"] {
            expected.insert(format!("unit{index:03}.{suffix}"));
        }
    }
    exact_names(&build_dir(root), expected)
}

fn compile_unit(directory: &Path, name: &str) -> io::Result<()> {
    tool_output(Command::new("/usr/bin/cc").current_dir(directory).args([
        "-O0",
        "-c",
        &format!("{name}.c"),
        "-o",
        &format!("{name}.o"),
    ]))?;
    Ok(())
}

fn link_program(directory: &Path) -> io::Result<()> {
    let mut command = Command::new("/usr/bin/cc");
    command.current_dir(directory).arg("main.o");
    for index in 0..BUILD_UNITS {
        command.arg(format!("unit{index:03}.o"));
    }
    command.args(["-o", "app"]);
    tool_output(&mut command)?;
    Ok(())
}

fn run_program(directory: &Path, expected: usize) -> io::Result<()> {
    let output = tool_output(Command::new(directory.join("app")).current_dir(directory))?;
    checked(
        output.stdout == format!("{expected}\n").as_bytes(),
        "compiled program output mismatch",
    )
}

/// Case setup is separate from Ready and from each measured round.
pub fn prepare_scenario(id: &str, root: &Path, fixture: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;
    match id {
        "S01" => {
            checked(
                fixture.join("repo/.git").is_dir(),
                "S01 requires frozen real git repository",
            )?;
            checked(Path::new("/usr/bin/git").is_file(), "S01 git unavailable")?;
        }
        "S02" => {
            let destination = build_dir(root);
            fs::create_dir(&destination)?;
            for index in 0..BUILD_UNITS {
                let name = format!("unit{index:03}");
                fs::copy(
                    fixture.join("build").join(format!("{name}.c")),
                    destination.join(format!("{name}.c")),
                )?;
                compile_unit(&destination, &name)?;
            }
            fs::copy(fixture.join("build/main.c"), destination.join("main.c"))?;
            compile_unit(&destination, "main")?;
            link_program(&destination)?;
            run_program(&destination, (0..BUILD_UNITS).sum())?;
            // No deferred setup writes may leak into measured incremental work.
            for entry in fs::read_dir(&destination)? {
                let file = File::open(entry?.path())?;
                file.sync_all()?;
                checked_close(file)?;
            }
            sync_dir(&destination)?;
            sync_dir(root)?;
        }
        "S03" => {
            checked(
                fixture.join("package.tar").is_file(),
                "S03 requires frozen local tar archive",
            )?;
            checked(
                Path::new("/usr/bin/python3").is_file(),
                "S03 Python tarfile unavailable",
            )?;
        }
        _ => return Err(invalid(format!("unknown scenario {id}"))),
    }
    Ok(())
}

fn inspect_repository(fixture: &Path) -> io::Result<Row> {
    let repository = fixture.join("repo");
    let mut row = Row::default();
    let start = Instant::now();
    // Explicit traversal is separate from git's own status/index traversal.
    let mut observed = Vec::new();
    for directory in fs::read_dir(&repository)? {
        let directory = directory?;
        if directory.file_name() == ".git" {
            continue;
        }
        checked(
            directory.file_type()?.is_dir(),
            "unexpected repository root entry",
        )?;
        for entry in fs::read_dir(directory.path())? {
            let entry = entry?;
            checked(
                entry.file_type()?.is_file(),
                "unexpected non-file repository entry",
            )?;
            checked(
                entry.metadata()?.len() == INPUT_SIZE as u64,
                "repository file size mismatch",
            )?;
            observed.push(
                entry
                    .path()
                    .strip_prefix(&repository)
                    .map_err(|e| invalid(e.to_string()))?
                    .to_string_lossy()
                    .into_owned(),
            );
            row.metadata_ops += 1;
        }
    }
    observed.sort();
    let expected: Vec<_> = (0..INPUT_FILES)
        .map(|index| input_name(index, true))
        .collect();
    checked(observed == expected, "repository traversal names differ")?;
    let status = tool_output(Command::new("/usr/bin/git").current_dir(&repository).args([
        "--no-optional-locks",
        "-c",
        "safe.directory=*",
        "-c",
        "core.preloadIndex=false",
        "status",
        "--porcelain",
        "--untracked-files=all",
    ]))?;
    checked(
        status.stdout.is_empty(),
        "frozen repository has dirty git status",
    )?;
    let search = tool_output(Command::new("/usr/bin/git").current_dir(&repository).args([
        "--no-optional-locks",
        "-c",
        "safe.directory=*",
        "grep",
        "-l",
        "V2_NEEDLE",
        "--",
        ".",
    ]))?;
    let mut matches: Vec<_> = String::from_utf8(search.stdout)
        .map_err(|e| invalid(e.to_string()))?
        .lines()
        .map(str::to_owned)
        .collect();
    matches.sort();
    checked(matches == expected, "git grep match set differs")?;
    for index in 0..INPUT_FILES {
        check_bytes(
            &repository.join(input_name(index, true)),
            &expected_text(index, true),
            &mut row,
        )?;
        row.read_ops += 1;
        row.bytes_read += INPUT_SIZE as u64;
    }
    row.work_ns = start.elapsed().as_nanos();
    row.completed_ns = row.work_ns;
    Ok(row)
}

fn incremental_build(root: &Path, round: usize) -> io::Result<Row> {
    let directory = build_dir(root);
    let mut row = Row::default();
    let start = Instant::now();
    // A genuine equal-sized change on every round, including R2/R3.
    let replacement = format!("int unit000(void) {{ return {}; }}\n", 1000 + round);
    let mut source = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(directory.join("unit000.c"))?;
    source.write_all(replacement.as_bytes())?;
    checked_close(source)?;
    row.checked_closes += 1;
    row.write_ops += 1;
    row.bytes_written += replacement.len() as u64;
    compile_unit(&directory, "unit000")?;
    link_program(&directory)?;
    run_program(&directory, (0..BUILD_UNITS).sum::<usize>() + 1000 + round)?;
    row.work_ns = start.elapsed().as_nanos();
    let sync_start = Instant::now();
    for name in ["unit000.c", "unit000.o", "app"] {
        let file = File::open(directory.join(name))?;
        file.sync_all()?;
        row.final_fsyncs += 1;
        checked_close(file)?;
        row.checked_closes += 1;
    }
    sync_dir(&directory)?;
    row.final_fsyncs += 1;
    row.final_sync_ns = sync_start.elapsed().as_nanos();
    row.completed_ns = start.elapsed().as_nanos();
    let validation_start = Instant::now();
    check_bytes(
        &directory.join("unit000.c"),
        replacement.as_bytes(),
        &mut row,
    )?;
    for name in ["unit000.o", "app"] {
        let bytes = fs::read(directory.join(name))?;
        checked(bytes.starts_with(b"\x7fELF"), format!("{name} is not ELF"))?;
        row.files_verified += 1;
        row.bytes_verified += bytes.len() as u64;
    }
    verify_build_inventory(root)?;
    row.validation_ns = validation_start.elapsed().as_nanos();
    Ok(row)
}

fn published_dir(root: &Path, round: usize) -> PathBuf {
    root.join(format!("publish-r{round}"))
}

fn verify_publication(directory: &Path, row: &mut Row) -> io::Result<()> {
    for index in 0..INPUT_FILES {
        check_bytes(
            &directory.join(input_name(index, false)),
            &expected_text(index, false),
            row,
        )?;
    }
    for index in 0..SMALL_FILES {
        check_pattern(
            &directory
                .join("generated")
                .join(format!("small{index:03}.dat")),
            INPUT_SIZE,
            900 + index as u64,
            row,
        )?;
    }
    check_pattern(&directory.join("stream.dat"), STREAM_BYTES, 77, row)?;
    let mut actual = BTreeSet::new();
    let mut expected = BTreeSet::from(["generated/".to_owned(), "stream.dat".to_owned()]);
    for index in 0..32 {
        expected.insert(format!("d{index:02}/"));
    }
    for index in 0..INPUT_FILES {
        expected.insert(input_name(index, false));
    }
    for index in 0..SMALL_FILES {
        expected.insert(format!("generated/small{index:03}.dat"));
    }
    let mut directories = vec![directory.to_path_buf()];
    while let Some(path) = directories.pop() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let relative = entry
                .path()
                .strip_prefix(directory)
                .map_err(|e| invalid(e.to_string()))?
                .to_string_lossy()
                .into_owned();
            if entry.file_type()?.is_dir() {
                actual.insert(format!("{relative}/"));
                directories.push(entry.path());
            } else {
                checked(
                    entry.file_type()?.is_file(),
                    "unexpected publication special file",
                )?;
                actual.insert(relative);
            }
        }
    }
    checked(actual == expected, "publication namespace differs")
}

fn offline_publish(root: &Path, fixture: &Path, round: usize) -> io::Result<Row> {
    let staging = root.join(format!("staging-r{round}"));
    let published = published_dir(root, round);
    let mut row = Row::default();
    let start = Instant::now();
    fs::create_dir(&staging)?;
    // Real tar extraction, using the rootfs Python standard library (no tar binary).
    tool_output(Command::new("/usr/bin/python3").args([
        "-c", "import sys, tarfile\nwith tarfile.open(sys.argv[1], 'r:') as archive:\n archive.extractall(sys.argv[2])\n",
    ]).arg(fixture.join("package.tar")).arg(&staging))?;
    let generated = staging.join("generated");
    fs::create_dir(&generated)?;
    let mut small = vec![0; INPUT_SIZE];
    for index in 0..SMALL_FILES {
        fill_pattern(900 + index as u64, 0, &mut small);
        let mut file = File::create(generated.join(format!("small{index:03}.dat")))?;
        file.write_all(&small)?;
        checked_close(file)?;
        row.write_ops += 1;
        row.bytes_written += INPUT_SIZE as u64;
        row.checked_closes += 1;
    }
    let mut stream = File::create(staging.join("stream.dat"))?;
    let mut block = [0; 1024];
    for offset in (0..STREAM_BYTES).step_by(block.len()) {
        fill_pattern(77, offset as u64, &mut block);
        stream.write_all(&block)?;
        row.write_ops += 1;
        row.bytes_written += block.len() as u64;
    }
    checked_close(stream)?;
    row.checked_closes += 1;
    // Validation is an actual step of this complete workflow before publication.
    verify_publication(&staging, &mut row)?;
    row.work_ns = start.elapsed().as_nanos();
    let sync_start = Instant::now();
    let mut directories = vec![staging.clone()];
    let mut position = 0;
    while position < directories.len() {
        for entry in fs::read_dir(&directories[position])? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                directories.push(entry.path());
            } else {
                let file = File::open(entry.path())?;
                file.sync_all()?;
                row.final_fsyncs += 1;
                checked_close(file)?;
                row.checked_closes += 1;
            }
        }
        position += 1;
    }
    for directory in directories.iter().rev() {
        sync_dir(directory)?;
        row.final_fsyncs += 1;
    }
    fs::rename(&staging, &published)?;
    sync_dir(root)?;
    row.final_fsyncs += 1;
    row.final_sync_ns = sync_start.elapsed().as_nanos();
    row.completed_ns = start.elapsed().as_nanos();
    let validation_start = Instant::now();
    verify_publication(&published, &mut row)?;
    checked(!staging.exists(), "staging remained after atomic publish")?;
    exact_names(root, (1..=round).map(|r| format!("publish-r{r}")).collect())?;
    row.validation_ns = validation_start.elapsed().as_nanos();
    Ok(row)
}

/// Run one round. Each environment calls this for exactly one scenario.
pub fn run_scenario(id: &str, root: &Path, fixture: &Path, round: usize) -> io::Result<Row> {
    match id {
        "S01" => inspect_repository(fixture),
        "S02" => incremental_build(root, round),
        "S03" => offline_publish(root, fixture, round),
        _ => Err(invalid(format!("unknown scenario {id}"))),
    }
}

/// Read-only final retained-state oracle, also usable on the host after VM exit.
/// Program execution is checked in each guest round; this verifies final source
/// and fully reads the retained ELF artifacts without executing them on the host.
pub fn verify_scenario(id: &str, root: &Path, fixture: &Path, rounds: usize) -> io::Result<()> {
    let mut row = Row::default();
    match id {
        "S01" => {
            for index in 0..INPUT_FILES {
                check_bytes(
                    &fixture.join("repo").join(input_name(index, true)),
                    &expected_text(index, true),
                    &mut row,
                )?;
            }
        }
        "S02" => {
            let directory = build_dir(root);
            let replacement = format!("int unit000(void) {{ return {}; }}\n", 1000 + rounds);
            check_bytes(
                &directory.join("unit000.c"),
                replacement.as_bytes(),
                &mut row,
            )?;
            check_bytes(
                &directory.join("main.c"),
                &fs::read(fixture.join("build/main.c"))?,
                &mut row,
            )?;
            for index in 0..BUILD_UNITS {
                if index > 0 {
                    let name = format!("unit{index:03}.c");
                    check_bytes(
                        &directory.join(&name),
                        &fs::read(fixture.join("build").join(name))?,
                        &mut row,
                    )?;
                }
                let object = fs::read(directory.join(format!("unit{index:03}.o")))?;
                checked(object.starts_with(b"\x7fELF"), "retained object is not ELF")?;
            }
            for name in ["main.o", "app"] {
                checked(
                    fs::read(directory.join(name))?.starts_with(b"\x7fELF"),
                    "retained artifact is not ELF",
                )?;
            }
            verify_build_inventory(root)?;
        }
        "S03" => {
            for round in 1..=rounds {
                verify_publication(&published_dir(root, round), &mut row)?;
                checked(
                    !root.join(format!("staging-r{round}")).exists(),
                    "retained staging after publication",
                )?;
            }
            exact_names(
                root,
                (1..=rounds).map(|r| format!("publish-r{r}")).collect(),
            )?;
        }
        _ => return Err(invalid(format!("unknown scenario {id}"))),
    }
    Ok(())
}
