//! A faithful port of Python's `difflib.SequenceMatcher` (ratio path),
//! including the `autojunk` heuristic. Scrapling's adaptive scoring depends
//! on bit-exact ratios, so this mirrors CPython's implementation step by
//! step: chain_b with popular-element removal, find_longest_match with the
//! junk-extension passes, and get_matching_blocks with the adjacency
//! collapse.
//!
//! Performance notes (results are unchanged): the matcher borrows its
//! sequences (no element clones - b2j keys are references), scratch
//! buffers are allocated once per matcher and kept on the stack for
//! small inputs via SmallVec, the per-row `j2len` dict is kept in flat
//! buffers indexed by position in `b`, the b2j index uses a linear list
//! for small inputs, identical sequences short-circuit in
//! get_matching_blocks (CPython's algorithm yields one full-length
//! block there too), and short string ratios use a dedicated
//! zero-allocation matcher (tiny_ratio).

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use smallvec::SmallVec;

/// Index of b's elements. A linear list for small inputs (the common
/// case in adaptive scoring: attribute values, tag paths), a hash map
/// for large ones. Lookup results are identical either way.
enum B2J<'a, T> {
    Linear(SmallVec<[(&'a T, SmallVec<[usize; 16]>); 8]>),
    Hashed(HashMap<&'a T, Vec<usize>>),
}

impl<'a, T: Eq + Hash> B2J<'a, T> {
    fn get(&self, key: &T) -> Option<&[usize]> {
        match self {
            B2J::Linear(entries) => entries
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.as_slice()),
            B2J::Hashed(map) => map.get(key).map(|v| v.as_slice()),
        }
    }
}

pub struct SequenceMatcher<'a, T> {
    a: &'a [T],
    b: &'a [T],
    b2j: B2J<'a, T>,
    /// The autojunk-popular elements purged from b2j (CPython's
    /// `bpopular`; the match logic itself never reads it, exactly like
    /// CPython, where find_longest_match consults `bjunk`, not
    /// `bpopular`).
    #[allow(dead_code)]
    isbpopular: HashSet<&'a T>,
    matching_blocks: Option<Vec<(usize, usize, usize)>>,
    // Scratch for find_longest_match, allocated once. Two row buffers
    // and two touched lists, ping-ponged by swapping the references
    // (never the buffers themselves: a SmallVec with a 64-element
    // inline store costs hundreds of bytes per value swap).
    scratch_rows: [SmallVec<[usize; 64]>; 2],
    scratch_touched: [SmallVec<[usize; 64]>; 2],
}

impl<'a, T: Eq + Hash> SequenceMatcher<'a, T> {
    pub fn new(a: &'a [T], b: &'a [T]) -> Self {
        Self::with_autojunk(a, b, true)
    }

    pub fn with_autojunk(a: &'a [T], b: &'a [T], autojunk: bool) -> Self {
        let mut isbpopular = HashSet::new();
        let purge_test = if autojunk && b.len() >= 200 {
            Some(b.len() / 100 + 1)
        } else {
            None
        };
        let blen = b.len();
        let mut matcher = SequenceMatcher {
            a,
            b,
            b2j: B2J::Linear(SmallVec::new()),
            isbpopular: HashSet::new(),
            matching_blocks: None,
            scratch_rows: [SmallVec::from_elem(0, blen), SmallVec::from_elem(0, blen)],
            scratch_touched: [SmallVec::new(), SmallVec::new()],
        };
        // Small inputs: build the linear index directly (no hash map
        // round-trip). The purge, when active, drops over-popular keys.
        if b.len() <= 256 {
            let mut entries: SmallVec<[(&T, SmallVec<[usize; 16]>); 8]> = SmallVec::new();
            for (i, elt) in b.iter().enumerate() {
                match entries.iter_mut().find(|(k, _)| *k == elt) {
                    Some((_, idxs)) => idxs.push(i),
                    None => {
                        let mut idxs = SmallVec::new();
                        idxs.push(i);
                        entries.push((elt, idxs));
                    }
                }
            }
            if let Some(test) = purge_test {
                entries.retain(|(elt, idxs)| {
                    if idxs.len() > test {
                        isbpopular.insert(*elt);
                        false
                    } else {
                        true
                    }
                });
            }
            matcher.b2j = B2J::Linear(entries);
            matcher.isbpopular = isbpopular;
            return matcher;
        }
        let mut map: HashMap<&T, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            map.entry(elt).or_default().push(i);
        }
        if let Some(test) = purge_test {
            let popular: Vec<&T> = map
                .iter()
                .filter(|(_, idxs)| idxs.len() > test)
                .map(|(elt, _)| *elt)
                .collect();
            for elt in popular {
                isbpopular.insert(elt);
                map.remove(elt);
            }
        }
        matcher.b2j = if map.len() <= 64 {
            B2J::Linear(map.into_iter().map(|(k, v)| (k, SmallVec::from_vec(v))).collect())
        } else {
            B2J::Hashed(map)
        };
        matcher.isbpopular = isbpopular;
        matcher
    }

    /// CPython's find_longest_match tests `bjunk` (elements rejected by
    /// the user-supplied isjunk function), NOT the autojunk-popular set:
    /// popular elements are only purged from b2j, and the extension
    /// passes walk across them by equality. Scrapling always passes
    /// isjunk=None, so bjunk is empty here.
    fn isbjunk(&self, _elt: &T) -> bool {
        false
    }

    /// (a_start, b_start, size) of the longest matching block in the given
    /// ranges, exactly like CPython's find_longest_match.
    fn find_longest_match(
        &mut self,
        alo: usize,
        ahi: usize,
        blo: usize,
        bhi: usize,
    ) -> (usize, usize, usize) {
        // CPython starts each call with an empty j2len; the scratch
        // buffers are reused across calls, so reset whatever the
        // previous call left in them (either buffer may hold its final
        // row, depending on the row parity when it returned).
        for idx in 0..2 {
            let touched = std::mem::take(&mut self.scratch_touched[idx]);
            for &j in &touched {
                self.scratch_rows[idx][j] = 0;
            }
        }
        let (mut besta, mut bestb, mut bestsize) = (alo, blo, 0usize);
        // Flat-buffer version of CPython's j2len dicts: cur[j] holds the
        // value for the previous row, entries not touched in that row are
        // zero, maintained via the touched lists. The two buffers are
        // ping-ponged by swapping the local references.
        let (rows_l, rows_r) = self.scratch_rows.split_at_mut(1);
        let (tch_l, tch_r) = self.scratch_touched.split_at_mut(1);
        let (mut cur, mut nxt): (&mut SmallVec<[usize; 64]>, &mut SmallVec<[usize; 64]>) =
            (&mut rows_l[0], &mut rows_r[0]);
        let (mut tcur, mut tnxt): (&mut SmallVec<[usize; 64]>, &mut SmallVec<[usize; 64]>) =
            (&mut tch_l[0], &mut tch_r[0]);
        for i in alo..ahi {
            tnxt.clear();
            if let Some(indices) = self.b2j.get(&self.a[i]) {
                for &j in indices {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = if j == 0 { 1 } else { cur[j - 1] + 1 };
                    nxt[j] = k;
                    tnxt.push(j);
                    if k > bestsize {
                        besta = i + 1 - k;
                        bestb = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            for t in 0..tcur.len() {
                let j = tcur[t];
                cur[j] = 0;
            }
            std::mem::swap(&mut cur, &mut nxt);
            std::mem::swap(&mut tcur, &mut tnxt);
        }
        // Extend the best block by non-junk elements on each end.
        while besta > alo
            && bestb > blo
            && !self.isbjunk(&self.b[bestb - 1])
            && self.a[besta - 1] == self.b[bestb - 1]
        {
            besta -= 1;
            bestb -= 1;
            bestsize += 1;
        }
        while besta + bestsize < ahi
            && bestb + bestsize < bhi
            && !self.isbjunk(&self.b[bestb + bestsize])
            && self.a[besta + bestsize] == self.b[bestb + bestsize]
        {
            bestsize += 1;
        }
        // Junk extension passes (only have an effect when a user isjunk
        // function marks elements; never with isjunk=None).
        while besta > alo
            && bestb > blo
            && self.isbjunk(&self.b[bestb - 1])
            && self.a[besta - 1] == self.b[bestb - 1]
        {
            besta -= 1;
            bestb -= 1;
            bestsize += 1;
        }
        while besta + bestsize < ahi
            && bestb + bestsize < bhi
            && self.isbjunk(&self.b[bestb + bestsize])
            && self.a[besta + bestsize] == self.b[bestb + bestsize]
        {
            bestsize += 1;
        }
        (besta, bestb, bestsize)
    }

    pub fn matching_blocks(&mut self) -> &[(usize, usize, usize)] {
        if self.matching_blocks.is_none() {
            if self.a == self.b {
                // Identical sequences: CPython's algorithm produces one
                // full-length block; take it directly.
                let n = self.a.len();
                self.matching_blocks = Some(vec![(0, 0, n), (n, n, 0)]);
                return self.matching_blocks.as_deref().unwrap();
            }
            let mut blocks = Vec::new();
            let mut queue = vec![(0, self.a.len(), 0, self.b.len())];
            while let Some((alo, ahi, blo, bhi)) = queue.pop() {
                let (x, y, size) = self.find_longest_match(alo, ahi, blo, bhi);
                if size > 0 {
                    blocks.push((x, y, size));
                    if alo < x && blo < y {
                        queue.push((alo, x, blo, y));
                    }
                    if x + size < ahi && y + size < bhi {
                        queue.push((x + size, ahi, y + size, bhi));
                    }
                }
            }
            blocks.sort();
            // Collapse adjacent equal blocks.
            let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
            let mut non_adjacent = Vec::new();
            for (ai, bj, size) in blocks {
                if i1 + k1 == ai && j1 + k1 == bj {
                    k1 += size;
                } else {
                    if k1 > 0 {
                        non_adjacent.push((i1, j1, k1));
                    }
                    i1 = ai;
                    j1 = bj;
                    k1 = size;
                }
            }
            if k1 > 0 {
                non_adjacent.push((i1, j1, k1));
            }
            non_adjacent.push((self.a.len(), self.b.len(), 0));
            self.matching_blocks = Some(non_adjacent);
        }
        self.matching_blocks.as_deref().unwrap()
    }

    pub fn ratio(&mut self) -> f64 {
        let matches: usize = self.matching_blocks().iter().map(|b| b.2).sum();
        let length = self.a.len() + self.b.len();
        if length == 0 {
            1.0
        } else {
            2.0 * matches as f64 / length as f64
        }
    }
}

/// Ratio of two sequences, skipping the matcher construction entirely
/// when they are identical (CPython yields one full-length block there).
pub fn seq_ratio<T: Eq + Hash>(a: &[T], b: &[T]) -> f64 {
    if a == b {
        return 1.0;
    }
    SequenceMatcher::new(a, b).ratio()
}

/// Zero-allocation SequenceMatcher ratio for short char slices
/// (autojunk can never trigger below 200 elements, and isjunk is always
/// None, so only the non-junk extension passes can fire). Mirrors
/// find_longest_match + get_matching_blocks exactly; the unit tests
/// assert it agrees with the generic matcher on thousands of cases.
fn tiny_ratio(a: &[char], b: &[char]) -> f64 {
    const MAX: usize = 64;
    debug_assert!(a.len() <= MAX && b.len() <= MAX);
    if a == b {
        return 1.0;
    }
    // b2j: distinct chars of b (first-appearance order) + positions.
    let mut bkeys = ['\0'; MAX];
    let mut bpos = [0usize; MAX];
    let mut bstart = [0usize; MAX + 1];
    let mut nkeys = 0usize;
    {
        // Count occurrences per distinct key, lay positions out grouped.
        let mut counts = [0usize; MAX];
        for &ch in b {
            match (0..nkeys).find(|&i| bkeys[i] == ch) {
                Some(i) => counts[i] += 1,
                None => {
                    bkeys[nkeys] = ch;
                    counts[nkeys] = 1;
                    nkeys += 1;
                }
            }
        }
        let mut off = 0usize;
        for i in 0..nkeys {
            bstart[i] = off;
            off += counts[i];
            counts[i] = 0; // reuse as fill cursor
        }
        bstart[nkeys] = off;
        for (j, &ch) in b.iter().enumerate() {
            let i = (0..nkeys).find(|&i| bkeys[i] == ch).unwrap();
            bpos[bstart[i] + counts[i]] = j;
            counts[i] += 1;
        }
    }
    let find_indices = |ch: char| -> &[usize] {
        match (0..nkeys).find(|&i| bkeys[i] == ch) {
            Some(i) => &bpos[bstart[i]..bstart[i + 1]],
            None => &[],
        }
    };
    let longest = |alo: usize, ahi: usize, blo: usize, bhi: usize| -> (usize, usize, usize) {
        let (mut besta, mut bestb, mut bestsize) = (alo, blo, 0usize);
        // Ping-pong row buffers selected by index: swapping the arrays
        // themselves would copy 512 bytes per row.
        let mut bufs = [[0usize; MAX]; 2];
        let mut tch = [[0usize; MAX]; 2];
        let mut ntch = [0usize; 2];
        let mut ci = 0usize;
        for i in alo..ahi {
            let ni = 1 - ci;
            ntch[ni] = 0;
            for &j in find_indices(a[i]) {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = if j == 0 { 1 } else { bufs[ci][j - 1] + 1 };
                bufs[ni][j] = k;
                tch[ni][ntch[ni]] = j;
                ntch[ni] += 1;
                if k > bestsize {
                    besta = i + 1 - k;
                    bestb = j + 1 - k;
                    bestsize = k;
                }
            }
            for t in 0..ntch[ci] {
                let j = tch[ci][t];
                bufs[ci][j] = 0;
            }
            ci = ni;
        }
        while besta > alo && bestb > blo && a[besta - 1] == b[bestb - 1] {
            besta -= 1;
            bestb -= 1;
            bestsize += 1;
        }
        while besta + bestsize < ahi && bestb + bestsize < bhi && a[besta + bestsize] == b[bestb + bestsize] {
            bestsize += 1;
        }
        (besta, bestb, bestsize)
    };
    // get_matching_blocks with an explicit stack.
    let mut blocks = [(0usize, 0usize, 0usize); MAX + 1];
    let mut nblocks = 0usize;
    let mut stack = [(0usize, 0usize, 0usize, 0usize); 2 * MAX + 2];
    let mut nstack = 0usize;
    stack[nstack] = (0, a.len(), 0, b.len());
    nstack += 1;
    while nstack > 0 {
        nstack -= 1;
        let (alo, ahi, blo, bhi) = stack[nstack];
        let (x, y, size) = longest(alo, ahi, blo, bhi);
        if size > 0 {
            blocks[nblocks] = (x, y, size);
            nblocks += 1;
            if alo < x && blo < y {
                stack[nstack] = (alo, x, blo, y);
                nstack += 1;
            }
            if x + size < ahi && y + size < bhi {
                stack[nstack] = (x + size, ahi, y + size, bhi);
                nstack += 1;
            }
        }
    }
    blocks[..nblocks].sort_unstable();
    let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
    let mut matches = 0usize;
    for &(ai, bj, size) in &blocks[..nblocks] {
        if i1 + k1 == ai && j1 + k1 == bj {
            k1 += size;
        } else {
            matches += k1;
            i1 = ai;
            j1 = bj;
            k1 = size;
        }
    }
    matches += k1;
    let length = a.len() + b.len();
    if length == 0 {
        1.0
    } else {
        2.0 * matches as f64 / length as f64
    }
}

/// Convenience for string inputs: Python compares sequences of code points,
/// so convert to char vectors first.
pub fn str_ratio(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    // Attribute values and element texts are short; keep the char
    // sequences on the stack and use the allocation-free matcher.
    let (ca, cb) = (a.chars().count(), b.chars().count());
    if ca <= 64 && cb <= 64 {
        let mut abuf = ['\0'; 64];
        let mut bbuf = ['\0'; 64];
        for (i, c) in a.chars().enumerate() {
            abuf[i] = c;
        }
        for (i, c) in b.chars().enumerate() {
            bbuf[i] = c;
        }
        return tiny_ratio(&abuf[..ca], &bbuf[..cb]);
    }
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let mut matcher = SequenceMatcher::new(&av, &bv);
    matcher.ratio()
}

/// Python's `round(x, 2)`: round-half-even applied to the exact binary
/// value, returned as the nearest double. Implemented with exact integer
/// arithmetic on the f64 mantissa (scores live in [0, 100], ratios in
/// [0, 1], but the implementation is general for |x| < 2^48 or so);
/// the tests assert it agrees with the format-based definition.
pub fn py_round2(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let bits = x.abs().to_bits();
    if (bits >> 52) & 0x7ff == 0 {
        return 0.0; // subnormal: far below the 0.005 rounding quantum
    }
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1023;
    let mantissa = (bits & 0xf_ffff_ffff_ffff) | 0x10_0000_0000_0000; // 53-bit
    // x = mantissa * 2^(exp-52); x*100 = mantissa*25 * 2^(exp-50)
    let scaled = mantissa as u128 * 25;
    let shift = 50 - exp;
    let rounded: u128 = if shift <= 0 {
        scaled << (-shift)
    } else if shift >= 128 {
        0
    } else {
        let q = scaled >> shift;
        let rem = scaled & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        if rem > half || (rem == half && q % 2 == 1) {
            q + 1
        } else {
            q
        }
    };
    let r = (rounded as f64) / 100.0 * sign;
    if r == 0.0 {
        0.0 // normalize -0.0 like Python dict keys treat it
    } else {
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_cases() {
        assert_eq!(str_ratio("", ""), 1.0);
        assert_eq!(str_ratio("abc", "abc"), 1.0);
        assert_eq!(str_ratio("abc", "abd"), 2.0 * 2.0 / 6.0);
        assert_eq!(str_ratio("hello world", "hello"), 2.0 * 5.0 / 16.0);
        // longest-match extension path
        assert_eq!(str_ratio("abcd", "bcde"), 0.75);
    }

    #[test]
    fn seq_ratio() {
        let a = vec!["html".to_string(), "body".to_string(), "div".to_string()];
        let b = vec!["html".to_string(), "body".to_string(), "span".to_string()];
        assert_eq!(SequenceMatcher::new(&a, &b).ratio(), 2.0 * 2.0 / 6.0);
    }

    #[test]
    fn tiny_matches_generic() {
        // Deterministic pseudo-random short strings: str_ratio (tiny
        // stack matcher) must agree with the generic matcher everywhere.
        let mut state = 0x243F6A8885A308D3u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let alphabet = ['a', 'b', 'c', ' ', 'あ', '🙂', '\t', 'x'];
        for _ in 0..4000 {
            let la = (next() % 65) as usize;
            let lb = (next() % 65) as usize;
            let a: String = (0..la)
                .map(|_| alphabet[(next() % 8) as usize])
                .collect();
            let b: String = (0..lb)
                .map(|_| alphabet[(next() % 8) as usize])
                .collect();
            let av: Vec<char> = a.chars().collect();
            let bv: Vec<char> = b.chars().collect();
            let generic = SequenceMatcher::new(&av, &bv).ratio();
            assert_eq!(str_ratio(&a, &b), generic, "mismatch for {a:?} vs {b:?}");
        }
    }

    #[test]
    fn round2_half_even() {
        assert_eq!(py_round2(0.125), 0.12);
        assert_eq!(py_round2(0.375), 0.38);
        assert_eq!(py_round2(2.675), 2.67);
        assert_eq!(py_round2(-0.001), 0.0);
        assert_eq!(py_round2(82.6), 82.6);
        assert_eq!(py_round2(79.74), 79.74);
    }

    #[test]
    fn round2_matches_format_definition() {
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20000 {
            // Random doubles across the score/ratio ranges, including
            // values constructed to sit near .xx5 boundaries.
            let v = (next() % 100001) as f64 / 1000.0;
            let w = v + (next() % 3) as f64 * 0.005;
            for x in [v, w, v / 100.0, -v] {
                let via_format: f64 = format!("{x:.2}").parse().unwrap();
                let expect = if via_format == 0.0 { 0.0 } else { via_format };
                assert_eq!(py_round2(x), expect, "round mismatch for {x}");
            }
        }
    }
}
