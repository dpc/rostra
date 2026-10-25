/// Web-compatible subsequence score on already-lowercased strings.
///
/// Zero means no match (including an empty query). Consecutive matches and
/// boundaries after `_`, `-`, space or at the start receive bonuses. Callers
/// needing bounded work must bound both inputs before calling.
pub fn fuzzy_match(query: &str, text: &str) -> i32 {
    let q: Vec<char> = query.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let mut qi = 0;
    for &tc in &t {
        if qi < q.len() && tc == q[qi] {
            qi += 1;
        }
    }
    if qi < q.len() {
        return 0;
    }
    let mut score = 0;
    qi = 0;
    let mut prev_match_idx: i32 = -2;
    for (ti, &tc) in t.iter().enumerate() {
        if qi < q.len() && tc == q[qi] {
            score += 1;
            if ti as i32 == prev_match_idx + 1 {
                score += 2;
            }
            if ti == 0 || t[ti - 1] == '_' || t[ti - 1] == '-' || t[ti - 1] == ' ' {
                score += 3;
            }
            prev_match_idx = ti as i32;
            qi += 1;
        }
    }
    score
}
