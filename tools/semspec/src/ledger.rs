use crate::model::{ReviewState, valid_case_id, valid_vocab_name};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub item: String,
    pub digest: String,
    pub reviewer: String,
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revocation {
    pub item: String,
    pub reviewer: String,
    pub date: String,
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub format: u32,
    #[serde(default)]
    pub approval: Vec<Approval>,
    #[serde(default)]
    pub revocation: Vec<Revocation>,
}
impl Default for Ledger {
    fn default() -> Self {
        Self {
            format: 1,
            approval: vec![],
            revocation: vec![],
        }
    }
}
fn valid_item(item: &str) -> bool {
    item == "@engine"
        || valid_case_id(item)
        || item.strip_prefix("@vocab:").is_some_and(valid_vocab_name)
}
impl Ledger {
    pub fn parse(text: &str) -> Result<Self> {
        let ledger: Self = toml::from_str(text)?;
        ensure!(ledger.format == 1, "unsupported ledger format");
        let mut seen = BTreeSet::new();
        for a in &ledger.approval {
            ensure!(
                valid_item(&a.item) && seen.insert(&a.item),
                "invalid/duplicate approval {}",
                a.item
            );
            let hash = a.digest.strip_prefix("sha256:").unwrap_or("");
            ensure!(
                hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
                "invalid digest"
            );
            ensure!(!a.reviewer.trim().is_empty(), "empty reviewer");
            chrono::NaiveDate::parse_from_str(&a.date, "%Y-%m-%d")?;
            ensure!(
                a.signature.is_none(),
                "SSH signature verification requires semspec v0.2"
            );
        }
        for r in &ledger.revocation {
            ensure!(
                valid_item(&r.item) && !r.reviewer.trim().is_empty() && !r.reason.trim().is_empty(),
                "invalid revocation"
            );
            chrono::NaiveDate::parse_from_str(&r.date, "%Y-%m-%d")?;
        }
        Ok(ledger)
    }
    pub fn state(&self, item: &str, current: &str) -> ReviewState {
        if let Some(a) = self.approval.iter().find(|a| a.item == item) {
            return if a.digest == current {
                ReviewState::Reviewed {
                    by: a.reviewer.clone(),
                    on: a.date.clone(),
                }
            } else {
                ReviewState::Stale {
                    approved: a.digest.clone(),
                }
            };
        }
        self.revocation
            .iter()
            .rev()
            .find(|r| r.item == item)
            .map(|r| ReviewState::Revoked {
                reason: r.reason.clone(),
            })
            .unwrap_or(ReviewState::Unreviewed)
    }
    pub fn approve(&mut self, approval: Approval) {
        self.approval.retain(|a| a.item != approval.item);
        self.approval.push(approval);
    }
    pub fn revoke(&mut self, revocation: Revocation) {
        self.approval.retain(|a| a.item != revocation.item);
        self.revocation.push(revocation);
    }
}
