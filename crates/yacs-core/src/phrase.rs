//! Generating pairing phrases.
//!
//! Words come from the EFF Large Wordlist (7776 words, CC BY 3.0 US,
//! <https://www.eff.org/dice>), so each word adds log2(7776) ≈ 12.9 bits.

use std::sync::LazyLock;

use crate::error::{Error, Result};

/// 6 words ≈ 77.5 bits: far beyond offline guessing, even against a leaked channel id.
pub const DEFAULT_PHRASE_WORDS: usize = 6;

static WORDS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| include_str!("eff_large_wordlist.txt").lines().collect());

/// A random phrase of `words` words from the system RNG, separated by spaces.
pub fn generate_phrase(words: usize) -> Result<String> {
    let list = &*WORDS;
    let mut picked = Vec::with_capacity(words);
    while picked.len() < words {
        picked.push(list[random_index(list.len())?]);
    }
    Ok(picked.join(" "))
}

/// Uniform in `0..n` by rejection sampling, so no word is more likely than another.
fn random_index(n: usize) -> Result<usize> {
    let n = n as u32;
    let limit = u32::MAX - u32::MAX % n;
    loop {
        let x = getrandom::u32().map_err(|_| Error::Rng)?;
        if x < limit {
            return Ok((x % n) as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::normalize_phrase;

    #[test]
    fn wordlist_is_complete() {
        assert_eq!(WORDS.len(), 7776);
        assert_eq!(WORDS.iter().collect::<HashSet<_>>().len(), 7776);
        assert_eq!((WORDS[0], WORDS[7775]), ("abacus", "zoom"));
    }

    #[test]
    fn phrases_have_the_requested_length_and_are_already_normalized() {
        let phrase = generate_phrase(DEFAULT_PHRASE_WORDS).unwrap();
        assert_eq!(phrase.split(' ').count(), DEFAULT_PHRASE_WORDS);
        assert_eq!(normalize_phrase(&phrase), phrase);
        assert!(phrase.split(' ').all(|w| WORDS.contains(&w)));
    }

    #[test]
    fn phrases_differ() {
        let phrases: HashSet<_> = (0..20).map(|_| generate_phrase(6).unwrap()).collect();
        assert_eq!(phrases.len(), 20);
    }
}
