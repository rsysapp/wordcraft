//! embed: what identifies compared content: an unambiguous framing of it, kept as its bytes while it
//! is short and as its SHA-256 once it is longer. So the inspection keeps at most a few hundred
//! bytes per distinct thing instead of a printed copy of it: a part or an element of any size
//! costs the same to remember, and the common small marks cost no hashing. Two different contents
//! with one identity would take a SHA-256 collision.

/// Content kept as its framed bytes up to this length; longer content as its SHA-256.
const EXACT: usize = 256;

/// A content's identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Digest {
    Exact(Box<[u8]>),
    Sha256([u8; 32]),
}

impl Digest {
    /// The memory the identity itself holds.
    pub(super) fn held(&self) -> u64 {
        match self {
            Digest::Exact(bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            Digest::Sha256(_) => 32,
        }
    }
}

/// Collects framed content: its bytes while short, a running SHA-256 after.
#[derive(Clone, Default)]
pub(super) struct Hasher {
    short: Vec<u8>,
    long: Option<Sha256>,
    length: u64,
}

impl Hasher {
    /// Bytes fed so far, for charging the work they cost.
    pub(super) fn length(&self) -> u64 {
        self.length
    }

    pub(super) fn update(&mut self, bytes: &[u8]) {
        self.length = self.length.wrapping_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        match &mut self.long {
            Some(long) => long.update(bytes),
            None if self.short.len().saturating_add(bytes.len()) <= EXACT => self.short.extend_from_slice(bytes),
            None => {
                let mut long = Sha256::default();
                long.update(&self.short);
                long.update(bytes);
                self.short = Vec::new();
                self.long = Some(long);
            }
        }
    }

    /// One framed field: a tag, the length, the bytes. Framing makes the encoding unambiguous
    /// without escaping anything.
    pub(super) fn field(&mut self, tag: u8, bytes: &[u8]) {
        self.update(&[tag]);
        self.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        self.update(bytes);
    }

    pub(super) fn finish(self) -> Digest {
        match self.long {
            Some(long) => Digest::Sha256(long.finish()),
            None => Digest::Exact(self.short.into_boxed_slice()),
        }
    }
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// An incremental SHA-256 (FIPS 180-4).
#[derive(Clone)]
struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Sha256 {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }
}

impl Sha256 {
    fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        while !bytes.is_empty() {
            let room = 64 - self.filled;
            let take = room.min(bytes.len());
            let (now, rest) = bytes.split_at(take);
            if let Some(target) = self.block.get_mut(self.filled..self.filled + take) {
                target.copy_from_slice(now);
            }
            self.filled += take;
            bytes = rest;
            if self.filled == 64 {
                let block = self.block;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (index, chunk) in block.chunks_exact(4).enumerate() {
            if let (Some(word), Ok(bytes)) = (w.get_mut(index), <[u8; 4]>::try_from(chunk)) {
                *word = u32::from_be_bytes(bytes);
            }
        }
        for index in 16..64 {
            let at = |offset: usize| w.get(index - offset).copied().unwrap_or(0);
            let s0 = at(15).rotate_right(7) ^ at(15).rotate_right(18) ^ (at(15) >> 3);
            let s1 = at(2).rotate_right(17) ^ at(2).rotate_right(19) ^ (at(2) >> 10);
            let value = at(16).wrapping_add(s0).wrapping_add(at(7)).wrapping_add(s1);
            if let Some(word) = w.get_mut(index) {
                *word = value;
            }
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for (k, word) in K.iter().zip(w.iter()) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(choose).wrapping_add(*k).wrapping_add(*word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(digest: [u8; 32]) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn matches_the_standard_vectors() {
        let digest = |bytes: &[u8]| {
            let mut hasher = Sha256::default();
            hasher.update(bytes);
            hex(hasher.finish())
        };
        assert_eq!(digest(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(digest(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let mut split = Sha256::default();
        for byte in b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq" {
            split.update(&[*byte]);
        }
        assert_eq!(hex(split.finish()), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        assert_eq!(digest(&vec![b'a'; 1_000_000]), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn keeps_short_content_and_hashes_long_content() {
        let mut short = Hasher::default();
        short.field(b'T', b"Rent");
        assert!(matches!(short.finish(), Digest::Exact(bytes) if bytes.len() == 13));
        let mut long = Hasher::default();
        long.field(b'T', &[b'a'; 300]);
        let mut again = Hasher::default();
        for byte in [b'a'; 300] {
            if again.length() == 0 {
                again.update(b"T");
                again.update(&300u64.to_le_bytes());
            }
            again.update(&[byte]);
        }
        assert!(matches!(long.clone().finish(), Digest::Sha256(_)));
        assert_eq!(long.finish(), again.finish());
    }
}
