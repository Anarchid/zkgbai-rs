//! Portable snapshots with explicit runtime exclusions and no saved pointers.
pub use checkpoint_derive::state;
pub use rmp_serde;
pub use serde;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

thread_local! { static LITERALS: Cell<&'static [&'static str]> = const { Cell::new(&[]) }; }

/// Resolve static identifiers to this binary's literals, never leak save-file strings.
/// The caller's generated catalogue is sorted and contains source string literals.
pub fn with_literals<T>(literals: &'static [&'static str], f: impl FnOnce() -> T) -> T {
    struct Reset(&'static [&'static str]);
    impl Drop for Reset {
        fn drop(&mut self) {
            LITERALS.set(self.0);
        }
    }
    let _reset = Reset(LITERALS.replace(literals));
    f()
}

pub trait StaticField: Sized {
    type Owned: serde::de::DeserializeOwned;
    fn resolve(value: Self::Owned) -> Result<Self, String>;
}
impl StaticField for &'static str {
    type Owned = String;
    fn resolve(value: String) -> Result<Self, String> {
        LITERALS.with(|l| {
            let literals = l.get();
            literals
                .binary_search(&value.as_str())
                .map(|i| literals[i])
                .map_err(|_| format!("checkpoint identifier absent from this build: {value:?}"))
        })
    }
}
macro_rules! identity { ($($t:ty),*) => { $(impl StaticField for $t { type Owned=Self; fn resolve(v:Self)->Result<Self,String>{Ok(v)} })* }; }
identity!(bool, char, u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f32, f64, String);
impl<T: StaticField> StaticField for Option<T> {
    type Owned = Option<T::Owned>;
    fn resolve(v: Self::Owned) -> Result<Self, String> {
        v.map(T::resolve).transpose()
    }
}
impl<T: StaticField> StaticField for Vec<T> {
    type Owned = Vec<T::Owned>;
    fn resolve(v: Self::Owned) -> Result<Self, String> {
        v.into_iter().map(T::resolve).collect()
    }
}
impl<T: StaticField> StaticField for VecDeque<T> {
    type Owned = Vec<T::Owned>;
    fn resolve(v: Self::Owned) -> Result<Self, String> {
        v.into_iter().map(T::resolve).collect()
    }
}
impl<T: StaticField + Ord> StaticField for BTreeSet<T> {
    type Owned = Vec<T::Owned>;
    fn resolve(v: Self::Owned) -> Result<Self, String> {
        v.into_iter().map(T::resolve).collect()
    }
}
impl<K: StaticField + Ord, V: StaticField> StaticField for BTreeMap<K, V>
where
    K::Owned: Ord,
{
    type Owned = BTreeMap<K::Owned, V::Owned>;
    fn resolve(v: Self::Owned) -> Result<Self, String> {
        v.into_iter().map(|(k, v)| Ok((K::resolve(k)?, V::resolve(v)?))).collect()
    }
}
macro_rules! tuple { ($($n:ident),*) => { impl<$($n:StaticField),*> StaticField for ($($n,)*) { type Owned=($($n::Owned,)*); #[allow(non_snake_case)] fn resolve(value:Self::Owned)->Result<Self,String>{let ($($n,)*)=value; Ok(($($n::resolve($n)?,)*))} } }; }
tuple!(A, B);
tuple!(A, B, C);
tuple!(A, B, C, D);
impl<T: StaticField, const N: usize> StaticField for [T; N]
where
    [T::Owned; N]: serde::de::DeserializeOwned,
{
    type Owned = [T::Owned; N];
    fn resolve(value: Self::Owned) -> Result<Self, String> {
        value.into_iter().map(T::resolve).collect::<Result<Vec<_>, _>>()?.try_into().map_err(|_| "array length".into())
    }
}

/// One pure calculation. Saving waits for it and buffers the answer in BOTH
/// branches; loading never restarts a calculation against a newer world.
pub struct Pending<T> {
    rx: Option<std::sync::mpsc::Receiver<T>>,
    ready: std::cell::RefCell<Option<Result<T, String>>>,
}
impl<T> From<std::sync::mpsc::Receiver<T>> for Pending<T> {
    fn from(rx: std::sync::mpsc::Receiver<T>) -> Self {
        Self { rx: Some(rx), ready: Default::default() }
    }
}
thread_local! { static DETERMINISTIC: Cell<bool> = const { Cell::new(false) }; }
pub fn deterministic() -> bool {
    DETERMINISTIC.get()
}
pub fn with_determinism<T>(value: bool, f: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            DETERMINISTIC.set(self.0);
        }
    }
    let _reset = Reset(DETERMINISTIC.replace(value));
    f()
}
impl<T> Pending<T> {
    pub fn try_recv(&self) -> Result<T, std::sync::mpsc::TryRecvError> {
        use std::sync::mpsc::TryRecvError;
        if let Some(v) = self.ready.borrow_mut().take() {
            return v.map_err(|_| TryRecvError::Disconnected);
        }
        let rx = self.rx.as_ref().ok_or(TryRecvError::Disconnected)?;
        if deterministic() { rx.recv().map_err(|_| TryRecvError::Disconnected) } else { rx.try_recv() }
    }
    fn freeze(&self) -> Result<(), String> {
        if self.ready.borrow().is_none() {
            let v = self
                .rx
                .as_ref()
                .ok_or("pending computation already consumed")?
                .recv_timeout(std::time::Duration::from_secs(60))
                .map_err(|e| e.to_string());
            if matches!(&v,Err(e) if e.contains("timed out")) {
                return Err("checkpoint computation timed out".into());
            }
            *self.ready.borrow_mut() = Some(v);
        }
        Ok(())
    }
}
impl<T: serde::Serialize> serde::Serialize for Pending<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.freeze().map_err(serde::ser::Error::custom)?;
        self.ready.borrow().as_ref().unwrap().serialize(s)
    }
}
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Pending<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self { rx: None, ready: std::cell::RefCell::new(Some(<Result<T, String>>::deserialize(d)?)) })
    }
}

/// Canonical ordering includes HashMap keys and preserves non-finite floats.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let tree = serde_value::to_value(value).map_err(|e| e.to_string())?;
    rmp_serde::to_vec_named(&tree).map_err(|e| e.to_string())
}
pub fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

/// Explicit deserializer prevents serde from silently defaulting a missing Option.
pub fn required<'de, T: serde::Deserialize<'de>, D: serde::Deserializer<'de>>(d: D) -> Result<T, D::Error> {
    T::deserialize(d)
}

/// Preserve both the uninitialized and initialized state of a lazy cache.
pub mod once_cell {
    use serde::{Deserialize, Serialize};
    use std::cell::OnceCell;
    pub fn serialize<T: Serialize, S: serde::Serializer>(value: &OnceCell<T>, s: S) -> Result<S::Ok, S::Error> {
        value.get().serialize(s)
    }
    pub fn deserialize<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(d: D) -> Result<OnceCell<T>, D::Error> {
        let cell = OnceCell::new();
        if let Some(value) = Option::<T>::deserialize(d)? {
            let _ = cell.set(value);
        }
        Ok(cell)
    }
}
