use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct Case {
    pub id: String,
    pub title: String,
    pub domain: String,
    pub file: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub claim: String,
    pub violation: String,
    pub rationale: Option<String>,
    pub script: String,
    pub annotation: Annotation,
    pub preparation: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Annotation {
    pub xfail_on: BTreeSet<String>,
    pub xfail_reason: Option<String>,
    pub timeout: Option<String>,
}
pub fn valid_case_id(id: &str) -> bool {
    let parts: Vec<_> = id.split('-').collect();
    parts.len() == 3
        && parts[0] == "S"
        && !parts[1].is_empty()
        && parts[1].bytes().all(|c| c.is_ascii_uppercase())
        && parts[2].len() == 3
        && parts[2].bytes().all(|c| c.is_ascii_digit())
}
pub fn valid_vocab_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && std::path::Path::new(name)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewState {
    Reviewed { by: String, on: String },
    Unreviewed,
    Stale { approved: String },
}
impl ReviewState {
    pub fn reviewed(&self) -> bool {
        matches!(self, Self::Reviewed { .. })
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Reviewed { .. } => "REVIEWED",
            Self::Unreviewed => "UNREVIEWED",
            Self::Stale { .. } => "STALE",
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "verdict", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    Pass,
    Fail {
        output_tail: String,
    },
    Skip {
        reason: String,
    },
    #[serde(rename = "XFAIL")]
    XFail {
        reason: String,
        output_tail: String,
    },
    #[serde(rename = "XPASS")]
    XPass,
    Error {
        message: String,
    },
}
impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail { .. } => "FAIL",
            Self::Skip { .. } => "SKIP",
            Self::XFail { .. } => "XFAIL",
            Self::XPass => "XPASS",
            Self::Error { .. } => "ERROR",
        }
    }
    pub fn retain(&self) -> bool {
        matches!(
            self,
            Self::Fail { .. } | Self::XFail { .. } | Self::XPass | Self::Error { .. }
        )
    }
}
#[derive(Debug, Serialize)]
pub struct CaseResult {
    pub id: String,
    pub verdict: Verdict,
    pub review: ReviewState,
    pub duration_ms: u128,
    pub workdir: Option<PathBuf>,
}
#[derive(Debug, Serialize)]
pub struct RunReport {
    pub engine_semantics: &'static str,
    pub engine_review: ReviewState,
    pub vocab_review: Vec<(String, ReviewState)>,
    pub platform: String,
    pub results: Vec<CaseResult>,
}
impl RunReport {
    /// Require a complete, unique report inventory; ERROR keeps precedence over gate failures.
    /// Strict PASS and human review are independent requirements and never modify approvals.
    pub fn exit_code(&self, expected: &[&str], require_reviewed: bool, require_pass: bool) -> i32 {
        if self
            .results
            .iter()
            .any(|r| matches!(r.verdict, Verdict::Error { .. }))
        {
            return 3;
        }
        let actual: BTreeSet<_> = self.results.iter().map(|row| row.id.as_str()).collect();
        if actual.is_empty()
            || actual.len() != self.results.len()
            || actual.len() != expected.len()
            || actual != expected.iter().copied().collect()
            || (require_pass
                && self
                    .results
                    .iter()
                    .any(|row| !matches!(row.verdict, Verdict::Pass)))
        {
            return 1;
        }
        if self
            .results
            .iter()
            .any(|r| matches!(r.verdict, Verdict::Fail { .. } | Verdict::XPass))
            || (require_reviewed
                && (!self.engine_review.reviewed()
                    || self.vocab_review.iter().any(|(_, r)| !r.reviewed())
                    || self.results.iter().any(|r| !r.review.reviewed())))
        {
            return 1;
        }
        0
    }
}
