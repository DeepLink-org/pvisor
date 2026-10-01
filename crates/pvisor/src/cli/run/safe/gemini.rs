pub(super) fn patch() -> Vec<String> {
    vec![
        "--overlaynet-allow".into(),
        "generativelanguage.googleapis.com:443".into(),
    ]
}
