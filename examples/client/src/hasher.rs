use std::hash::{BuildHasher, Hasher};

/// A custom hasher that uses the first 8 bytes of a U256 as the hash value
#[derive(Default)]
pub struct BytesHasher {
    hash: u64,
}

impl Hasher for BytesHasher {
    fn finish(&self) -> u64 {
        self.hash
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut hash_bytes = [0u8; 8];
        let len = bytes.len().min(8);
        hash_bytes[..len].copy_from_slice(&bytes[..len]);
        self.hash = u64::from_be_bytes(hash_bytes);
    }
}

/// A builder for BytesHasher
#[derive(Clone, Default)]
pub struct BytesHasherBuilder;

impl BuildHasher for BytesHasherBuilder {
    type Hasher = BytesHasher;

    fn build_hasher(&self) -> Self::Hasher {
        BytesHasher::default()
    }
}
