//! Fixed error contracts and generated namespace state transitions.
#[path = "namespace_recovery/model.rs"]
mod model;

use model::{ErrorComparison, JournalMode, Operation, run_trace};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

#[test]
fn namespace_type_errors_retries_and_reopens_match_native_without_changing_lower() {
    use Operation::*;
    let operations = [
        Rename("dir", "occupied"),
        Remove("occupied", true),
        Rename("a", "occupied"),
        Rename("dir", "a"),
        Create("a"),
        Mkdir("dir"),
        Remove("dir", false),
        Remove("a", true),
        Rename("missing", "missing"),
        Remove("missing", false),
        Rename("empty", "occupied"),
        Rename("dir", "empty"),
        Remove("empty", true),
        Rename("empty/inside", "a"),
        Mkdir("empty/inside"),
        Create("empty/inside/new"),
        Rename("empty", "occupied"),
        Remove("occupied/item", false),
        Rename("empty", "occupied"),
        Remove("occupied/inside", true),
        Remove("occupied/inside/new", false),
        Remove("occupied/inside", true),
        Remove("occupied", true),
        Create("occupied"),
        Rename("occupied", "final"),
        Rename("final", "final"),
        Remove("final", false),
        Create("final"),
    ];
    for mode in JournalMode::ALL {
        assert_eq!(run_trace(mode, &operations, ErrorComparison::Exact), 14);
    }
}

fn operation() -> impl Strategy<Value = Operation> {
    let path = prop::sample::select(vec![
        "a",
        "dir",
        "dir/inside",
        "occupied",
        "occupied/item",
        "empty",
        "missing",
        "missing/child",
        "final",
        "界",
        "empty/界",
    ]);
    prop_oneof![
        (path.clone(), path.clone()).prop_map(|(old, new)| Operation::Rename(old, new)),
        (path.clone(), any::<bool>())
            .prop_map(|(path, directory)| Operation::Remove(path, directory)),
        path.clone().prop_map(Operation::Create),
        path.clone().prop_map(Operation::Mkdir),
        (path, prop::collection::vec(any::<u8>(), 0..257))
            .prop_map(|(path, bytes)| Operation::Write(path, bytes)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource("proptest-regressions"))),
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_namespace_sequences_match_native(
        operations in prop::collection::vec(operation(), 1..33),
    ) {
        for mode in JournalMode::ALL {
            run_trace(mode, &operations, ErrorComparison::Outcome);
        }
    }

    #[test]
    fn successful_recreation_cycles_preserve_bytes_and_original_baselines(
        first in prop::collection::vec(any::<u8>(), 0..1025),
        second in prop::collection::vec(any::<u8>(), 0..1025),
    ) {
        use Operation::*;
        let operations = [
            Create("fresh"),
            Write("fresh", first.clone()),
            Rename("fresh", "dir/created"),
            Write("dir/created", second.clone()),
            Remove("dir/created", false),
            Mkdir("fresh"),
            Create("fresh/child"),
            Write("fresh/child", first),
            Rename("fresh", "empty/recreated"),
            Remove("empty/recreated/child", false),
            Remove("empty/recreated", true),
            Remove("a", false),
            Create("a"),
            Write("a", second),
        ];
        for mode in JournalMode::ALL {
            prop_assert_eq!(run_trace(mode, &operations, ErrorComparison::Exact), 0);
        }
    }
}
