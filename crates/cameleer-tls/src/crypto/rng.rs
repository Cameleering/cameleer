//! Cryptographically secure random number generator: a ChaCha20-based DRBG
//! with fast key erasure, seeded (and periodically reseeded) from the OS:
//! `/dev/urandom` on Unix, `BCryptGenRandom` (system-preferred RNG) on Windows.
//!
//! Each request generates keystream blocks under the current key; the first 32
//! bytes of fresh keystream replace the key afterwards, so a later state
//! compromise does not reveal earlier outputs. If the OS source fails the
//! generator panics rather than producing weak output.

use super::chacha::chacha20_block;
use std::sync::Mutex;

struct Drbg {
    key: [u8; 32],
    counter: u64,
    generated: u64,
}

const RESEED_INTERVAL: u64 = 1 << 20; // bytes

impl Drbg {
    fn seeded() -> Drbg {
        let mut key = [0u8; 32];
        os_random(&mut key);
        Drbg { key, counter: 0, generated: 0 }
    }

    fn reseed(&mut self) {
        let mut fresh = [0u8; 32];
        os_random(&mut fresh);
        for (k, f) in self.key.iter_mut().zip(fresh.iter()) {
            *k ^= f;
        }
        self.generated = 0;
    }

    fn block(&mut self) -> [u8; 64] {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&self.counter.to_le_bytes());
        self.counter = self.counter.wrapping_add(1);
        chacha20_block(&self.key, 0, &nonce)
    }

    fn fill(&mut self, out: &mut [u8]) {
        if self.generated > RESEED_INTERVAL {
            self.reseed();
        }
        for chunk in out.chunks_mut(64) {
            let b = self.block();
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
        // fast key erasure
        let b = self.block();
        self.key.copy_from_slice(&b[..32]);
        self.generated += out.len() as u64;
    }
}

#[cfg(not(windows))]
fn os_random(out: &mut [u8]) {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").expect("cameleer-tls: cannot open /dev/urandom");
    f.read_exact(out).expect("cameleer-tls: cannot read /dev/urandom");
}

#[cfg(windows)]
#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(algorithm: *mut core::ffi::c_void, buffer: *mut u8, len: u32, flags: u32) -> i32;
}

#[cfg(windows)]
fn os_random(out: &mut [u8]) {
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 2;
    for chunk in out.chunks_mut(u32::MAX as usize) {
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), chunk.as_mut_ptr(), chunk.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        assert!(status >= 0, "cameleer-tls: BCryptGenRandom failed with NTSTATUS {status:#x}");
    }
}

static RNG: Mutex<Option<Drbg>> = Mutex::new(None);

/// Fills `out` with cryptographically secure random bytes.
pub fn fill(out: &mut [u8]) {
    let mut guard = RNG.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(Drbg::seeded).fill(out);
}

/// Returns `n` random bytes.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    fill(&mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drbg_outputs_differ_and_look_random() {
        let a = random_bytes(64);
        let b = random_bytes(64);
        assert_ne!(a, b);
        let big = random_bytes(1 << 16);
        let ones: u32 = big.iter().map(|x| x.count_ones()).sum();
        let total = (big.len() * 8) as f64;
        let ratio = ones as f64 / total;
        assert!((ratio - 0.5).abs() < 0.01, "bit ratio {ratio}");
    }

    #[test]
    fn deterministic_given_key() {
        let mut d1 = Drbg { key: [7; 32], counter: 0, generated: 0 };
        let mut d2 = Drbg { key: [7; 32], counter: 0, generated: 0 };
        let mut a = [0u8; 100];
        let mut b = [0u8; 100];
        d1.fill(&mut a);
        d2.fill(&mut b);
        assert_eq!(a, b);
        d1.fill(&mut a);
        assert_ne!(a, b);
    }
}
