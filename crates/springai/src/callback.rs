use std::ffi::{CStr, CString, c_char};

use springai_sys::SSkirmishAICallback;

/// Handle to the engine callback table for one Skirmish AI instance.
///
/// This is a cheap `Copy` value: every object handle in this crate ([`Unit`],
/// [`UnitDef`], ...) embeds one so that it can query the engine on its own,
/// mirroring the engine's C++ OO wrapper.
///
/// [`Unit`]: crate::Unit
/// [`UnitDef`]: crate::UnitDef
#[derive(Clone, Copy)]
pub struct Callback {
    raw: *const SSkirmishAICallback,
    ai_id: i32,
}

// SAFETY: the callback table is owned by the engine and stays valid from
// `init()` until `release()` of this AI instance. The engine only ever calls
// into an AI from its simulation thread, so the handle is never used
// concurrently; `Send`/`Sync` are needed only to park it in statics.
unsafe impl Send for Callback {}
unsafe impl Sync for Callback {}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Callback(ai={})", self.ai_id)
    }
}

impl Callback {
    /// # Safety
    /// `raw` must point to the engine-provided callback table for `ai_id`
    /// and remain valid for as long as this handle (or any object handle
    /// derived from it) is used.
    pub unsafe fn from_raw(ai_id: i32, raw: *const SSkirmishAICallback) -> Self {
        assert!(!raw.is_null(), "null SSkirmishAICallback");
        Self { raw, ai_id }
    }

    /// The engine's id for this AI instance.
    #[inline]
    pub fn ai_id(&self) -> i32 {
        self.ai_id
    }

    #[inline]
    pub(crate) fn table(&self) -> &SSkirmishAICallback {
        // SAFETY: guaranteed by `from_raw`'s contract.
        unsafe { &*self.raw }
    }
}

/// Invoke a callback-table function, passing the AI id as first argument.
macro_rules! ffi {
    ($cb:expr, $f:ident $(, $arg:expr)* $(,)?) => {{
        let cb: &$crate::Callback = &$cb;
        let func = cb.table().$f.expect(concat!("engine callback ", stringify!($f), " is missing"));
        #[allow(unused_unsafe)]
        unsafe { func(cb.ai_id() $(, $arg)*) }
    }};
}
pub(crate) use ffi;

/// Convert an engine-owned C string into an owned `String`.
pub(crate) fn owned_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        // SAFETY: the engine hands out NUL-terminated strings.
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }
}

/// Build a C string for passing to the engine; interior NULs are stripped.
pub(crate) fn c_string(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| CString::new(s.replace('\0', "")).unwrap())
}

/// Read a variable-length id array using the engine's "query size, then fill"
/// convention.
pub(crate) fn read_ids(mut fill: impl FnMut(*mut i32, i32) -> i32) -> Vec<i32> {
    let n = fill(std::ptr::null_mut(), i32::MAX);
    if n <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0i32; n as usize];
    let got = fill(buf.as_mut_ptr(), n).clamp(0, n);
    buf.truncate(got as usize);
    buf
}

/// Same as [`read_ids`] for float arrays.
pub(crate) fn read_floats(mut fill: impl FnMut(*mut f32, i32) -> i32) -> Vec<f32> {
    let n = fill(std::ptr::null_mut(), i32::MAX);
    if n <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0f32; n as usize];
    let got = fill(buf.as_mut_ptr(), n).clamp(0, n);
    buf.truncate(got as usize);
    buf
}

/// Read a `customParams`-style key/value table.
pub(crate) fn read_params(
    mut fill: impl FnMut(*mut *const c_char, *mut *const c_char) -> i32,
) -> Vec<(String, String)> {
    let n = fill(std::ptr::null_mut(), std::ptr::null_mut());
    if n <= 0 {
        return Vec::new();
    }
    let mut keys = vec![std::ptr::null::<c_char>(); n as usize];
    let mut vals = vec![std::ptr::null::<c_char>(); n as usize];
    let got = fill(keys.as_mut_ptr(), vals.as_mut_ptr()).clamp(0, n) as usize;
    keys.iter().zip(&vals).take(got).map(|(&k, &v)| (owned_str(k), owned_str(v))).collect()
}
