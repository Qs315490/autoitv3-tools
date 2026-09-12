//! The two stream ciphers an AutoIt build scrambles its embedded files with.
//!
//! AutoIt does not encrypt with a real cipher here: it builds a pseudo-random
//! byte stream from a 32-bit seed and XORs the data with it. Both generators
//! are reproduced bit-for-bit, because a single wrong byte anywhere in the
//! stream turns the decrypted header into noise and the container stops
//! parsing — the cipher is not self-synchronising.
//!
//! * `EA06` (AutoIt v3.2.0 and later) uses the generator AutoIt's own source
//!   calls `LAME`: a 17-word lagged-Fibonacci setup whose output is a *double*
//!   in `[0, 1)`, scaled by 256. The double is assembled from the raw word
//!   rather than computed, so the bit pattern has to be built the same way.
//! * `EA05` uses a tweaked Mersenne Twister: AutoIt's tempering and seeding
//!   differ from the textbook `MT19937`, so a stock implementation would not
//!   decrypt anything.
//!
//! Both are ports of the corresponding routines in the MIT-licensed
//! AutoIt-Ripper (`lame.py` / `mt.py`), which in turn mirror AutoIt's source.

/// The keystream `EA06` derives from `seed`, `len` bytes long.
pub fn ea06_keystream(seed: u32, len: usize) -> Vec<u8> {
    let mut lame = Lame::new(seed);
    (0..len).map(|_| lame.next_byte()).collect()
}

/// The keystream `EA05` derives from `seed`, `len` bytes long.
pub fn ea05_keystream(seed: u32, len: usize) -> Vec<u8> {
    let mut mt = Mt::new(seed);
    (0..len).map(|_| mt.next_byte()).collect()
}

/// XOR `data` with a keystream derived from `seed`.
///
/// Decrypting and encrypting are the same operation, which is what lets the
/// tests build a container and then read it back.
pub fn xor_keystream(data: &[u8], seed: u32, ea06: bool) -> Vec<u8> {
    let stream = if ea06 {
        ea06_keystream(seed, data.len())
    } else {
        ea05_keystream(seed, data.len())
    };
    data.iter().zip(stream).map(|(byte, key)| byte ^ key).collect()
}

/// AutoIt's `LAME` generator: a 17-word lagged-Fibonacci accumulator.
#[derive(Debug, Clone)]
struct Lame {
    /// Index of the word the next output is written to and taken from.
    c0: usize,
    /// Index of the second word the output combines.
    c1: usize,
    /// The generator's state, `grp1[0..17]`.
    grp1: [u32; 17],
}

impl Lame {
    /// `srand(seed)`: fill the state, then throw nine outputs away.
    fn new(seed: u32) -> Self {
        let mut grp1 = [0u32; 17];
        let mut seed = seed;
        for slot in grp1.iter_mut() {
            seed = 1u32.wrapping_sub(seed.wrapping_mul(0x53A9_B4FB));
            *slot = seed;
        }
        // The warm-up count and the two read positions are part of the format,
        // not tuning: any other choice yields a different stream.
        let mut lame = Lame { c0: 0, c1: 10, grp1 };
        for _ in 0..9 {
            lame.advance();
        }
        lame
    }

    /// One step of `fpusht`, returning the generator's raw output.
    fn advance(&mut self) -> f64 {
        let rolled = self.grp1[self.c0]
            .rotate_left(9)
            .wrapping_add(self.grp1[self.c1].rotate_left(13));
        self.grp1[self.c0] = rolled;
        self.c0 = if self.c0 == 0 { 16 } else { self.c0 - 1 };
        self.c1 = if self.c1 == 0 { 16 } else { self.c1 - 1 };

        // The word becomes the mantissa of a double in `[1, 2)`, built by hand
        // (a bit pattern, not an arithmetic conversion), then shifted down.
        let low = rolled.wrapping_shl(20) as u64;
        let high = ((rolled >> 12) | 0x3FF0_0000) as u64;
        f64::from_bits((high << 32) | (low & 0xFFFF_FFFF)) - 1.0
    }

    /// One byte: the reference consumes two outputs per byte.
    fn next_byte(&mut self) -> u8 {
        self.advance();
        (self.advance() * 256.0) as u8
    }
}

/// AutoIt's `EA05` Mersenne Twister, with its non-standard seeding and
/// tempering.
#[derive(Debug, Clone)]
struct Mt {
    state: [u32; 624],
    /// How many words have been taken; a new block is twisted every 624.
    taken: usize,
}

impl Mt {
    /// `MT(seed)`: the state is a plain recurrence over the seed.
    fn new(seed: u32) -> Self {
        let mut state = [0u32; 624];
        state[0] = seed;
        for i in 1..624 {
            let last = state[i - 1];
            state[i] = (i as u32).wrapping_add(0x6C07_8965u32.wrapping_mul(last ^ (last >> 30)));
        }
        Mt { state, taken: 0 }
    }

    /// Regenerate the whole state block, in the order AutoIt does it.
    ///
    /// The middle loop reads words the first loop has already overwritten, so
    /// the two loops are not independent and must stay in this order.
    fn twist(&mut self) {
        for i in 0..227 {
            let mut word = self.state[i + 397];
            word ^= (self.state[i] ^ ((self.state[i + 1] ^ self.state[i]) & 0x7FFF_FFFE)) >> 1;
            if self.state[i + 1] & 1 != 0 {
                word ^= 0x9908_B0DF;
            }
            self.state[i] = word;
        }
        for i in 0..396 {
            let mut word = self.state[i];
            word ^= (self.state[i + 227]
                ^ ((self.state[i + 228] ^ self.state[i + 227]) & 0x7FFF_FFFE))
                >> 1;
            if self.state[i + 228] & 1 != 0 {
                word ^= 0x9908_B0DF;
            }
            self.state[227 + i] = word;
        }
        let mut word = self.state[396];
        word ^= (self.state[623] ^ ((self.state[0] ^ self.state[623]) & 0x7FFF_FFFE)) >> 1;
        if self.state[0] & 1 != 0 {
            word ^= 0x9908_B0DF;
        }
        self.state[623] = word;
    }

    /// One byte of keystream: AutoIt tempers the word, then keeps its low byte.
    fn next_byte(&mut self) -> u8 {
        if self.taken % 624 == 0 {
            self.twist();
        }
        let word = self.state[self.taken % 624] as u64;
        self.taken += 1;

        // Widened to 64 bits on purpose: the reference does this arithmetic in
        // unbounded integers, and the shifts carry bits past 32. Only the low
        // byte survives, but truncating early would change it.
        let word = ((((word >> 11) ^ word) & 0xFF3A_58AD) << 7) ^ (word >> 11) ^ word;
        let word = ((word & 0xFFFF_DF8C) << 15)
            ^ word
            ^ ((((word & 0xFFFF_DF8C) << 15) ^ word) >> 18);
        ((word >> 1) & 0xFF) as u8
    }
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../../tests/unit/script_keys.rs"]
mod tests;
