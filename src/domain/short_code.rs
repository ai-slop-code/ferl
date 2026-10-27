use std::fmt;

use serde::Serialize;

use super::{ValidationError, invalid};

pub const GENERATED_CODE_LENGTH: usize = 8;
const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const ACCEPTED_RANDOM_BYTE_MAX: u8 = 252; // 252 is the largest multiple of 36 below 256.
const MAX_REJECTIONS_PER_CHARACTER: usize = 128;

#[derive(Serialize)]
#[serde(transparent)]
pub struct ShortCode(String);

pub trait RandomBytes {
    fn fill(&mut self, output: &mut [u8]) -> Result<(), RandomnessError>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RandomnessError;

impl ShortCode {
    pub fn parse_custom(raw: &str) -> Result<Self, ValidationError> {
        if (3..=64).contains(&raw.len())
            && raw
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            Ok(Self(raw.to_owned()))
        } else {
            Err(invalid("short-code syntax is invalid"))
        }
    }

    pub fn generate() -> Result<Self, RandomnessError> {
        struct OsRandom;
        impl RandomBytes for OsRandom {
            fn fill(&mut self, output: &mut [u8]) -> Result<(), RandomnessError> {
                getrandom::fill(output).map_err(|_| RandomnessError)
            }
        }
        Self::generate_with(&mut OsRandom)
    }

    pub fn generate_with(random: &mut impl RandomBytes) -> Result<Self, RandomnessError> {
        let mut code = String::with_capacity(GENERATED_CODE_LENGTH);
        for _ in 0..GENERATED_CODE_LENGTH {
            let mut selected = None;
            for _ in 0..MAX_REJECTIONS_PER_CHARACTER {
                let mut byte = [0_u8; 1];
                random.fill(&mut byte)?;
                if byte[0] < ACCEPTED_RANDOM_BYTE_MAX {
                    selected = Some(ALPHABET[usize::from(byte[0] % 36)]);
                    break;
                }
            }
            let Some(character) = selected else {
                return Err(RandomnessError);
            };
            code.push(char::from(character));
        }
        Ok(Self(code))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ShortCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ShortCode([REDACTED])")
    }
}

impl fmt::Debug for RandomnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RandomnessError")
    }
}

impl fmt::Display for RandomnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cryptographic randomness is unavailable")
    }
}

impl std::error::Error for RandomnessError {}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::VecDeque};

    use super::{RandomBytes, RandomnessError, ShortCode};

    struct Sequence {
        bytes: VecDeque<u8>,
        calls: Cell<usize>,
        fail: bool,
    }

    impl RandomBytes for Sequence {
        fn fill(&mut self, output: &mut [u8]) -> Result<(), RandomnessError> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                return Err(RandomnessError);
            }
            for byte in output {
                *byte = self.bytes.pop_front().unwrap_or(0);
            }
            Ok(())
        }
    }

    #[test]
    fn syntax_is_exact_and_reservation_conflicts_are_out_of_scope() {
        for code in ["api", "assets", "health", "api-sale", "---", "a-b"] {
            assert_eq!(ShortCode::parse_custom(code).unwrap().as_str(), code);
        }
        for code in ["ab", "ABC", "a_b", "a b", "ééé"] {
            assert!(ShortCode::parse_custom(code).is_err());
        }
    }

    #[test]
    fn generator_rejects_out_of_range_bytes_before_modulo_mapping() {
        let mut random = Sequence {
            bytes: [255, 252, 251, 0, 35, 36, 72, 108, 144, 180, 216, 248]
                .into_iter()
                .collect(),
            calls: Cell::new(0),
            fail: false,
        };
        let code = ShortCode::generate_with(&mut random).unwrap();
        assert_eq!(code.as_str().len(), 8);
        assert_eq!(code.as_str(), "9a9aaaaa");
        assert_eq!(random.calls.get(), 10);
    }

    #[test]
    fn randomness_failure_and_exhausted_rejection_sampling_are_bounded() {
        let mut failing = Sequence {
            bytes: VecDeque::new(),
            calls: Cell::new(0),
            fail: true,
        };
        assert!(ShortCode::generate_with(&mut failing).is_err());
        assert_eq!(failing.calls.get(), 1);

        let mut rejected = Sequence {
            bytes: std::iter::repeat_n(255, 1_000).collect(),
            calls: Cell::new(0),
            fail: false,
        };
        assert!(ShortCode::generate_with(&mut rejected).is_err());
        assert_eq!(rejected.calls.get(), 128);
    }

    #[test]
    fn production_generator_uses_fixed_length_lowercase_alphanumeric() {
        let value = ShortCode::generate().expect("operating system randomness is available");
        assert_eq!(value.as_str().len(), 8);
        assert!(
            value
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        );
    }
}
