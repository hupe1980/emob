//! The two check-digit algorithms a contract identifier can carry.
//!
//! # Why a check digit is worth computing rather than storing
//!
//! An eMAID is read off a card, typed into a support form, copied out of a
//! partner's spreadsheet and pasted into an `Authorize`. Its last character
//! exists to catch exactly those journeys: it is a **transcription guard**, and
//! a transcription guard nobody evaluates is a character that makes an
//! identifier one longer.
//!
//! Getting it wrong is not cosmetic. A contract id that has lost a digit still
//! *parses*, still routes, and bills a session to somebody else's contract —
//! or to nobody, which surfaces weeks later as an unallocated CDR.
//!
//! # Two algorithms, because there are two grammars
//!
//! They are not variants of one idea and they do not agree:
//!
//! - **ISO 15118-1 / EMI3** ([`iso`]) works over a 2×2 matrix group. Each
//!   character decodes to a matrix; the sequence is combined against the powers
//!   of two fixed generators, and the residues mod 2 and mod 3 encode the
//!   digit. It detects transpositions, which a weighted sum does not.
//! - **DIN SPEC 91286** ([`din`]) is a weighted sum in base 11, with letters
//!   contributing two terms because their numeric values are two digits, and an
//!   `X` for the eleventh residue — the Roman numeral trick ISBN-10 uses.
//!
//! Both are implemented from the published grammars and pinned by the test
//! vectors the reference implementations publish, because an algorithm of this
//! shape is either exactly right or silently wrong.

/// The 36 characters a contract identifier is built from, in the order their
/// matrix encodings are defined.
const ALPHABET: [(char, i64); 36] = [
    ('0', 0),
    ('1', 16),
    ('2', 32),
    ('3', 4),
    ('4', 20),
    ('5', 36),
    ('6', 8),
    ('7', 24),
    ('8', 40),
    ('9', 2),
    ('A', 18),
    ('B', 34),
    ('C', 6),
    ('D', 22),
    ('E', 38),
    ('F', 10),
    ('G', 26),
    ('H', 42),
    ('I', 1),
    ('J', 17),
    ('K', 33),
    ('L', 5),
    ('M', 21),
    ('N', 37),
    ('O', 9),
    ('P', 25),
    ('Q', 41),
    ('R', 3),
    ('S', 19),
    ('T', 35),
    ('U', 7),
    ('V', 23),
    ('W', 39),
    ('X', 11),
    ('Y', 27),
    ('Z', 43),
];

/// A 2×2 matrix over the integers, which is the whole of the ISO algorithm's
/// machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Matrix {
    a: i64,
    b: i64,
    c: i64,
    d: i64,
}

impl Matrix {
    const fn new(a: i64, b: i64, c: i64, d: i64) -> Self {
        Self { a, b, c, d }
    }

    /// The packed form each character's matrix is defined by: two bits, then
    /// two bits, then two more.
    const fn unpack(x: i64) -> Self {
        Self::new(x & 1, (x >> 1) & 1, (x >> 2) & 3, x >> 4)
    }

    /// …and back, which is how the resulting matrix becomes a character.
    const fn pack(self) -> i64 {
        self.a + (self.b << 1) + (self.c << 2) + (self.d << 4)
    }

    const fn mul(self, other: Self) -> Self {
        Self::new(
            self.a * other.a + self.b * other.c,
            self.a * other.b + self.b * other.d,
            self.c * other.a + self.d * other.c,
            self.c * other.b + self.d * other.d,
        )
    }
}

/// A row vector, the shape each character contributes in.
#[derive(Debug, Clone, Copy)]
struct Vector(i64, i64);

impl Vector {
    /// Row vector times matrix.
    const fn mul(self, m: Matrix) -> Self {
        Self(self.0 * m.a + self.1 * m.c, self.0 * m.b + self.1 * m.d)
    }

    const fn add(self, other: Self) -> Self {
        Self(self.0 + other.0, self.1 + other.1)
    }
}

/// The ISO 15118-1 and EMI3 check digit.
///
/// `body` is the identifier **without** its check digit — country, provider and
/// instance, uppercase, separators removed, 14 characters.
///
/// # Errors
///
/// `None` when the body is not 14 characters of `[0-9A-Z]`, which is the only
/// way the algorithm can fail: every reachable result decodes to a character,
/// because the residues mod 2 and mod 3 span exactly the 36 encodings.
pub(crate) fn iso(body: &str) -> Option<char> {
    const LENGTH: usize = 14;
    if body.len() != LENGTH {
        return None;
    }

    // The two generators, and their powers. `p1` is the Fibonacci matrix and
    // `p2` its Pell cousin; the residues they produce are independent, which is
    // what lets four small numbers carry a 36-way choice.
    let mut p1 = Matrix::new(0, 1, 1, 1);
    let mut p2 = Matrix::new(0, 1, 1, 2);
    let (step1, step2) = (p1, p2);

    let mut t1 = Vector(0, 0);
    let mut t2 = Vector(0, 0);
    for (index, ch) in body.chars().enumerate() {
        let encoded = Matrix::unpack(encode(ch)?);
        t1 = t1.add(Vector(encoded.a, encoded.b).mul(p1));
        t2 = t2.add(Vector(encoded.c, encoded.d).mul(p2));
        if index + 1 < LENGTH {
            p1 = p1.mul(step1);
            p2 = p2.mul(step2);
        }
    }
    // `-p2^-15`, which closes the sequence: the check digit is the character
    // that makes the whole 15-character word sum to the identity.
    t2 = t2.mul(Matrix::new(0, 2, 2, 1));

    decode(Matrix::new(t1.0 & 1, t1.1 & 1, t2.0 % 3, t2.1 % 3).pack())
}

/// The DIN SPEC 91286 check digit.
///
/// `body` is the identifier without its check digit — country, provider and a
/// six-character instance, uppercase, separators removed, 11 characters.
///
/// # Errors
///
/// `None` when the body is not 11 characters of `[0-9A-Z]`.
pub(crate) fn din(body: &str) -> Option<char> {
    const LENGTH: usize = 11;
    if body.len() != LENGTH {
        return None;
    }

    let mut sum: i64 = 0;
    let mut coefficient: u32 = 0;
    for ch in body.chars() {
        let value = numeric_value(ch)?;
        if value < 10 {
            sum += value * pow2(coefficient);
            coefficient += 1;
        } else {
            // A letter's numeric value is two decimal digits, and each carries
            // its own weight — which is why an eleven-character body can reach
            // a coefficient of twenty-two.
            sum += (value / 10) * pow2(coefficient) + (value % 10) * pow2(coefficient + 1);
            coefficient += 2;
        }
    }

    let residue = sum % 11;
    // Eleven residues, ten digits. `X` takes the eleventh, exactly as ISBN-10
    // does and for the same reason.
    Some(if residue >= 10 {
        'X'
    } else {
        char::from(b'0' + u8::try_from(residue).ok()?)
    })
}

/// `0`–`9` → 0–9, `A`–`Z` → 10–35.
fn numeric_value(ch: char) -> Option<i64> {
    match ch {
        '0'..='9' => Some(i64::from(ch as u8 - b'0')),
        'A'..='Z' => Some(i64::from(ch as u8 - b'A') + 10),
        _ => None,
    }
}

/// Two to the power of a coefficient, as an exact integer. The exponent is
/// bounded by twice the body length, so this cannot overflow for any identifier
/// the grammars admit.
const fn pow2(exponent: u32) -> i64 {
    1i64 << exponent
}

fn encode(ch: char) -> Option<i64> {
    ALPHABET.iter().find_map(|&(c, x)| (c == ch).then_some(x))
}

fn decode(x: i64) -> Option<char> {
    ALPHABET
        .iter()
        .find_map(|&(c, encoded)| (encoded == x).then_some(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_iso_vectors_the_reference_implementations_publish() {
        // An algorithm of this shape is either exactly right or silently wrong,
        // so it is pinned to published answers rather than to a re-derivation.
        for (body, expected) in [
            ("NN123ABCDEFGHI", 'T'),
            ("FRXYZ123456789", '2'),
            ("ITA1B2C3E4F5G6", '4'),
            ("ESZU8WOX834H1D", 'R'),
            ("PT73902837ABCZ", 'Z'),
            ("DE83DUIEN83QGZ", 'D'),
            ("DE83DUIEN83ZGQ", 'M'),
            ("DE8AA001234567", '0'),
            ("NLTNM000122045", 'U'),
            ("NLTNM000722345", 'X'),
            ("NLTNMC00122045", 'K'),
        ] {
            assert_eq!(iso(body), Some(expected), "ISO check digit for {body}");
        }
    }

    #[test]
    fn the_iso_digit_detects_a_transposition() {
        // The property a weighted sum does not have, and the reason the
        // algorithm is a matrix product rather than a sum: two identifiers
        // differing only by a swap get different digits.
        assert_ne!(iso("DE83DUIEN83QGZ"), iso("DE83DUIEN83ZGQ"));
    }

    #[test]
    fn the_din_vectors_the_reference_implementations_publish() {
        for (body, expected) in [
            ("INTNM000071", '9'),
            ("INTNM000110", 'X'),
            ("INTNM000124", '0'),
            ("INTNM000114", '6'),
            ("INTNM000191", '5'),
            ("NLTNM012204", '5'),
            ("NLTNM122045", '0'),
        ] {
            assert_eq!(din(body), Some(expected), "DIN check digit for {body}");
        }
    }

    #[test]
    fn the_eleventh_residue_is_x() {
        // Ten digits cannot express eleven residues, and a scheme that folded
        // the eleventh onto `0` would stop catching a whole class of errors.
        assert_eq!(din("INTNM000110"), Some('X'));
    }

    #[test]
    fn a_wrong_length_or_a_stray_character_yields_nothing() {
        assert_eq!(iso("TOOSHORT"), None);
        assert_eq!(iso("DE8AA0012345678"), None);
        assert_eq!(iso("DE8AA00123456!"), None);
        assert_eq!(iso("DE8AA0012345 7"), None);
        assert_eq!(din("SHORT"), None);
        assert_eq!(din("NLTNM01220!"), None);
    }

    #[test]
    fn every_alphabet_entry_round_trips() {
        // The property the decode step depends on: the 36 encodings are
        // distinct, and each is reachable from the residues the algorithm can
        // produce.
        for &(ch, x) in &ALPHABET {
            assert_eq!(encode(ch), Some(x));
            assert_eq!(decode(x), Some(ch));
            assert_eq!(Matrix::unpack(x).pack(), x);
        }
        let mut packed: Vec<i64> = ALPHABET.iter().map(|&(_, x)| x).collect();
        packed.sort_unstable();
        packed.dedup();
        assert_eq!(packed.len(), 36, "the encodings must be distinct");
    }
}
