use xxhash_rust::xxh32::xxh32;

/// Pre-computed hex lookup table for hash values 0-255.
const HASH_TABLE: [&str; 256] = [
    "00", "01", "02", "03", "04", "05", "06", "07", "08", "09", "0a", "0b", "0c", "0d", "0e", "0f",
    "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "1a", "1b", "1c", "1d", "1e", "1f",
    "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "2a", "2b", "2c", "2d", "2e", "2f",
    "30", "31", "32", "33", "34", "35", "36", "37", "38", "39", "3a", "3b", "3c", "3d", "3e", "3f",
    "40", "41", "42", "43", "44", "45", "46", "47", "48", "49", "4a", "4b", "4c", "4d", "4e", "4f",
    "50", "51", "52", "53", "54", "55", "56", "57", "58", "59", "5a", "5b", "5c", "5d", "5e", "5f",
    "60", "61", "62", "63", "64", "65", "66", "67", "68", "69", "6a", "6b", "6c", "6d", "6e", "6f",
    "70", "71", "72", "73", "74", "75", "76", "77", "78", "79", "7a", "7b", "7c", "7d", "7e", "7f",
    "80", "81", "82", "83", "84", "85", "86", "87", "88", "89", "8a", "8b", "8c", "8d", "8e", "8f",
    "90", "91", "92", "93", "94", "95", "96", "97", "98", "99", "9a", "9b", "9c", "9d", "9e", "9f",
    "a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8", "a9", "aa", "ab", "ac", "ad", "ae", "af",
    "b0", "b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8", "b9", "ba", "bb", "bc", "bd", "be", "bf",
    "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "ca", "cb", "cc", "cd", "ce", "cf",
    "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8", "d9", "da", "db", "dc", "dd", "de", "df",
    "e0", "e1", "e2", "e3", "e4", "e5", "e6", "e7", "e8", "e9", "ea", "eb", "ec", "ed", "ee", "ef",
    "f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "fa", "fb", "fc", "fd", "fe", "ff",
];

/// Compute the hashline hash for a line of text.
///
/// Strips all whitespace, computes xxHash32 of the bytes, takes mod 256,
/// and returns a 2-char lowercase hex string (borrowed from a static table).
pub fn compute_line_hash(line: &str) -> &'static str {
    let normalized: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    let hash = xxh32(normalized.as_bytes(), 0) as usize % 256;
    HASH_TABLE[hash]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_line() {
        // Empty line should hash consistently
        let h = compute_line_hash("");
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn test_whitespace_insensitive() {
        let h1 = compute_line_hash("  const x = 1;  ");
        let h2 = compute_line_hash("const x = 1;");
        let h3 = compute_line_hash("constx=1;");
        assert_eq!(h1, h2);
        assert_eq!(h2, h3);
    }

    #[test]
    fn test_different_content_may_differ() {
        let h1 = compute_line_hash("const x = 1;");
        let h2 = compute_line_hash("const y = 2;");
        // Different content will usually differ but collisions are possible
        // Just verify they return valid 2-char hex
        assert_eq!(h1.len(), 2);
        assert_eq!(h2.len(), 2);
    }

    #[test]
    fn test_hash_is_valid_hex() {
        for &entry in &HASH_TABLE {
            assert_eq!(entry.len(), 2);
            assert!(u8::from_str_radix(entry, 16).is_ok());
        }
    }

    #[test]
    fn test_known_values() {
        // Verify the hash is deterministic
        let h = compute_line_hash("function hello() {");
        assert_eq!(h.len(), 2);
        // Same input should always produce same output
        assert_eq!(h, compute_line_hash("function hello() {"));
    }

    #[test]
    fn test_only_whitespace() {
        // A line that is only whitespace normalizes to empty
        let h1 = compute_line_hash("   ");
        let h2 = compute_line_hash("");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_tab_and_space_equivalence() {
        let h1 = compute_line_hash("\tconst x = 1;");
        let h2 = compute_line_hash("    const x = 1;");
        assert_eq!(h1, h2);
    }
}
