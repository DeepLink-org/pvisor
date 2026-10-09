use crate::{
    ledger::Ledger,
    model::{Case, ReviewState},
    parse::{parse_document, parse_setup},
    seal,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
pub struct Preparation {
    pub name: String,
    pub text: String,
    pub digest: String,
    pub script: String,
}
pub struct Project {
    pub root: PathBuf,
    pub cases: Vec<Case>,
    pub preparation: BTreeMap<String, Preparation>,
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
    /// Load explicitly selected Markdown inputs. Overlapping files are read once.
    /// Preparation names and REVIEWED.toml are relative to their nearest common directory.
    pub fn load(inputs: &[impl AsRef<Path>]) -> Result<Self> {
        ensure!(
            !inputs.is_empty(),
            "specify at least one Markdown file or directory"
        );
        let mut root = None;
        let mut files = BTreeSet::new();
        for input in inputs {
            let input = input
                .as_ref()
                .canonicalize()
                .context("find Markdown specifications")?;
            let directory = if input.is_dir() {
                files.extend(markdown_files(&input)?);
                input
            } else {
                ensure!(
                    input.is_file() && input.extension().is_some_and(|s| s == "md"),
                    "expected Markdown file or directory"
                );
                files.insert(input.clone());
                input.parent().unwrap().to_path_buf()
            };
            let root = root.get_or_insert_with(|| directory.clone());
            while !directory.starts_with(&*root) {
                ensure!(root.pop(), "inputs must share a filesystem root");
            }
        }
        let root = root.unwrap();
        let mut cases = vec![];
        let mut preparation = BTreeMap::new();
        let mut ids = BTreeSet::new();
        for path in files {
            let source = fs::read_to_string(&path)?;
            let relative = path.strip_prefix(&root)?;
            let mut parsed = parse_document(relative, &source)?;
            let mut names = vec![];
            let index = path.parent().unwrap().join("index.md");
            let setup_paths = if index == path {
                vec![path.clone()]
            } else {
                vec![index, path.clone()]
            };
            for setup_path in setup_paths {
                if !setup_path.is_file() {
                    continue;
                }
                let text = seal::normalize(&fs::read_to_string(&setup_path)?);
                let script = parse_setup(&text)?;
                if script.is_empty() {
                    continue;
                }
                let name = setup_path
                    .strip_prefix(&root)?
                    .to_str()
                    .context("Markdown path must be UTF8")?
                    .to_owned();
                names.push(name.clone());
                preparation
                    .entry(name.clone())
                    .or_insert_with(|| Preparation {
                        digest: seal::vocab_digest(&name, &text),
                        name,
                        text,
                        script,
                    });
            }
            for case in &mut parsed {
                ensure!(ids.insert(case.id.clone()), "duplicate case {}", case.id);
                for platform in &case.annotation.xfail_on {
                    ensure!(
                        ["all", "linux", "macos"].contains(&platform.as_str()),
                        "{}: unknown platform {platform}",
                        case.id
                    );
                }
                case.preparation = names.clone();
            }
            cases.extend(parsed);
        }
        ensure!(!cases.is_empty(), "no semantic cases found");
        cases.sort_by(|a, b| a.id.cmp(&b.id));
        let ledger_path = root.join("REVIEWED.toml");
        let ledger = if ledger_path.exists() {
            Ledger::parse(&fs::read_to_string(ledger_path)?)?
        } else {
            Ledger::default()
        };
        Ok(Self {
            root,
            cases,
            preparation,
            ledger,
        })
    }
    /// Select a nonempty inventory. Explicit IDs must be unique and belong to the selected domain.
    pub fn select(&self, ids: &[String], domain: Option<&str>) -> Result<Vec<&Case>> {
        ensure!(
            ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
            "selected case IDs must be unique"
        );
        if let Some(domain) = domain {
            ensure!(
                self.cases.iter().any(|case| case.domain == domain),
                "unknown domain {domain}"
            );
        }
        for id in ids {
            ensure!(
                self.cases
                    .iter()
                    .any(|case| case.id == *id && domain.is_none_or(|d| case.domain == d)),
                "unknown case or excluded by domain: {id}"
            );
        }
        let cases: Vec<_> = self
            .cases
            .iter()
            .filter(|case| {
                (ids.is_empty() || ids.contains(&case.id))
                    && domain.is_none_or(|d| case.domain == d)
            })
            .collect();
        ensure!(!cases.is_empty(), "selection contains no cases");
        Ok(cases)
    }
    pub fn preparation_names(&self, case: &Case) -> Vec<String> {
        case.preparation.clone()
    }
    pub fn case_digest(&self, case: &Case) -> String {
        seal::case_digest(
            &case.text,
            &self
                .preparation_names(case)
                .iter()
                .map(|n| self.preparation[n].digest.clone())
                .collect::<Vec<_>>(),
        )
    }
    pub fn item(&self, id: &str) -> Result<Item> {
        let (digest, text) = if id == "@engine" {
            (seal::engine_digest(), seal::normalize(seal::ENGINE_TEXT))
        } else if let Some(name) = id.strip_prefix("@vocab:") {
            let vocab = self
                .preparation
                .get(name)
                .context("unknown preparation document")?;
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
            for name in self.preparation_names(case) {
                text.push_str(&format!(
                    "Preparation {name}: {}\n",
                    self.preparation[&name].digest
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
            .chain(self.preparation.keys().map(|n| format!("@vocab:{n}")))
            .chain(self.cases.iter().map(|c| c.id.clone()))
            .collect()
    }
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
