//! Workload programs run inside the subject image, never host timing adapters.
use super::evidence::*;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

pub(super) const PYTHON: &str = r#"import sys,json,hashlib,sqlite3,ssl,zlib,pathlib,decimal
assert sys.version_info[:2] == (3,12)
assert hashlib.sha256(b'pvisor').hexdigest() == 'EXPECTED_HASH'
assert json.loads(json.dumps(list(range(32)))) == list(range(32))
assert zlib.decompress(zlib.compress(b'image'*100)) == b'image'*100
db=sqlite3.connect(':memory:')
assert db.execute('select sum(value) from json_each(?)',('[1,2,3]',)).fetchone()[0] == 6
assert str(decimal.Decimal('0.1')+decimal.Decimal('0.2')) == '0.3'
print('REFERENCE_READY',flush=True)
"#;
pub(super) fn python_probe() -> String {
    // Generate the constant from the same SHA-256 primitive used by OCI checks.
    PYTHON.replace("EXPECTED_HASH", &format!("{:x}", Sha256::digest(b"pvisor")))
}
pub(super) fn scan_probe(oracle: &Value, twice: bool) -> Result<String> {
    let tuple = format!(
        "({}, {}, '{}')",
        oracle[0].as_u64().context("file count")?,
        oracle[1].as_u64().context("total bytes")?,
        oracle[2].as_str().context("aggregate")?
    );
    let body = format!(
        r#"root='/usr/local/lib/python3.12'; files=[]; total=0; aggregate=hashlib.sha256()
for parent,dirs,names in os.walk(root,followlinks=False):
 for name in names:
  path=os.path.join(parent,name)
  if stat.S_ISREG(os.lstat(path).st_mode): files.append(path)
for path in sorted(files):
 size=os.stat(path).st_size; total+=size
 with open(path,'rb') as stream:
  if size:
   with mmap.mmap(stream.fileno(),0,access=mmap.ACCESS_READ) as data: digest=hashlib.sha256(data).hexdigest()
  else: digest=hashlib.sha256(b'').hexdigest()
 aggregate.update(path.encode()+b'\0'+digest.encode()+b'\n')
assert (len(files),total,aggregate.hexdigest()) == {tuple}, (len(files),total,aggregate.hexdigest())
"#
    );
    let mut code =
        "import os,sys,stat,mmap,hashlib,time,json\nassert sys.version_info[:2] == (3,12)\n"
            .to_owned();
    if twice {
        code.push_str(
            "timings={}\nfor phase in ['first_ns','repeat_ns']:\n start=time.perf_counter_ns()\n",
        );
        for line in body.lines() {
            code.push(' ');
            code.push_str(line);
            code.push('\n');
        }
        code.push_str(" timings[phase]=time.perf_counter_ns()-start\nprint('READ_PHASES '+json.dumps(timings),flush=True)\n");
    } else {
        code.push_str(&body);
    }
    code.push_str("print('REFERENCE_READY',flush=True)\n");
    Ok(code)
}
pub(super) fn payload(workload: &str, oracle: &Value, twice: bool) -> Result<Vec<String>> {
    match workload {
        "shell" => Ok(args(&[
            "/bin/sh",
            "-ec",
            ". /etc/os-release; test \"$ID\" = ubuntu; test \"$VERSION_ID\" = 24.04; printf 'REFERENCE_READY\\n'",
        ])),
        "python" => Ok(args(&[
            "/usr/local/bin/python",
            "-B",
            "-u",
            "-c",
            &python_probe(),
        ])),
        "mmap-scan" => Ok(args(&[
            "/usr/local/bin/python",
            "-B",
            "-u",
            "-c",
            &scan_probe(oracle, twice)?,
        ])),
        _ => anyhow::bail!("unknown workload {workload}"),
    }
}
fn normalize(path: &str) -> Result<String> {
    let mut result = vec![];
    for part in path.split('/') {
        match part {
            "" | "." => (),
            ".." => anyhow::bail!("unsafe OCI path"),
            _ => result.push(part),
        }
    }
    Ok(result.join("/"))
}
/// Apply whiteouts before members of each layer, then authenticate visible files.
pub(super) fn tree_oracle(image: &Value) -> Result<Value> {
    let mut files: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for descriptor in image["manifest"]["layers"].as_array().context("layers")? {
        let path = Path::new(image["source"].as_str().context("source")?)
            .join(digest_hex(descriptor["digest"].as_str().unwrap())?);
        let mut probe = File::open(&path)?;
        let mut magic = [0u8; 2];
        probe.read_exact(&mut magic)?;
        let reader: Box<dyn Read> = if magic == [0x1f, 0x8b] {
            Box::new(flate2::read::GzDecoder::new(File::open(path)?))
        } else {
            Box::new(File::open(path)?)
        };
        let mut archive = tar::Archive::new(reader);
        let mut members = vec![];
        for entry in archive.entries()? {
            let mut entry = entry?;
            let name = normalize(entry.path()?.to_str().context("non-UTF8 OCI path")?)?;
            let kind = entry.header().entry_type();
            let link = entry
                .link_name()?
                .map(|p| normalize(p.to_str().context("non-UTF8 OCI link")?))
                .transpose()?;
            let size = entry.size();
            let digest = if kind.is_file() {
                let mut hash = Sha256::new();
                let mut block = [0u8; 65536];
                loop {
                    let n = entry.read(&mut block)?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&block[..n]);
                }
                Some(format!("{:x}", hash.finalize()))
            } else {
                None
            };
            members.push((name, kind, link, size, digest));
        }
        for (name, _, _, _, _) in &members {
            let (parent, base) = name.rsplit_once('/').unwrap_or(("", name.as_str()));
            if let Some(victim) = base.strip_prefix(".wh.") {
                let victim = if base == ".wh..wh..opq" {
                    parent.to_owned()
                } else {
                    format!("{parent}/{victim}")
                        .trim_start_matches('/')
                        .to_owned()
                };
                files.retain(|k, _| k != &victim && !k.starts_with(&format!("{victim}/")));
            }
        }
        for (name, kind, link, size, digest) in members {
            if name.rsplit('/').next().unwrap_or("").starts_with(".wh.") {
                continue;
            }
            if kind.is_file() {
                files.retain(|k, _| !k.starts_with(&format!("{name}/")));
                files.insert(name, (size, digest.unwrap()));
            } else if kind.is_hard_link() {
                let target = link.context("hardlink target")?;
                let value = files
                    .get(&target)
                    .with_context(|| format!("unresolved OCI hardlink {name} -> {target}"))?
                    .clone();
                files.insert(name, value);
            } else if !kind.is_dir() {
                files.retain(|k, _| k != &name && !k.starts_with(&format!("{name}/")));
            }
        }
    }
    files.retain(|k, _| k.starts_with("usr/local/lib/python3.12/"));
    ensure!(!files.is_empty(), "empty OCI Python tree");
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    for (path, (size, digest)) in &files {
        hash.update(format!("/{path}\0{digest}\n").as_bytes());
        bytes += size;
    }
    Ok(json!([
        files.len(),
        bytes,
        format!("{:x}", hash.finalize())
    ]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_retains_separate_internal_clocks() -> Result<()> {
        let code = scan_probe(&json!([1, 2, "abc"]), true)?;
        assert!(code.contains("['first_ns','repeat_ns']"));
        assert_eq!(code.matches("REFERENCE_READY").count(), 1);
        assert!(!scan_probe(&json!([1, 2, "abc"]), false)?.contains("READ_PHASES"));
        Ok(())
    }
    #[test]
    fn safe_oci_names() {
        assert_eq!(normalize("./usr/x").unwrap(), "usr/x");
        assert!(normalize("usr/../etc").is_err());
    }
    #[test]
    fn layered_oracle_applies_whiteouts_and_hardlinks() -> Result<()> {
        let root = std::env::temp_dir().join(unique());
        std::fs::create_dir(&root)?;
        let make = |entries: Vec<(&str, Option<&str>, &[u8])>| -> Result<Value> {
            let mut builder = tar::Builder::new(Vec::new());
            for (name, link, bytes) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                if let Some(link) = link {
                    header.set_entry_type(tar::EntryType::Link);
                    header.set_size(0);
                    header.set_link_name(link)?;
                } else {
                    header.set_size(bytes.len() as u64);
                }
                header.set_cksum();
                builder.append_data(&mut header, name, bytes)?;
            }
            let bytes = builder.into_inner()?;
            let hash = format!("{:x}", Sha256::digest(&bytes));
            std::fs::write(root.join(&hash), &bytes)?;
            Ok(json!({"digest":format!("sha256:{hash}"),"size":bytes.len()}))
        };
        let base = make(vec![
            ("usr/local/lib/python3.12/a.py", None, b"a"),
            ("usr/local/lib/python3.12/old.py", None, b"old"),
            ("usr/local/lib/python3.12/d/old.py", None, b"old"),
        ])?;
        let layer = make(vec![
            ("usr/local/lib/python3.12/.wh.old.py", None, b""),
            ("usr/local/lib/python3.12/d/.wh..wh..opq", None, b""),
            (
                "usr/local/lib/python3.12/b.py",
                Some("usr/local/lib/python3.12/a.py"),
                b"",
            ),
            ("usr/local/lib/python3.12/d/new.py", None, b"new"),
        ])?;
        let observed = tree_oracle(&json!({"source":root,"manifest":{"layers":[base,layer]}}))?;
        assert_eq!(observed[0], 3);
        assert_eq!(observed[1], 5);
        let mut hash = Sha256::new();
        for (path, bytes) in [
            ("/usr/local/lib/python3.12/a.py", b"a".as_slice()),
            ("/usr/local/lib/python3.12/b.py", b"a".as_slice()),
            ("/usr/local/lib/python3.12/d/new.py", b"new".as_slice()),
        ] {
            hash.update(format!("{path}\0{:x}\n", Sha256::digest(bytes)).as_bytes());
        }
        assert_eq!(observed[2], format!("{:x}", hash.finalize()));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
