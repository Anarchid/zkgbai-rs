/// Generate trivial engine getters on a handle type.
///
/// Forms inside the block:
/// - `fn name() -> T = C_fn;` — plain value passthrough (`i32`, `f32`, `bool`).
/// - `str fn name() = C_fn;` — returns an owned `String`.
/// - `res fn name() = C_fn;` — takes a [`Resource`](crate::Resource), returns `f32`.
macro_rules! getters {
    ($ty:ident . $id:ident { $($body:tt)* }) => {
        impl $ty { getters!(@items $id; $($body)*); }
    };
    (@items $id:ident;) => {};
    (@items $id:ident; $(#[$m:meta])* fn $name:ident() -> $ret:ty = $cfn:ident; $($rest:tt)*) => {
        $(#[$m])*
        #[inline]
        pub fn $name(&self) -> $ret {
            $crate::callback::ffi!(self.cb, $cfn, self.$id)
        }
        getters!(@items $id; $($rest)*);
    };
    (@items $id:ident; $(#[$m:meta])* str fn $name:ident() = $cfn:ident; $($rest:tt)*) => {
        $(#[$m])*
        pub fn $name(&self) -> String {
            $crate::callback::owned_str($crate::callback::ffi!(self.cb, $cfn, self.$id))
        }
        getters!(@items $id; $($rest)*);
    };
    (@items $id:ident; $(#[$m:meta])* res fn $name:ident() = $cfn:ident; $($rest:tt)*) => {
        $(#[$m])*
        #[inline]
        pub fn $name(&self, res: $crate::Resource) -> f32 {
            $crate::callback::ffi!(self.cb, $cfn, self.$id, res.id())
        }
        getters!(@items $id; $($rest)*);
    };
}
pub(crate) use getters;

/// Declare a `Copy` object handle (`cb` + integer id) whose identity is the id.
macro_rules! handle {
    ($(#[$m:meta])* $ty:ident, $id:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy)]
        pub struct $ty {
            pub(crate) cb: $crate::Callback,
            pub(crate) $id: i32,
        }

        impl $ty {
            /// Wrap a raw engine id. Negative ids mean "none" and yield `None`.
            #[inline]
            pub fn from_id(cb: $crate::Callback, id: i32) -> Option<Self> {
                (id >= 0).then_some(Self { cb, $id: id })
            }

            /// The raw engine id.
            #[inline]
            pub fn id(&self) -> i32 {
                self.$id
            }

            /// The callback this handle queries through.
            #[inline]
            pub fn callback(&self) -> $crate::Callback {
                self.cb
            }
        }

        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                self.$id == other.$id
            }
        }
        impl Eq for $ty {}
        impl std::hash::Hash for $ty {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.$id.hash(state)
            }
        }
        impl PartialOrd for $ty {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for $ty {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.$id.cmp(&other.$id)
            }
        }
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!(stringify!($ty), "({})"), self.$id)
            }
        }
    };
}
pub(crate) use handle;
