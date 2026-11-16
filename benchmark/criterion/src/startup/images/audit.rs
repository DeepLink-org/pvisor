//! Final read-only audit of retained prepared/cold-hot operation evidence.
use super::{
    process::{load, save},
    source,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// Recompute clocks, pairing, byte totals and raw-file identities. Fixed
/// campaign sizes are intentionally absent: Criterion owns operation counts.
pub(super) fn cohort(
    output: &Path,
    records: &[Value],
    lazy: bool,
    expected_blocks: u64,
) -> Result<()> {
    let result = (|| -> Result<Value> {
        ensure!(!records.is_empty(), "no checked image operations");
        let mut blocks: BTreeMap<u64, BTreeSet<(String, String)>> = BTreeMap::new();
        let mut evidence = BTreeMap::new();
        for record in records {
            let block = record["block"].as_u64().context("missing block")?;
            let phase = record["phase"].as_str().context("missing phase")?;
            let group = record["condition"].as_str().context("missing condition")?;
            let state = record["state"].as_str().context("missing state")?;
            ensure!(
                ["oci", "turbo", "pvisor"].contains(&group),
                "unexpected image condition"
            );
            ensure!(
                if lazy {
                    ["cold", "warm"].contains(&state)
                } else {
                    state == "prepared"
                },
                "unexpected cache state"
            );
            ensure!(
                blocks
                    .entry(block)
                    .or_default()
                    .insert((group.into(), state.into())),
                "duplicate pair arm"
            );
            let id = if lazy {
                format!("{phase}-{block:06}-{group}-{state}")
            } else {
                format!("{phase}-{block:06}-{group}")
            };
            let folder = if group == "pvisor" {
                output.join("pvisor/trials").join(id)
            } else {
                output.join("trials").join(id)
            };
            let result = &record["result"];
            ensure!(
                result["valid"] == true && load(&folder.join("result.json"))? == *result,
                "record differs from retained validated result"
            );
            let timing = load(&folder.join("timed/timing.json"))?;
            ensure!(
                timing == result["timing"]
                    && timing["valid"] == true
                    && timing["exit_code"] == 0
                    && timing["timed_out"] == false,
                "raw timing is invalid or differs"
            );
            let start = timing["start_ns"].as_u64().context("missing start")?;
            let ready = timing["marker_ns"].as_u64().context("missing marker")?;
            let end = timing["end_ns"].as_u64().context("missing exit")?;
            ensure!(
                start < ready
                    && ready <= end
                    && ready - start == result["ready_ns"].as_u64().context("ready missing")?
                    && end - start
                        == result["completion_ns"]
                            .as_u64()
                            .context("completion missing")?,
                "clock arithmetic mismatch"
            );
            let stdout = fs::read(folder.join("timed/stdout.bin"))?;
            ensure!(
                stdout == source::MARKER
                    && source::bytes_digest(&stdout)
                        == result["stdout_sha256"]
                            .as_str()
                            .context("stdout hash missing")?,
                "stdout marker/hash mismatch"
            );
            ensure!(
                load(&folder.join("timed/argv.json"))? == result["timed_argv"],
                "actual argv differs from result"
            );
            ensure!(
                result["workload_argv"] == json!(source::workload()),
                "different measured application"
            );
            if group == "pvisor" {
                let bundle =
                    PathBuf::from(result["bundle"].as_str().context("missing bundle path")?);
                ensure!(
                    bundle.starts_with(folder.join("stage"))
                        && source::digest(&bundle)?
                            == result["bundle_sha256"]
                                .as_str()
                                .context("bundle hash missing")?,
                    "VM Bundle changed/escaped trial"
                );
                let bundle = load(&bundle)?;
                ensure!(
                    bundle["run"]["state"] == "completed"
                        && bundle["run"]["exit_code"] == 0
                        && bundle["run"]["executor"]["isolation"] == "virtual_machine"
                        && bundle["run"]["command"] == json!(source::workload())
                        && bundle["safety"]["filesystem_changes_staged"] == true,
                    "VM Bundle contract failed"
                );
                ensure!(
                    fs::read(folder.join("workspace/startup-canary"))?
                        == b"unchanged image startup input\n"
                        && fs::read_dir(folder.join("workspace"))?.count() == 1,
                    "VM workspace changed"
                );
                source::require_empty_upper(&folder.join("stage/upper"))?;
                let requests = load(&folder.join("requests.json"))?;
                ensure!(
                    source::digest(&folder.join("requests.json"))?
                        == result["requests_sha256"]
                            .as_str()
                            .context("requests hash missing")?,
                    "request evidence changed"
                );
                ensure!(
                    requests["errors"].as_array().is_some_and(Vec::is_empty),
                    "cache observer errors"
                );
                let mut totals = [0u64; 3];
                let mut at_ready = [0u64; 3];
                for row in requests["rows"]
                    .as_array()
                    .context("request rows missing")?
                {
                    let dir = PathBuf::from(
                        row["raw_dir"]
                            .as_str()
                            .context("raw request path missing")?,
                    );
                    ensure!(
                        dir.starts_with(output.join("pvisor/proxy-frames")),
                        "raw cache evidence escapes owner"
                    );
                    let request = fs::read(dir.join("request.frame"))?;
                    let response = fs::read(dir.join("response.frame"))?;
                    let body = fs::read(dir.join("body.bin"))?;
                    ensure!(
                        source::bytes_digest(&request) == row["request_sha256"].as_str().unwrap()
                            && source::bytes_digest(&response)
                                == row["response_sha256"].as_str().unwrap(),
                        "raw cache frame changed"
                    );
                    let request = parse_frame(&request)?;
                    let response = parse_frame(&response)?;
                    ensure!(
                        request["request"] == row["request"] && response == row["response"],
                        "raw cache frame projection mismatch"
                    );
                    let hash = format!("sha256:{}", source::bytes_digest(&body));
                    ensure!(
                        row["forwarded_body_sha256"] == hash,
                        "raw cache body changed"
                    );
                    if response["status"] == "data" {
                        ensure!(
                            response["length"].as_u64() == Some(body.len() as u64)
                                && response["sha256"] == hash,
                            "Data length/digest mismatch"
                        );
                    } else {
                        ensure!(body.is_empty(), "non-Data cache response has a body");
                    }
                    let op = row["op"].as_str().context("missing operation")?;
                    let amounts = [
                        if op == "read" { body.len() as u64 } else { 0 },
                        if op == "metadata" {
                            body.len() as u64
                        } else {
                            0
                        },
                        fs::metadata(dir.join("response.frame"))?.len() + body.len() as u64,
                    ];
                    let time = row["response_finished_ns"]
                        .as_u64()
                        .context("missing response completion")?;
                    ensure!(
                        time >= start && time <= end,
                        "cache response outside measured process"
                    );
                    for (index, name) in ["content_bytes", "metadata_bytes", "response_bytes"]
                        .iter()
                        .enumerate()
                    {
                        ensure!(
                            row[*name].as_u64() == Some(amounts[index]),
                            "cache classification/frame size mismatch"
                        );
                        totals[index] += amounts[index];
                        if time <= ready {
                            at_ready[index] += amounts[index];
                        }
                    }
                    evidence.insert(dir.display().to_string(), source::tree(&dir)?);
                }
                for (index, name) in ["content_bytes", "metadata_bytes", "response_bytes"]
                    .iter()
                    .enumerate()
                {
                    ensure!(
                        result["traffic"][*name].as_u64() == Some(totals[index])
                            && result["at_ready"][*name].as_u64() == Some(at_ready[index]),
                        "cache aggregate differs from raw frames"
                    );
                }
                if lazy {
                    ensure!(
                        (state == "cold" && totals[0] > 0) || (state == "warm" && totals[0] == 0),
                        "cold/hot cache gate failed"
                    );
                } else {
                    ensure!(totals == [0, 0, 0], "prepared VM fetched cache data");
                }
            } else {
                let registry = load(&folder.join("registry-responses.json"))?;
                ensure!(
                    registry == result["registry"]
                        && registry["totals"] == result["traffic"]
                        && source::digest(&folder.join("registry-responses.log"))?
                            == result["registry_log_sha256"]
                                .as_str()
                                .context("registry log digest missing")?,
                    "registry evidence changed"
                );
                let mut totals = [0u64; 3];
                let mut blob = 0u64;
                for row in registry["requests"]
                    .as_array()
                    .context("registry responses missing")?
                {
                    let bytes = row["body_bytes"]
                        .as_u64()
                        .context("invalid response byte count")?;
                    ensure!(
                        [Some(200), Some(206)].contains(&row["status"].as_u64()),
                        "registry error"
                    );
                    match row["role"].as_str() {
                        Some("content") => totals[0] += bytes,
                        Some("metadata") => totals[1] += bytes,
                        _ => ensure!(bytes == 0, "unclassified registry bytes"),
                    }
                    totals[2] += bytes;
                    if row["method"] == "GET"
                        && row["uri"]
                            .as_str()
                            .context("registry URI")?
                            .contains("/blobs/")
                    {
                        blob += bytes;
                    }
                }
                for (index, name) in ["content_bytes", "metadata_bytes", "response_bytes"]
                    .iter()
                    .enumerate()
                {
                    ensure!(
                        result["traffic"][*name].as_u64() == Some(totals[index]),
                        "registry aggregate mismatch"
                    );
                }
                ensure!(
                    registry["blob_body_bytes"].as_u64() == Some(blob)
                        && registry["ready_boundary_available"] == false,
                    "registry claims unmeasured Ready bytes"
                );
                if lazy {
                    ensure!(
                        (state == "cold" && blob > 0 && totals[0] > 0)
                            || (state == "warm" && blob == 0 && totals[0] == 0),
                        "cold/hot registry gate failed"
                    );
                }
                ensure!(
                    load(&folder.join("host-delta.json"))?["unchanged"] == true,
                    "host device cleanup not verified"
                );
            }
            evidence.insert(folder.display().to_string(), source::tree(&folder)?);
        }
        let expected: BTreeSet<_> = ["oci", "turbo", "pvisor"]
            .into_iter()
            .flat_map(|group| {
                if lazy {
                    vec![
                        (group.to_owned(), "cold".to_owned()),
                        (group.to_owned(), "warm".to_owned()),
                    ]
                } else {
                    vec![(group.to_owned(), "prepared".to_owned())]
                }
            })
            .collect();
        for (block, arms) in &blocks {
            ensure!(
                *arms == expected,
                "incomplete randomized paired block {block}"
            );
            let order = load(&output.join(format!("order-{block:06}.json")))?;
            let mut actual: Vec<_> = order["order"]
                .as_array()
                .context("missing execution order")?
                .iter()
                .map(|i| i.as_u64())
                .collect();
            actual.sort_unstable();
            ensure!(
                actual == [Some(0), Some(1), Some(2)],
                "invalid randomized block order"
            );
        }
        ensure!(
            blocks.len() as u64 == expected_blocks
                && (0..expected_blocks).all(|b| blocks.contains_key(&b)),
            "one or more attempted paired blocks is missing or incomplete"
        );
        Ok(
            json!({"valid":true,"audited_blocks":blocks.len(),"audited_trials":records.len(),"excluded":0,
            "evidence":evidence,"limits":"runtime ownership and live resource proofs retained; no historic samples imported"}),
        )
    })();
    save(
        &output.join("audit.json"),
        &match &result {
            Ok(value) => value.clone(),
            Err(error) => json!({"valid":false,"error":format!("{error:#}")}),
        },
    )?;
    result.map(|_| ())
}
fn parse_frame(raw: &[u8]) -> Result<Value> {
    ensure!(raw.len() >= 4, "truncated frame prefix");
    let length = u32::from_be_bytes(raw[..4].try_into()?) as usize;
    ensure!(
        length > 0 && length <= 1024 * 1024 && raw.len() == length + 4,
        "invalid raw frame length"
    );
    Ok(serde_json::from_slice(&raw[4..])?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_audit_uses_original_lengths() {
        assert!(parse_frame(b"\0\0\0\0").is_err());
        assert!(parse_frame(b"\0\0\0\x02{}extra").is_err());
        assert_eq!(parse_frame(b"\0\0\0\x02{}").unwrap(), json!({}));
    }
    #[test]
    fn empty_and_partial_cohorts_never_pass() {
        let t = tempfile::tempdir().unwrap();
        assert!(cohort(t.path(), &[], false, 1).is_err());
        assert_eq!(load(&t.path().join("audit.json")).unwrap()["valid"], false);
    }
}
