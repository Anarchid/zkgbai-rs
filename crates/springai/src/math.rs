use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

/// An engine-space position or direction (`AIFloat3`). `y` is up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };

    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// A ground-plane point with `y = 0`.
    #[inline]
    pub const fn xz(x: f32, z: f32) -> Self {
        Self { x, y: 0.0, z }
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        self.x == 0.0 && self.y == 0.0 && self.z == 0.0
    }

    /// Distance in the ground plane, ignoring height. This is what almost all
    /// AI logic wants.
    #[inline]
    pub fn distance_2d(&self, other: Vec3) -> f32 {
        let dx = self.x - other.x;
        let dz = self.z - other.z;
        (dx * dx + dz * dz).sqrt()
    }

    #[inline]
    pub fn distance_2d_sq(&self, other: Vec3) -> f32 {
        let dx = self.x - other.x;
        let dz = self.z - other.z;
        dx * dx + dz * dz
    }

    #[inline]
    pub fn distance(&self, other: Vec3) -> f32 {
        (*self - other).length()
    }

    #[inline]
    pub fn length(&self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    #[inline]
    pub fn length_2d(&self) -> f32 {
        (self.x * self.x + self.z * self.z).sqrt()
    }

    #[inline]
    pub(crate) fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }

    #[inline]
    pub(crate) fn from_array(a: [f32; 3]) -> Self {
        Self::new(a[0], a[1], a[2])
    }

    /// Read a position from an engine `float[3]`; null yields zero.
    ///
    /// # Safety
    /// `p` must be null or point to three readable floats.
    pub(crate) unsafe fn from_ptr(p: *const f32) -> Self {
        if p.is_null() { Self::ZERO } else { unsafe { Self::new(*p, *p.add(1), *p.add(2)) } }
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    #[inline]
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl AddAssign for Vec3 {
    #[inline]
    fn add_assign(&mut self, o: Vec3) {
        *self = *self + o;
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    #[inline]
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl SubAssign for Vec3 {
    #[inline]
    fn sub_assign(&mut self, o: Vec3) {
        *self = *self - o;
    }
}

impl Mul<f32> for Vec3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, k: f32) -> Vec3 {
        Vec3::new(self.x * k, self.y * k, self.z * k)
    }
}

impl Neg for Vec3 {
    type Output = Vec3;
    #[inline]
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}
