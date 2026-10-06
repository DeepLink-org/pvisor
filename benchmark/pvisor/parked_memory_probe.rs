//! Identical static Linux task for parked process/container/execution density.
//! Coordinator stdin barrier replaces timed sleep; never rebuild between arms.
use std::{env, fs, io::{self, BufRead, Write}, process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH}};

unsafe extern "C" {
    fn sched_setaffinity(pid: i32, size: usize, mask: *const u8) -> i32;
    fn sched_getaffinity(pid: i32, size: usize, mask: *mut u8) -> i32;
}

fn affinity(text: &str) {
    let mut expected=[0u8;128];
    for cpu in text.split(',') {
        let cpu: usize=cpu.parse().expect("CPU ID");
        assert!(cpu<1024);expected[cpu/8]|=1<<(cpu%8);
    }
    let mut installed=[0u8;128];
    unsafe {
        assert_eq!(sched_setaffinity(0,expected.len(),expected.as_ptr()),0,"install CPU affinity");
        assert!(sched_getaffinity(0,installed.len(),installed.as_mut_ptr())>=0,"read CPU affinity");
    }
    assert_eq!(installed,expected,"CPU affinity differs");
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325,|h,b|(h^u64::from(*b)).wrapping_mul(0x100000001b3))
}

fn main() {
    let args: Vec<String>=env::args().collect();assert_eq!(args.len(),5);
    let kind=&args[1];assert!(kind=="repeated"||kind=="random");
    let seed: u64=args[2].parse().unwrap();assert_ne!(seed,0);
    affinity(&args[3]);let mib: usize=args[4].parse().unwrap();assert_eq!(mib,64);
    let mut data=vec![0u8;mib*1024*1024];let mut state=seed;
    for (i,value) in data.iter_mut().enumerate() {
        *value=if kind=="repeated" {(i%251) as u8} else {
            state^=state<<13;state^=state>>7;state^=state<<17;state as u8
        };
    }
    for i in 0..64 {assert_eq!(fs::read_to_string(format!("files/f{i:03}")).unwrap(),format!("old-{i}\n"));}
    let token=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let expected=checksum(&data);let pid=std::process::id();
    println!("PVISOR_PARKED_READY {{\"kind\":\"{kind}\",\"seed\":{seed},\"bytes\":{},\"token\":\"{token}\",\"pid\":{pid},\"checksum\":\"{expected:016x}\"}}",data.len());
    io::stdout().flush().unwrap();
    let mut command=String::new();io::stdin().lock().read_line(&mut command).unwrap();
    assert_eq!(command.trim(),format!("GO {token}"),"wrong or missing recovered stdin barrier");
    let started=Instant::now();let restored=checksum(std::hint::black_box(&data));
    assert_eq!(restored,expected,"private memory changed while parked");
    let first_scan_ms=started.elapsed().as_secs_f64()*1000.0;
    for i in 0..4 {fs::write(format!("files/f{i:03}"),format!("new-{i}\n")).unwrap();}
    let cwd=env::current_dir().unwrap();
    let status=Command::new("/usr/bin/git").args(["-c",&format!("safe.directory={}",cwd.display()),"status","--porcelain"]).output().unwrap();
    assert!(status.status.success(),"git status failed");
    let text=String::from_utf8(status.stdout).unwrap();
    let mut paths: Vec<_>=text.lines().map(|line|line[3..].to_string()).collect();paths.sort();
    assert_eq!(paths,(0..4).map(|i|format!("files/f{i:03}")).collect::<Vec<_>>(),"wrong Git changes");
    for i in 0..64 {
        assert_eq!(fs::read_to_string(format!("files/f{i:03}")).unwrap(),
            format!("{}-{i}\n",if i<4 {"new"} else {"old"}));
    }
    assert_eq!(checksum(std::hint::black_box(&data)),expected,"post-tool memory changed");
    println!("PVISOR_PARKED_RESULT {{\"kind\":\"{kind}\",\"seed\":{seed},\"bytes\":{},\"token\":\"{token}\",\"pid\":{pid},\"checksum\":\"{restored:016x}\",\"first_scan_ms\":{first_scan_ms},\"tool_ms\":{},\"changes\":4,\"integrity\":\"passed\"}}",data.len(),started.elapsed().as_secs_f64()*1000.0);
}
