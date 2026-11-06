//! Engine handles persist by identity; callbacks bind only during explicit restore.
use crate::Callback;
use checkpoint::serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use std::cell::Cell;
thread_local! { static CALLBACK: Cell<Option<Callback>> = const { Cell::new(None) }; }
pub fn with_callback<T>(cb: Callback, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<Callback>);
    impl Drop for Reset {
        fn drop(&mut self) {
            CALLBACK.set(self.0);
        }
    }
    let _reset = Reset(CALLBACK.replace(Some(cb)));
    f()
}
pub fn callback() -> Callback {
    CALLBACK.get().expect("checkpoint callback outside restore")
}
impl Serialize for Callback {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit()
    }
}
impl<'de> Deserialize<'de> for Callback {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        <()>::deserialize(d)?;
        CALLBACK.get().ok_or_else(|| D::Error::custom("engine handle decoded outside restore context"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn engine_handles_rebind_to_current_instance_only_in_restore_context() {
        let raw: springai_sys::SSkirmishAICallback = unsafe { std::mem::zeroed() };
        let before = unsafe { Callback::from_raw(17, &raw) };
        let after = unsafe { Callback::from_raw(42, &raw) };
        let unit = crate::Unit::from_id(before, 123).unwrap();
        let bytes = checkpoint::encode(&unit).unwrap();
        assert!(checkpoint::rmp_serde::from_slice::<crate::Unit>(&bytes).is_err());
        let restored: crate::Unit = with_callback(after, || checkpoint::rmp_serde::from_slice(&bytes)).unwrap();
        assert_eq!(restored.id(), 123);
        assert_eq!(restored.callback().ai_id(), 42);
        assert!(checkpoint::rmp_serde::from_slice::<crate::Unit>(&bytes).is_err());
    }
}
