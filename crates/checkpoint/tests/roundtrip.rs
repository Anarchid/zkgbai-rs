use checkpoint::{serde, state};
use std::collections::{BTreeMap, HashMap};

#[state]
struct Example {
    count: u64,
    history: Vec<(i32, f32)>,
    names: BTreeMap<&'static str, (i32, &'static str)>,
    #[checkpoint(
        rebuild = "String::from(\"new runtime\")",
        reason = "Demonstrates explicitly rebuilt process resource"
    )]
    runtime: String,
}
#[test]
fn owned_fields_static_identifiers_and_nonfinite_values_survive() {
    let data = Example {
        count: 77,
        history: vec![(4, f32::INFINITY)],
        names: BTreeMap::from([("a", (1, "b"))]),
        runtime: "old".into(),
    };
    let bytes = checkpoint::encode(&data).unwrap();
    let decoded: Example =
        checkpoint::with_literals(&["a", "b"], || checkpoint::rmp_serde::from_slice(&bytes)).unwrap();
    assert_eq!(decoded.count, 77);
    assert_eq!(decoded.history, data.history);
    assert_eq!(decoded.names, data.names);
    assert_eq!(decoded.runtime, "new runtime");
    assert_eq!(checkpoint::encode(&decoded).unwrap(), bytes);
    assert!(checkpoint::with_literals(&["a"], || checkpoint::rmp_serde::from_slice::<Example>(&bytes)).is_err());
}
#[test]
fn maps_have_canonical_order() {
    let a = HashMap::from([((1, 2), 5), ((2, 3), 6)]);
    let b = HashMap::from([((2, 3), 6), ((1, 2), 5)]);
    assert_eq!(checkpoint::encode(&a).unwrap(), checkpoint::encode(&b).unwrap());
}
#[test]
fn pending_results_remain_available_to_both_branches() {
    let (tx, rx) = std::sync::mpsc::channel();
    let pending = checkpoint::Pending::from(rx);
    std::thread::spawn(move || tx.send(123u32).unwrap());
    let bytes = checkpoint::encode(&pending).unwrap();
    let restored: checkpoint::Pending<u32> = checkpoint::rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(pending.try_recv().unwrap(), 123);
    assert_eq!(restored.try_recv().unwrap(), 123);
}
#[state]
struct Changed {
    count: u64,
    additional: bool,
}
#[state]
struct OptionalChanged {
    count: u64,
    additional: Option<bool>,
}
#[test]
fn absent_new_fields_are_rejected() {
    #[derive(serde::Serialize)]
    #[serde(crate = "serde")]
    struct Before {
        count: u64,
    }
    let bytes = checkpoint::encode(&Before { count: 1 }).unwrap();
    assert!(checkpoint::rmp_serde::from_slice::<Changed>(&bytes).is_err());
    assert!(checkpoint::rmp_serde::from_slice::<OptionalChanged>(&bytes).is_err());
}

#[state]
struct Lazy {
    #[serde(with = "checkpoint::once_cell")]
    cached: std::cell::OnceCell<u64>,
}
#[test]
fn lazy_cache_preserves_initialized_and_uninitialized_states() {
    for value in [None, Some(123)] {
        let original = Lazy { cached: std::cell::OnceCell::new() };
        if let Some(v) = value {
            original.cached.set(v).unwrap();
        }
        let bytes = checkpoint::encode(&original).unwrap();
        let restored: Lazy = checkpoint::rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(restored.cached.get().copied(), value);
    }
}
