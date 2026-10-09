use sha2::{Digest, Sha256};
pub const ENGINE_SEMANTICS: &str = "6";
pub const ENGINE_TEXT: &str = include_str!("../ENGINE.md");
pub fn normalize(text: &str) -> String {
    let mut lines: Vec<_> = text.lines().map(str::trim_end).collect();
    while lines.last() == Some(&"") {
        lines.pop();
    }
    format!("{}\n", lines.join("\n"))
}
pub fn digest(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
    }
    format!("sha256:{}", hex(&hash.finalize()))
}
pub fn engine_digest() -> String {
    digest(&[b"semspec/engine/v1\0", ENGINE_SEMANTICS.as_bytes()])
}
pub fn vocab_digest(name: &str, text: &str) -> String {
    digest(&[
        b"semspec/vocab/v1\0",
        name.as_bytes(),
        b"\0",
        normalize(text).as_bytes(),
    ])
}
pub fn case_digest(text: &str, vocab: &[String]) -> String {
    let mut vocab = vocab.to_vec();
    vocab.sort();
    digest(&[
        b"semspec/case/v1\0",
        normalize(text).as_bytes(),
        b"\0",
        vocab.join("\n").as_bytes(),
        b"\0",
        ENGINE_SEMANTICS.as_bytes(),
    ])
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
