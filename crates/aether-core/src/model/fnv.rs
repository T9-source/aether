//! Dependency-free FNV-1a hashing used by request/response fingerprints.

pub(crate) struct Fnv1a(u64);

impl Fnv1a {
    pub(crate) fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub(crate) fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_input_sensitive() {
        let mut a = Fnv1a::new();
        a.write(b"hello");
        let x = a.finish();

        let mut b = Fnv1a::new();
        b.write(b"hellp");
        let y = b.finish();

        let mut c = Fnv1a::new();
        c.write(b"hello");
        assert_eq!(x, c.finish());
        assert_ne!(x, y);
    }
}
