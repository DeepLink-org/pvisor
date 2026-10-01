use crate::{
    config::Config,
    ledger::Ledger,
    model::{Case, ReviewState, valid_vocab_name},
    parse::parse_spec,
    seal,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
pub struct Vocab {
    pub name: String,
    pub text: String,
    pub digest: String,
}
pub struct Project {
    pub root: PathBuf,
    pub config: Config,
    pub cases: Vec<Case>,
    pub vocab: BTreeMap<String, Vocab>,
    pub ledger: Ledger,
}
pub struct Item {
    pub id: String,
    pub digest: String,
    pub text: String,
    pub review: ReviewState,
}
pub fn markdown_files(path: &Path) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() && !entry.file_name().as_encoded_bytes().starts_with(b".") {
            out.extend(markdown_files(&entry.path())?);
        } else if kind.is_file()
            && entry.path().extension().is_some_and(|s| s == "md")
            && entry.file_name() != "README.md"
        {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}
impl Project {
    pub fn load(config_file: &Path) -> Result<Self> {
        let config_file = config_file.canonicalize().context("find semspec.toml")?;
        let root = config_file.parent().unwrap().to_path_buf();
        let config = Config::parse(&fs::read_to_string(&config_file)?)?;
        let mut cases = vec![];
        let mut files = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for dir in &config.project.spec_dirs {
            for path in markdown_files(&root.join(dir))? {
                ensure!(
                    files.insert(path.canonicalize()?),
                    "overlapping spec directories"
                );
                cases.extend(parse_spec(
                    path.strip_prefix(&root)?,
                    &fs::read_to_string(&path)?,
                )?);
            }
        }
        ensure!(!cases.is_empty(), "no semantic cases found");
        for case in &cases {
            ensure!(ids.insert(case.id.clone()), "duplicate case {}", case.id);
            ensure!(
                !config.project.retired.contains(&case.id),
                "retired case ID reused: {}",
                case.id
            );
            for name in &case.annotation.requires {
                ensure!(
                    config.requirements.contains_key(name),
                    "{}: unknown requirement {name}",
                    case.id
                );
            }
            for platform in &case.annotation.xfail_on {
                ensure!(
                    platform == "all" || config.platforms.contains_key(platform),
                    "{}: unknown platform {platform}",
                    case.id
                );
            }
        }
        cases.sort_by(|a, b| a.id.cmp(&b.id));
        let mut vocab = BTreeMap::new();
        for path in &config.subject.vocab {
            load_vocab(&root.join(path), &mut vocab)?;
        }
        for case in &cases {
            if let Some(names) = &case.annotation.vocab {
                ensure!(
                    names.iter().collect::<BTreeSet<_>>().len() == names.len(),
                    "{}: duplicate vocabulary",
                    case.id
                );
                for name in names {
                    ensure!(valid_vocab_name(name), "invalid vocab name {name}");
                    if vocab.contains_key(name) {
                        continue;
                    }
                    let found: Vec<_> = config
                        .project
                        .spec_dirs
                        .iter()
                        .map(|d| root.join(d).join("vocab").join(name))
                        .filter(|p| p.is_file())
                        .collect();
                    ensure!(
                        found.len() == 1,
                        "vocabulary {name} must resolve to one file"
                    );
                    load_vocab(&found[0], &mut vocab)?;
                }
            }
        }
        let ledger_path = root.join(&config.project.ledger);
        let ledger = if ledger_path.exists() {
            Ledger::parse(&fs::read_to_string(ledger_path)?)?
        } else {
            Ledger::default()
        };
        for approval in &ledger.approval {
            if crate::model::valid_case_id(&approval.item) && !ids.contains(&approval.item) {
                ensure!(
                    config.project.retired.contains(&approval.item),
                    "deleted approved case must be retired: {}",
                    approval.item
                );
            }
        }
        Ok(Self {
            root,
            config,
            cases,
            vocab,
            ledger,
        })
    }
    pub fn vocab_names(&self, case: &Case) -> Vec<String> {
        case.annotation.vocab.clone().unwrap_or_else(|| {
            self.config
                .subject
                .vocab
                .iter()
                .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
                .collect()
        })
    }
    pub fn case_digest(&self, case: &Case) -> String {
        seal::case_digest(
            &case.text,
            &self
                .vocab_names(case)
                .iter()
                .map(|n| self.vocab[n].digest.clone())
                .collect::<Vec<_>>(),
        )
    }
    pub fn item(&self, id: &str) -> Result<Item> {
        let (digest, text) = if id == "@engine" {
            (seal::engine_digest(), seal::normalize(seal::ENGINE_TEXT))
        } else if let Some(name) = id.strip_prefix("@vocab:") {
            let vocab = self.vocab.get(name).context("unknown vocabulary")?;
            (vocab.digest.clone(), vocab.text.clone())
        } else {
            let case = self
                .cases
                .iter()
                .find(|c| c.id == id)
                .context("unknown semantic case")?;
            let mut text = case.text.clone();
            text.push_str(&format!(
                "\n---\nEngine semantics: {}\n",
                seal::ENGINE_SEMANTICS
            ));
            for name in self.vocab_names(case) {
                text.push_str(&format!(
                    "Vocabulary {name}: {}\n",
                    self.vocab[&name].digest
                ));
            }
            (self.case_digest(case), text)
        };
        Ok(Item {
            id: id.into(),
            review: self.ledger.state(id, &digest),
            digest,
            text,
        })
    }
    pub fn items(&self) -> Vec<String> {
        std::iter::once("@engine".into())
            .chain(self.vocab.keys().map(|n| format!("@vocab:{n}")))
            .chain(self.cases.iter().map(|c| c.id.clone()))
            .collect()
    }
    pub fn snapshot_path(&self, item: &str) -> Result<PathBuf> {
        self.item(item)?;
        Ok(self
            .root
            .join(&self.config.project.approved_snapshots)
            .join(format!("{item}.md")))
    }
}
fn load_vocab(path: &Path, vocab: &mut BTreeMap<String, Vocab>) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("vocab filename must be UTF8")?
        .to_owned();
    ensure!(valid_vocab_name(&name), "invalid vocabulary filename");
    ensure!(
        !vocab.contains_key(&name),
        "duplicate vocabulary filename {name}"
    );
    let text = seal::normalize(
        &fs::read_to_string(path).with_context(|| format!("read vocabulary {}", path.display()))?,
    );
    vocab.insert(
        name.clone(),
        Vocab {
            digest: seal::vocab_digest(&name, &text),
            name,
            text,
        },
    );
    Ok(())
}
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("file requires a parent directory")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
