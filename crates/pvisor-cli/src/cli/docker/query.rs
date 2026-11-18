//! Bounded Docker formatting/filtering, validated before touching storage.
use super::*;
use pvisor_daemon::api::{PreparedCatalog, PreparedCatalogService};
use serde_json::{Value, json};

pub(super) fn labels(entries: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for entry in entries {
        let (key, value) = entry.split_once('=').unwrap_or((entry, ""));
        ensure!(
            !key.is_empty() && !key.contains('\0') && !value.contains('\0'),
            "invalid label"
        );
        result.insert(key.into(), value.into());
    }
    Ok(result)
}

pub(super) fn copy_endpoint(value: &str) -> anyhow::Result<Option<(String, PathBuf)>> {
    if let Some((container, path)) = value.split_once(':')
        && !container.contains('/')
    {
        ensure!(
            !container.is_empty() && !path.is_empty(),
            "invalid CONTAINER:PATH"
        );
        return Ok(Some((container.into(), Path::new("/").join(path))));
    }
    ensure!(value != "-", "cp archive stdin/stdout is unsupported");
    Ok(None)
}
use std::path::Path;

enum Part {
    Text(String),
    Field { path: Vec<String>, json: bool },
}

pub(super) struct Format {
    parts: Vec<Part>,
    table: bool,
}

impl Format {
    pub(super) fn parse(value: &str) -> anyhow::Result<Self> {
        if value == "json" {
            return Ok(Self {
                parts: vec![Part::Field {
                    path: vec![],
                    json: true,
                }],
                table: false,
            });
        }
        let (table, mut rest) = value
            .strip_prefix("table ")
            .map_or((false, value), |s| (true, s));
        let mut parts = vec![];
        while let Some((text, expression)) = rest.split_once("{{") {
            parts.push(Part::Text(text.replace("\\t", "\t").replace("\\n", "\n")));
            let (expression, next) = expression
                .split_once("}}")
                .context("unclosed format expression")?;
            let expression = expression.trim();
            let (as_json, expression) = expression
                .strip_prefix("json ")
                .map_or((false, expression), |s| (true, s.trim()));
            let (field, index) = if let Some(expression) = expression.strip_prefix("index ") {
                let (field, key) = expression
                    .trim()
                    .split_once(char::is_whitespace)
                    .context("index needs a field and quoted key")?;
                (
                    field,
                    Some(
                        serde_json::from_str::<String>(key.trim())
                            .context("index key must be a JSON string")?,
                    ),
                )
            } else {
                (expression, None)
            };
            ensure!(
                field.starts_with('.')
                    && field
                        .chars()
                        .all(|c| c == '.' || c == '_' || c.is_ascii_alphanumeric()),
                "unsupported format expression; use .Field, json .Field, or index .Field \"key\""
            );
            let mut path: Vec<_> = field
                .trim_start_matches('.')
                .split('.')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            if let Some(key) = index {
                path.push(key);
            }
            parts.push(Part::Field {
                path,
                json: as_json,
            });
            rest = next;
        }
        ensure!(!rest.contains("}}"), "unexpected closing format delimiter");
        parts.push(Part::Text(rest.replace("\\t", "\t").replace("\\n", "\n")));
        Ok(Self { parts, table })
    }

    pub(super) fn render(&self, row: &Value) -> anyhow::Result<String> {
        let mut output = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => output.push_str(text),
                Part::Field {
                    path,
                    json: as_json,
                } => {
                    let mut value = row;
                    for key in path {
                        value = value
                            .get(key)
                            .with_context(|| format!("unknown format field: {key}"))?;
                    }
                    if !as_json && let Some(text) = value.as_str() {
                        output.push_str(text);
                    } else {
                        output.push_str(&serde_json::to_string(value)?);
                    }
                }
            }
        }
        Ok(output)
    }

    pub(super) fn header(&self) -> Option<String> {
        self.table.then(|| {
            self.parts
                .iter()
                .map(|part| match part {
                    Part::Text(text) => text.clone(),
                    Part::Field { path, .. } => path
                        .last()
                        .map(|s| s.to_ascii_uppercase())
                        .unwrap_or_default(),
                })
                .collect()
        })
    }
}

struct Filters(BTreeMap<String, Vec<String>>);
impl Filters {
    fn resolve_ancestors(&mut self, store: &Store) -> anyhow::Result<()> {
        if let Some(values) = self.0.get_mut("ancestor") {
            let mut resolved = Vec::new();
            for selector in values.iter() {
                if let Some(id) = store.image_id(selector)? {
                    resolved.push(id);
                } else {
                    let prefix = selector.strip_prefix("sha256:").unwrap_or(selector);
                    if prefix.len() >= 4 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        // A running Session retains an untagged image pin even
                        // after its last catalog alias has been removed.
                        resolved.push(selector.clone());
                    }
                }
            }
            *values = resolved;
        }
        Ok(())
    }

    fn parse(values: &[String], image: bool) -> anyhow::Result<Self> {
        let mut result = BTreeMap::<String, Vec<String>>::new();
        for value in values {
            let (key, value) = value.split_once('=').context("filter requires KEY=VALUE")?;
            let supported = if image {
                ["label", "reference", "dangling"].contains(&key)
            } else {
                [
                    "id", "name", "status", "label", "ancestor", "exited", "health",
                ]
                .contains(&key)
            };
            ensure!(
                supported && !value.is_empty(),
                "unsupported or empty filter: {key}"
            );
            match key {
                "exited" => {
                    value.parse::<i32>().context("invalid exit-code filter")?;
                }
                "dangling" => ensure!(
                    ["true", "false"].contains(&value),
                    "invalid dangling filter"
                ),
                "health" => ensure!(
                    ["starting", "healthy", "unhealthy", "none"].contains(&value),
                    "invalid health filter"
                ),
                "reference" => {
                    globset::Glob::new(value)?;
                }
                "label" => {
                    labels(&[value.into()])?;
                }
                _ => {}
            }
            result.entry(key.into()).or_default().push(value.into());
        }
        Ok(Self(result))
    }

    fn matches(&self, metadata: &Value, image: bool) -> anyhow::Result<bool> {
        for (key, values) in &self.0 {
            let matches = |value: &String| -> anyhow::Result<bool> {
                Ok(match key.as_str() {
                    "label" => {
                        let (key, expected) = value
                            .split_once('=')
                            .map_or((value.as_str(), None), |(k, v)| (k, Some(v)));
                        metadata["Config"]["Labels"].get(key).is_some_and(|actual| {
                            expected.is_none_or(|expected| actual.as_str() == Some(expected))
                        })
                    }
                    "id" => metadata["Id"].as_str().unwrap_or("").starts_with(value),
                    "name" => metadata["Name"].as_str().unwrap_or("").contains(value),
                    "status" => metadata["State"]["Status"] == *value,
                    "exited" => {
                        metadata["State"]["Status"] == "exited"
                            && metadata["State"]["ExitCode"].as_i64() == value.parse::<i64>().ok()
                    }
                    "health" => {
                        metadata
                            .pointer("/State/Health/Status")
                            .and_then(Value::as_str)
                            .unwrap_or("none")
                            == value
                    }
                    "ancestor" => metadata["Image"].as_str().is_some_and(|id| {
                        id.starts_with(value)
                            || id
                                .strip_prefix("sha256:")
                                .is_some_and(|id| id.starts_with(value))
                    }),
                    "reference" => {
                        let pattern = globset::Glob::new(value)?.compile_matcher();
                        metadata["RepoTags"].as_array().is_some_and(|tags| {
                            tags.iter()
                                .any(|tag| tag.as_str().is_some_and(|tag| pattern.is_match(tag)))
                        })
                    }
                    "dangling" if image => value == "false",
                    _ => false,
                })
            };
            if key == "label" {
                for value in values {
                    if !matches(value)? {
                        return Ok(false);
                    }
                }
            } else {
                let mut found = false;
                for value in values {
                    found |= matches(value)?;
                }
                if !found {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

pub(super) fn validate(command: &DockerCommand) -> anyhow::Result<()> {
    match command {
        DockerCommand::Ps(args) => {
            Filters::parse(&args.filters, false)?;
            args.format.as_deref().map(Format::parse).transpose()?;
        }
        DockerCommand::Run(args) => {
            labels(&args.labels)?;
            args.stop_signal
                .as_deref()
                .map(pvisor_guest::api::GuestSignal::parse_signal)
                .transpose()?;
            ensure!(
                args.stop_timeout.is_none_or(|timeout| timeout <= 86_400),
                "stop timeout exceeds one day"
            );
            ensure!(
                args.restart == docker::RestartPolicy::No || !args.rm,
                "restart policies conflict with auto-removal"
            );
        }
        DockerCommand::Cp(args) => {
            let source = copy_endpoint(&args.source)?;
            let destination = copy_endpoint(&args.destination)?;
            ensure!(
                source.is_some() != destination.is_some(),
                "cp requires exactly one CONTAINER:PATH endpoint"
            );
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn inspect(command: DockerCommand) -> anyhow::Result<i32> {
    let (args, forced) = match command {
        DockerCommand::Inspect(args) => (args, None),
        DockerCommand::Container {
            command: ContainerCommand::Inspect(args),
        } => (args, Some("container")),
        DockerCommand::Image {
            command: ImageCommand::Inspect(args),
        } => (args, Some("image")),
        _ => unreachable!(),
    };
    if args.files || args.checkpoint.is_some() || !args.command.is_empty() {
        ensure!(
            forced != Some("image") && args.selectors.len() <= 1,
            "filesystem inspection requires one task Session"
        );
        return super::super::host_service::call(super::super::host::SessionCommand::Inspect(
            super::super::runtime::InspectArgs {
                selector: args.selectors.first().map(PathBuf::from),
                output_dir: args.output_dir,
                checkpoint: args.checkpoint,
                command: args.command,
            },
        ));
    }
    ensure!(
        forced.is_none() || args.kind.as_deref().is_none_or(|kind| Some(kind) == forced),
        "conflicting inspect resource types"
    );
    let kind = forced.or(args.kind.as_deref());
    ensure!(
        args.images_dir.is_none() || kind != Some("container"),
        "--images-dir selects images, not containers"
    );
    let format = args.format.as_deref().map(Format::parse).transpose()?;
    let catalog = args.images_dir.map(PreparedCatalog::new);
    let store = if catalog.is_none() {
        Some(Store::discover()?)
    } else {
        None
    };
    let mut rows = vec![];
    let status = batch(&args.selectors, |selector| {
        let row = if let Some(catalog) = &catalog {
            catalog.inspect(selector)?
        } else {
            let store = store.as_ref().context("missing image store")?;
            match kind {
                Some("image") => store.inspect_image(selector)?,
                Some("container") => store.inspect_container(selector)?,
                _ if store.find(selector)?.is_some() => store.inspect_container(selector)?,
                _ if Path::new(selector).is_absolute() || Path::new(selector).exists() => {
                    inspect_task(selector, &args.output_dir)?
                }
                _ if store.image_id(selector)?.is_some() => store.inspect_image(selector)?,
                _ => inspect_task(selector, &args.output_dir)?,
            }
        };
        if let Some(format) = &format {
            println!("{}", format.render(&row)?);
        } else {
            rows.push(row);
        }
        Ok(())
    })?;
    if format.is_none() {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    }
    Ok(status)
}

fn inspect_task(selector: &str, storage: &Path) -> anyhow::Result<Value> {
    use pvisor::session_service::{SessionSelection, SessionService, StatusRequest};
    let status = SessionService::lifecycle_status(
        &super::super::host::service_context(),
        StatusRequest {
            session: SessionSelection {
                selector: Some(selector.into()),
                storage: storage.into(),
            },
        },
    )?;
    Ok(json!({
        "Id":status.run.run_id,
        "State":{"Status":status.state.as_str(),"Running":status.live},
        "Pvisor":{"Kind":"task","ExecutionId":status.run.execution_id,"Stage":status.run.stage,
            "ExecSupported":status.exec_supported}
    }))
}

pub(super) fn kill(args: Kill) -> anyhow::Result<i32> {
    let signal = pvisor_guest::api::GuestSignal::parse_signal(args.signal.as_str())?;
    let store = Store::discover()?;
    batch(&args.selectors, |selector| {
        if let Some(instance) = store.find(selector)? {
            store.signal(&instance.id, &args.signal)?;
            if args.json {
                println!("{}", json!({"Id":instance.id,"Signal":signal}));
            } else {
                println!("{selector}");
            }
        } else {
            ensure!(
                [9, 15].contains(&signal),
                "native task profile does not support arbitrary process signals"
            );
            let code = super::super::host_service::call(super::super::host::SessionCommand::Kill(
                super::super::runtime::KillArgs {
                    selector: selector.into(),
                    output_dir: args.output_dir.clone(),
                    json: args.json,
                },
            ))?;
            ensure!(code == 0, "Session cancellation failed with code {code}");
        }
        Ok(())
    })
}

pub(super) fn containers(store: &Store, args: List) -> anyhow::Result<i32> {
    let mut filters = Filters::parse(&args.filters, false)?;
    filters.resolve_ancestors(store)?;
    let format = args.format.as_deref().map(Format::parse).transpose()?;
    if !args.quiet {
        if let Some(format) = &format {
            if let Some(header) = format.header() {
                println!("{header}");
            }
        } else {
            ps_header(&mut io::stdout(), false)?;
        }
    }
    for instance in store.instances(args.all)? {
        let metadata = if args.format.is_some() || !args.filters.is_empty() {
            store.inspect_container(&instance.id)?
        } else {
            Value::Null
        };
        if !filters.matches(&metadata, false)? {
            continue;
        }
        let ports = instance
            .config
            .ports
            .iter()
            .map(|p| format!("{}:{}->{}/tcp", p.address, p.host, p.guest))
            .collect::<Vec<_>>()
            .join(", ");
        if args.quiet {
            println!(
                "{}",
                if args.no_trunc {
                    &instance.id
                } else {
                    &instance.id[..12]
                }
            );
        } else if let Some(format) = &format {
            let row = json!({
                "ID": if args.no_trunc { &instance.id } else { &instance.id[..12] },
                "Image": instance.image.reference, "Command": instance.argv.join(" "),
                "State": instance.state, "Status": instance.state, "Names": instance.name,
                "Ports": ports, "Labels": instance.config.labels.iter().map(|(k,v)| format!("{k}={v}"))
                    .collect::<Vec<_>>().join(","),
                "Mounts": instance.config.mounts.iter().map(|m| m.target.to_string_lossy().into_owned())
                    .collect::<Vec<_>>().join(","),
                "Networks": instance.config.network_name.unwrap_or_default(),
                "CreatedAt": metadata["Created"]
            });
            println!("{}", format.render(&row)?);
        } else {
            ps_row(
                &mut io::stdout(),
                PsRow {
                    id: &instance.id,
                    image: &instance.image.reference,
                    command: &instance.argv,
                    state: &instance.state,
                    exit_code: instance.exit_code,
                    ports: &ports,
                    name: &instance.name,
                },
                false,
                args.no_trunc,
            )?;
        }
    }
    Ok(0)
}

pub(super) fn images(args: ListImages) -> anyhow::Result<i32> {
    let filters = Filters::parse(&args.filters, true)?;
    let format = args.format.as_deref().map(Format::parse).transpose()?;
    let images: Vec<(String, docker::Image)> = if let Some(directory) = args.images_dir {
        PreparedCatalog::new(directory).list()?
    } else {
        Store::discover()?
            .images()?
            .into_iter()
            .map(|image| (image.reference.clone(), image))
            .collect()
    };
    if !args.quiet {
        if let Some(format) = &format {
            if let Some(header) = format.header() {
                println!("{header}");
            }
        } else {
            println!("REPOSITORY\tTAG\tIMAGE ID");
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    for (reference, image) in images {
        let (repository, tag) = split_tag(&reference);
        if args
            .repository
            .as_ref()
            .is_some_and(|filter| filter != &reference && filter != repository)
        {
            continue;
        }
        let mut metadata = if args.format.is_some() || !args.filters.is_empty() {
            image.inspect()?
        } else {
            Value::Null
        };
        if !metadata.is_null() {
            // Reference filters select individual catalog tags, not every alias of an ID.
            metadata["RepoTags"] = json!([reference]);
        }
        if !filters.matches(&metadata, true)? {
            continue;
        }
        let id = if args.no_trunc {
            &image.id
        } else {
            image.id.get(7..19).context("invalid image ID")?
        };
        if args.quiet {
            if ids.insert(id.to_owned()) {
                println!("{id}");
            }
        } else if let Some(format) = &format {
            println!(
                "{}",
                format.render(&json!({
                    "ID": id,"Repository": repository,"Tag": tag,"Digest": image.digest,
                    "CreatedAt": metadata["Created"]
                }))?
            );
        } else {
            println!("{repository}\t{tag}\t{id}");
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formatting_is_data_only_and_rejects_unsupported_expressions() {
        let row = json!({"State":{"ExitCode":23},"Config":{"Labels":{"app.role":"worker"}}});
        for (format, expected) in [
            ("{{.State.ExitCode}}", "23"),
            ("{{index .Config.Labels \"app.role\"}}", "worker"),
            ("{{json .Config.Labels}}", "{\"app.role\":\"worker\"}"),
        ] {
            assert_eq!(
                Format::parse(format).unwrap().render(&row).unwrap(),
                expected
            );
        }
        for format in ["{{", "{{range .}}", "{{printf \"%s\" .}}", "}}"] {
            assert!(Format::parse(format).is_err(), "{format}");
        }
    }
    #[test]
    fn filters_validate_and_combine_labels_with_other_predicates() {
        let row = json!({"Name":"/worker","State":{"Status":"exited","ExitCode":23},
            "Config":{"Labels":{"a":"1","b":""}}});
        let filters = Filters::parse(
            &["label=a=1".into(), "label=b".into(), "exited=23".into()],
            false,
        )
        .unwrap();
        assert!(filters.matches(&row, false).unwrap());
        assert!(
            !Filters::parse(&["label=missing".into()], false)
                .unwrap()
                .matches(&row, false)
                .unwrap()
        );
        for filter in ["missing=x", "exited=no", "health=bogus"] {
            assert!(Filters::parse(&[filter.into()], false).is_err());
        }
    }
}
