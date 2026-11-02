//! Name matching: cheap prefilters first, then an fzf-style subsequence score.
//!
//! The whole point of Prism is that a sloppy query still finds the file. So the
//! engine never returns "no results" the way Windows Search does — it degrades in
//! tiers (all tokens matched → most tokens → best partial) and ranks the rest.

/// Per-entry signature used as the prefilter.
///
/// * `mask`  — 26 bits, one per ASCII letter present in the name.
/// * `bloom` — 32 bits of hashed characters (covers digits/punctuation/dots).
///
/// A query whose mask/bloom is not a subset cannot match, so 95-99% of the
/// 750k-entry index is rejected with two ANDs and two compares — no byte scans.
#[inline]
pub fn sigil(name: &[u8]) -> (u32, u64) {
    let mut mask = 0u64;
    let mut bloom = 0u32;
    for &b in name {
        let c = b | 0x20; // cheap ASCII lower
        if c.is_ascii_alphabetic() {
            mask |= 1u64 << (c - b'a');
        }
        bloom |= 1u32 << (hash32(c as u32) & 31);
    }
    (bloom, mask)
}

#[inline]
fn hash32(mut x: u32) -> u32 {
    x = x.wrapping_mul(0x9E37_79B1);
    x ^= x >> 15;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^ (x >> 13)
}

/// 128-bit trigram bloom. This is the strong prefilter: a query that appears
/// verbatim in a name cannot miss, but names that merely share its letters are
/// rejected before any byte of the name is touched.
#[inline]
pub fn trigram_sigil(name: &[u8]) -> [u64; 2] {
    let mut out = [0u64; 2];
    if name.len() < 3 {
        return out;
    }
    for w in name.windows(3) {
        let a = lowercase(w[0]) as u32;
        let b = lowercase(w[1]) as u32;
        let c = lowercase(w[2]) as u32;
        let h = hash32((a << 16) | (b << 8) | c);
        out[0] |= 1u64 << (h & 63);
        out[1] |= 1u64 << (hash32(h) & 63);
    }
    out
}

#[inline]
fn lowercase(b: u8) -> u8 {
    if b.is_ascii_alphabetic() {
        b | 0x20
    } else {
        b
    }
}

#[inline]
fn is_sep(c: u8) -> bool {
    matches!(
        c,
        b'.' | b'_' | b'-' | b' ' | b'\\' | b'/' | b'#' | b')' | b'['
    )
}

/// A query split into whitespace-separated tokens, each prefiltered.
pub struct Query {
    pub tokens: Vec<Vec<u8>>,
    /// AND of every token mask/bloom: entry must contain *all* query characters.
    pub mask_and: u64,
    pub bloom_and: u32,
    /// OR of every token mask: entry containing *any* query char stays a candidate.
    pub mask_or: u64,
    /// Bytes of all tokens glued, for whole-query substring bonuses.
    pub joined: Vec<u8>,
    /// Strong prefilter: every trigram of every long-enough token must be
    /// present. `None` when a token is shorter than 3 (no trigram to require).
    pub tri: Option<[u64; 2]>,
    pub empty: bool,
}

impl Query {
    pub fn parse(q: &str) -> Query {
        let mut tokens: Vec<Vec<u8>> = q
            .split_whitespace()
            .map(|t| t.bytes().map(|b| b | 0x20).take(64).collect())
            .filter(|t: &Vec<u8>| !t.is_empty())
            .take(12)
            .collect();
        // Longest token first: it rejects the most entries when ANDed.
        tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));

        let mut mask_and = !0u64;
        let mut bloom_and = !0u32;
        let mut mask_or = 0u64;
        let mut joined = Vec::new();
        let mut tri = if tokens.iter().all(|t| t.len() >= 3) {
            Some([0u64; 2])
        } else {
            None
        };
        for t in &tokens {
            let (b, m) = sigil(t);
            mask_and &= m;
            bloom_and &= b;
            mask_or |= m;
            joined.extend_from_slice(t);
            if let Some(acc) = &mut tri {
                let ts = trigram_sigil(t);
                acc[0] |= ts[0];
                acc[1] |= ts[1];
            }
        }
        if tokens.is_empty() {
            tri = None;
        }
        Query {
            empty: tokens.is_empty(),
            tokens,
            mask_and,
            bloom_and,
            mask_or,
            joined,
            tri,
        }
    }

    /// Cheap "impossible" test against the AND signature.
    #[inline]
    pub fn rejects_all(&self, bloom: u32, mask: u64) -> bool {
        (mask & self.mask_and) != self.mask_and || (bloom & self.bloom_and) != self.bloom_and
    }

    /// Cheap "no token even partially fits" test — the fallback tier prefilter.
    #[inline]
    pub fn rejects_any(&self, mask: u64) -> bool {
        self.tokens.len() > 1 && (mask & self.mask_or) == 0
    }
}

/// Score of one token against one name. Higher is better; `None` = not a subsequence.
///
/// Bonuses: adjacent runs, start-of-name, right after a separator (`.`, `_`, `-`,
/// space), case agreement. Penalties: leading gap, total gaps, name length.
pub fn score_token(name: &[u8], tok: &[u8], qcase: &[bool]) -> Option<i32> {
    let (nl, tl) = (name.len(), tok.len());
    if tl == 0 {
        return Some(0);
    }
    if tl > nl {
        return None;
    }

    // Fast path: contiguous substring → strong score, no DP.
    if let Some(pos) = find_subseq_free(name, tok) {
        let mut s = 100 + 40 * tl as i32 - pos as i32 / 2;
        if pos == 0 {
            s += 60;
        } else if is_sep(name[pos - 1]) {
            s += 45;
        }
        if case_bonus(name, tok, qcase, pos, true) {
            s += 25;
        }
        s -= nl as i32 / 6;
        return Some(s);
    }

    // Subsequence path.
    let mut score = 0i32;
    let mut ni = 0usize;
    let mut prev: Option<usize> = None;
    let mut first = usize::MAX;
    for (ti, &tc) in tok.iter().enumerate() {
        let mut found = None;
        while ni < nl {
            let c = name[ni] | 0x20;
            if c == tc {
                found = Some(ni);
                ni += 1;
                break;
            }
            ni += 1;
        }
        let i = found?;
        if first == usize::MAX {
            first = i;
            score += if i == 0 {
                55
            } else if is_sep(name[i - 1]) {
                42
            } else {
                12
            };
        } else if let Some(p) = prev {
            if i == p + 1 {
                score += 28; // run continues
            } else if is_sep(name[i - 1]) {
                score += 14;
            } else {
                score -= (i - p - 1) as i32 * 2; // gap penalty
            }
        }
        // Case agreement (query typed uppercase → name uppercase).
        if ti < qcase.len() && qcase[ti] {
            if name[i].is_ascii_lowercase() {
                // name lower, query upper → mild penalty, he typed a capital for a reason
                score -= 4;
            } else if name[i].is_ascii_uppercase() {
                score += 10;
            }
        }
        score += 10;
        prev = Some(i);
    }
    score -= first as i32 / 2;
    score -= nl as i32 / 6;
    Some(score)
}

/// Case-insensitive substring search (both sides already-lowerable ASCII).
#[inline]
fn find_subseq_free(name: &[u8], tok: &[u8]) -> Option<usize> {
    if tok.len() > name.len() {
        return None;
    }
    'outer: for s in 0..=(name.len() - tok.len()) {
        for k in 0..tok.len() {
            if (name[s + k] | 0x20) != tok[k] {
                continue 'outer;
            }
        }
        return Some(s);
    }
    None
}

fn case_bonus(name: &[u8], tok: &[u8], qcase: &[bool], at: usize, contiguous: bool) -> bool {
    if !contiguous {
        return false;
    }
    for i in 0..tok.len().min(qcase.len()) {
        if qcase[i] && name[at + i].is_ascii_uppercase() {
            return true;
        }
    }
    false
}

/// Multi-token score against a name.
/// Returns `(tier, score)` where tier = number of tokens matched (0..n).
/// `qcase` marks which query bytes were typed uppercase (for case agreement).
pub fn score_name(name: &[u8], q: &Query, qcase: &[bool]) -> (u8, i32) {
    if q.empty {
        return (1, 0);
    }
    let mut matched = 0u8;
    let mut total = 0i32;
    let mut joined_hit = false;
    for t in &q.tokens {
        if let Some(s) = score_token(name, t, qcase) {
            matched += 1;
            total += s;
        }
    }
    if matched as usize == q.tokens.len() && find_subseq_free(name, &q.joined).is_some() {
        joined_hit = true;
    }
    if joined_hit {
        total += 120;
    }
    (matched, total)
}

/// Which query bytes were typed uppercase (parallel to `joined`).
pub fn case_flags(q: &str) -> Vec<bool> {
    let mut v = Vec::with_capacity(q.len());
    for c in q.chars() {
        if c.is_whitespace() {
            continue;
        }
        v.push(c.is_uppercase());
        if v.len() >= 64 * 12 {
            break;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str, q: &str) -> (u8, i32) {
        let qq = Query::parse(q);
        let cs = case_flags(q);
        score_name(name.as_bytes(), &qq, &cs)
    }

    #[test]
    fn typo_tolerant_subsequence() {
        // "agends" misses the 'g' order but is still a subsequence of AGENTS
        assert_eq!(s("AGENTS.md", "agens").0, 1);
        assert!(s("AGENTS.md", "xyz").0 == 0);
    }

    #[test]
    fn a_space_is_a_second_token() {
        // Regression: the query field trimmed every typed character, so pressing
        // space was a no-op and "prism agents" arrived here as "prismagents", a
        // single misspelled token instead of two. Every word must survive as one.
        let q = Query::parse("prism agents");
        assert_eq!(q.tokens.len(), 2, "space split the words: {:?}", q.tokens);
        assert!(q.tokens.contains(&b"prism".to_vec()));
        assert!(q.tokens.contains(&b"agents".to_vec()));

        // Two whole words in one name. `prism_agents.exe` contains the joined
        // "prismagents" literally and gets the +120 glue bonus plus the separator
        // bonus; the underscoreless spelling only has the fragments in order, so
        // it still clears tier 2 but must rank below it.
        let split = s("prism_agents.exe", "prism agents");
        let run_on = s("prismagents.exe", "prism agents");
        assert_eq!(split.0, 2);
        assert_eq!(run_on.0, 2);
        assert!(split.1 > run_on.1, "{split:?} should rank above {run_on:?}");
    }

    #[test]
    fn leading_and_trailing_spaces_are_harmless() {
        // A typed space at the start or end of the query is still stored; the
        // parser simply has no token for it, so the same files keep matching.
        let q = Query::parse(" agents ");
        assert_eq!(q.tokens.len(), 1);
        assert_eq!(s("AGENTS.md", " agents ").0, 1);
        assert_eq!(s("AGENTS.md", " agents ").1, s("AGENTS.md", "agents").1);
    }

    #[test]
    fn substring_beats_scattered() {
        let tight = s("trailer_final_v3.mp4", "trailer").1;
        let loose = s("t_he_r_a_i_l_e_r_project.zip", "trailer").1;
        assert!(tight > loose, "{tight} should beat {loose}");
    }

    #[test]
    fn prefix_and_sep_bonus() {
        let a = s("prism.exe", "prism").1;
        let b = s("my_old_prism_backup.txt", "prism").1;
        assert!(a > b);
    }

    #[test]
    fn all_tokens_required_for_top_tier() {
        assert_eq!(s("report_2026_q3.pdf", "report q3").0, 2);
        assert_eq!(s("report_2026_q3.pdf", "report zzz").0, 1);
    }

    #[test]
    fn prefilter_never_rejects_a_real_match() {
        let q = Query::parse("cargo");
        for name in ["Cargo.toml", "cargo.lock", "CaRgO", "x-cargo-y"] {
            let (b, m) = sigil(name.as_bytes());
            assert!(!q.rejects_all(b, m), "{name} wrongly prefiltered out");
        }
        let (b, m) = sigil(b"readme.md");
        assert!(q.rejects_all(b, m));
    }

    #[test]
    fn empty_query_matches_everything() {
        let q = Query::parse("   ");
        assert!(q.empty);
        assert_eq!(s("anything.txt", "").0, 1);
    }
}
