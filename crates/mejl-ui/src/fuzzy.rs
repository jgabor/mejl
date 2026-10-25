//! A small fuzzy matcher for the palette and pickers.
//!
//! nucleo-matcher would be the obvious choice, but it is MPL-2.0 and
//! mejl only takes permissive licences. Pickers hold hundreds of items at
//! most, so a plain scorer is fast enough: each word of the pattern must
//! appear in order (case-insensitive), and matches score higher at word
//! starts, in runs and near the start of the text. A literal substring
//! always beats a scattered match.

/// A match: higher scores are better, and `positions` are the char
/// indices of the matched characters, sorted, for highlighting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Higher is better.
    pub score: i32,
    /// Matched char positions in the text.
    pub positions: Vec<usize>,
}

fn is_boundary(prev: Option<char>, c: char) -> bool {
    match prev {
        None => true,
        Some(p) => {
            matches!(p, ' ' | '-' | '_' | '/' | ':' | '.' | '@' | '<' | '(' | ',')
                || (p.is_lowercase() && c.is_uppercase())
        }
    }
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Scores one pattern word against the text.
fn word(pattern: &[char], text: &[char]) -> Option<Match> {
    if pattern.is_empty() {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    }
    let lowered: Vec<char> = text.iter().map(|c| lower(*c)).collect();
    // A literal substring, preferring one that starts at a word boundary.
    let n = pattern.len();
    let mut best_sub: Option<(i32, usize)> = None;
    if n <= lowered.len() {
        for start in 0..=lowered.len() - n {
            if lowered[start..start + n] == *pattern {
                let boundary = is_boundary(start.checked_sub(1).map(|i| text[i]), text[start]);
                let whole = text.get(start + n).is_none_or(|c| !c.is_alphanumeric());
                let score = 100
                    + 16 * n as i32
                    + if boundary { 40 } else { 0 }
                    + if boundary && whole { 20 } else { 0 }
                    - (start as i32).min(30);
                if best_sub.is_none_or(|(s, _)| score > s) {
                    best_sub = Some((score, start));
                }
            }
        }
    }
    if let Some((score, start)) = best_sub {
        return Some(Match {
            score,
            positions: (start..start + n).collect(),
        });
    }
    // A scattered subsequence, greedy from the left.
    let mut positions = Vec::with_capacity(n);
    let mut score = 0i32;
    let mut pi = 0;
    let mut last: Option<usize> = None;
    for (i, &c) in lowered.iter().enumerate() {
        if pi == n {
            break;
        }
        if c != pattern[pi] {
            continue;
        }
        let boundary = is_boundary(i.checked_sub(1).map(|j| text[j]), text[i]);
        let follows = last.is_some_and(|l| l + 1 == i);
        // A scattered match counts only at word starts or in a run, so
        // "new" finds "New rule" and "Nia Newell" but not "Snooze (web".
        if !boundary && !follows {
            continue;
        }
        let mut s = 16;
        if boundary {
            s += 24;
        }
        match last {
            Some(l) if l + 1 == i => s += 12,
            Some(l) => s -= ((i - l - 1) as i32).min(8),
            None => s -= (i as i32).min(12),
        }
        score += s;
        positions.push(i);
        last = Some(i);
        pi += 1;
    }
    (pi == n).then_some(Match { score, positions })
}

/// Scores `pattern` against `text`. Every whitespace-separated word of the
/// pattern must match. An empty pattern matches everything with score 0.
pub fn score(pattern: &str, text: &str) -> Option<Match> {
    let text: Vec<char> = text.chars().collect();
    let mut total = Match {
        score: 0,
        positions: Vec::new(),
    };
    for w in pattern.split_whitespace() {
        let p: Vec<char> = w.chars().map(lower).collect();
        let m = word(&p, &text)?;
        total.score += m.score;
        total.positions.extend(m.positions);
    }
    total.positions.sort_unstable();
    total.positions.dedup();
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_matches_everything() {
        assert_eq!(score("", "anything").map(|m| m.score), Some(0));
        assert_eq!(score("  ", "").map(|m| m.score), Some(0));
    }

    #[test]
    fn subsequence_in_order_only() {
        assert!(score("arc", "Archive").is_some());
        assert!(score("ar", "Archive").is_some());
        assert!(score("ma", "Move to a mailbox").is_some(), "word starts");
        assert!(
            score("new", "Snooze (web only)").is_none(),
            "not scattered inside words"
        );
        assert!(score("hca", "Archive").is_none());
        assert!(score("ave", "Archive").is_none());
        assert!(score("zz", "Archive").is_none());
        assert!(score("ä", "Ärger").is_some(), "case folding is Unicode");
    }

    #[test]
    fn better_matches_rank_higher() {
        let s = |p: &str, t: &str| score(p, t).map_or(i32::MIN, |m| m.score);
        assert!(s("new", "Compose: new message") > s("new", "Go to: Newsletters ninja"));
        assert!(s("arch", "Archive") > s("arch", "Search mail by recipient"));
        assert!(s("gi", "Go inbox") > s("gi", "Toggle signature"));
        assert!(s("inbox", "Inbox") > s("inbox", "Go to the inbox"));
    }

    #[test]
    fn words_match_independently() {
        let m = score("mes new", "Compose: new message").unwrap();
        assert!(m.positions.contains(&9));
        assert!(score("new zzz", "Compose: new message").is_none());
        let m = score("NEW", "Compose: new message").unwrap();
        assert_eq!(m.positions, vec![9, 10, 11]);
    }
}
