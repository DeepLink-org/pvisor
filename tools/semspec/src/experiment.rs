//! Markdown experiment plans, sharing the exact Python clock used by repository benchmarks.
use crate::{parse, runner, seal};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub warmup: usize,
    #[serde(default = "runs")]
    pub runs: usize,
    #[serde(default = "timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub prepare: Vec<String>,
    #[serde(default)]
    pub conclude: Vec<String>,
    #[serde(default)]
    pub cleanup: Vec<String>,
    pub expected_stdout: Option<String>,
}
fn runs() -> usize {
    30
}
fn timeout() -> u64 {
    30
}
#[derive(Serialize)]
pub struct Cell {
    pub id: String,
    pub end: usize,
    pub plan: Plan,
    pub digest: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub id: String,
    pub digest: String,
    pub data: serde_json::Value,
}

pub fn cells(source: &str) -> Result<Vec<Cell>> {
    let mut ids = BTreeSet::new();
    let mut cells = Vec::new();
    for block in parse::marked_blocks(source)? {
        let Some(parameters) = block.parameters.strip_prefix("bench ") else {
            continue;
        };
        let words = shell_words::split(parameters)?;
        ensure!(words.len() == 1, "bench requires only id=NAME");
        let id = words[0]
            .strip_prefix("id=")
            .context("bench requires id=NAME")?;
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                && ids.insert(id.to_owned()),
            "invalid or duplicate benchmark ID"
        );
        let plan: Plan = toml::from_str(&block.script).context("invalid benchmark plan")?;
        ensure!(
            !plan.argv.is_empty() && !plan.argv[0].is_empty() && plan.runs >= 2 && plan.timeout > 0,
            "benchmark requires argv, runs >= 2 and timeout > 0"
        );
        // Bind prose and every executable byte, runtime included. Results are observations, not approvals.
        let digest = seal::digest(&[
            b"semspec/experiment/v1\0",
            source.as_bytes(),
            include_bytes!("../runtime.py"),
        ]);
        cells.push(Cell {
            id: id.into(),
            end: block.end,
            plan,
            digest,
        });
    }
    Ok(cells)
}

pub fn run(cell: &Cell, source: &Path) -> Result<Outcome> {
    let root = tempfile::tempdir()?;
    let mut config = serde_json::to_value(&cell.plan)?;
    config["id"] = cell.id.clone().into();
    config["cwd"] = source
        .parent()
        .unwrap()
        .join(cell.plan.cwd.as_deref().unwrap_or(Path::new(".")))
        .canonicalize()?
        .to_string_lossy()
        .into_owned()
        .into();
    // Omit optional fields, rather than interpreting null as an expected string.
    if cell.plan.expected_stdout.is_none() {
        config.as_object_mut().unwrap().remove("expected_stdout");
    }
    let config_path = root.path().join("plan.json");
    fs::write(&config_path, serde_json::to_vec(&config)?)?;
    let script = root.path().join("runtime.py");
    let mut file = fs::File::create(&script)?;
    file.write_all(include_bytes!("../runtime.py"))?;
    let out = root.path().join("stdout");
    let err = root.path().join("stderr");
    let mut command = Command::new("python3");
    // execute_streams owns process groups. Pass JSON by file in the private temporary
    // directory because runner deliberately gives every interpreter null stdin.
    command.args(["-c", "import json,runpy,sys; r=runpy.run_path(sys.argv[1]); print(json.dumps(r['measure_plan'](json.load(open(sys.argv[2])),sys.argv[3])))"])
        .arg(script).arg(config_path).arg(root.path().join("trials"));
    let seconds = cell
        .plan
        .timeout
        .saturating_mul(4)
        .saturating_mul(cell.plan.runs.saturating_add(cell.plan.warmup) as u64)
        .saturating_add(10);
    let (status, timed_out) =
        runner::execute_streams(command, &out, &err, Duration::from_secs(seconds))?;
    ensure!(
        status.success() && !timed_out,
        "experiment engine failed: {}",
        fs::read_to_string(err)?
    );
    let data: serde_json::Value = serde_json::from_slice(&fs::read(out)?)?;
    Ok(Outcome {
        id: cell.id.clone(),
        digest: cell.digest.clone(),
        data,
    })
}
