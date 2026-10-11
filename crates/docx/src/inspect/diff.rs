//! embed: Myers' shortest edit script, bounded by edits and by work.

/// One step of an edit script: an element of both sides, or of one only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Op {
    Same(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// Work an inspection may still do, counted in comparisons and steps, so a crafted pair of
/// documents stops the comparison on every target alike, whatever the clock says.
pub(super) struct Work {
    left: u64,
}

impl Work {
    pub(super) fn new(units: u64) -> Work {
        Work { left: units }
    }

    /// Spend `units`; false once the budget is gone.
    pub(super) fn spend(&mut self, units: u64) -> bool {
        match self.left.checked_sub(units) {
            Some(left) => {
                self.left = left;
                true
            }
            None => {
                self.left = 0;
                false
            }
        }
    }
}

/// The edit script between `n` elements and `m` elements, where `same(i, j)` says whether element
/// `i` of the first equals element `j` of the second. `None` when it needs more than `max_changes`
/// insertions and deletions, or more work than is left. The common prefix and suffix are matched
/// first, then Myers' greedy search runs on the rest: memory grows with the square of the edits,
/// never with the length.
pub(super) fn script(n: usize, m: usize, same: &dyn Fn(usize, usize) -> bool, max_changes: usize, work: &mut Work) -> Option<Vec<Op>> {
    // The suffix first: where a resolved change's edge is a white space either side could own,
    // the later one keeps it, which reads a deletion rejected before its text as one change.
    let mut suffix = 0;
    while suffix < n && suffix < m && same(n - 1 - suffix, m - 1 - suffix) {
        suffix += 1;
        if !work.spend(1) {
            return None;
        }
    }
    let mut prefix = 0;
    while prefix < n - suffix && prefix < m - suffix && same(prefix, prefix) {
        prefix += 1;
        if !work.spend(1) {
            return None;
        }
    }
    let (inner_n, inner_m) = (n - prefix - suffix, m - prefix - suffix);
    let inner = myers(inner_n, inner_m, &|i, j| same(prefix + i, prefix + j), max_changes, work)?;
    let mut ops = Vec::with_capacity(prefix + suffix + inner.len());
    ops.extend((0..prefix).map(|i| Op::Same(i, i)));
    ops.extend(inner.into_iter().map(|op| match op {
        Op::Same(i, j) => Op::Same(prefix + i, prefix + j),
        Op::Delete(i) => Op::Delete(prefix + i),
        Op::Insert(j) => Op::Insert(prefix + j),
    }));
    ops.extend((0..suffix).map(|k| Op::Same(n - suffix + k, m - suffix + k)));
    Some(ops)
}

/// A position no path reached.
const NONE: i64 = -1;

/// How diagonal `k` is entered at edit `d` from the frontier of edit `d - 1`: from diagonal `k + 1`
/// by an insertion (down) or from `k - 1` by a deletion (right), whichever reaches further and stays
/// inside the grid. Answers (came down, x before the edit), or `None` when neither can.
fn entry(previous: &[i64], d: i64, k: i64, n: i64, m: i64) -> Option<(bool, i64)> {
    let at = |diagonal: i64| -> i64 {
        let index = diagonal + d - 1;
        if diagonal < -(d - 1) || diagonal > d - 1 {
            return NONE;
        }
        usize::try_from(index).ok().and_then(|i| previous.get(i).copied()).unwrap_or(NONE)
    };
    let from_above = at(k + 1);
    let from_left = at(k - 1);
    // Down: x stays, y = x - k must stay within m. Right: x + 1 must stay within n.
    let down = (from_above != NONE && from_above - k <= m).then_some(from_above);
    let right = (from_left != NONE && from_left < n && from_left + 1 - k >= 0).then_some(from_left);
    match (down, right) {
        (Some(x_down), Some(x_left)) => Some(if x_left < x_down { (true, x_down) } else { (false, x_left) }),
        (Some(x_down), None) => Some((true, x_down)),
        (None, Some(x_left)) => Some((false, x_left)),
        (None, None) => None,
    }
}

fn myers(n: usize, m: usize, same: &dyn Fn(usize, usize) -> bool, max_changes: usize, work: &mut Work) -> Option<Vec<Op>> {
    let (n, m) = (i64::try_from(n).ok()?, i64::try_from(m).ok()?);
    let limit = i64::try_from(max_changes).ok()?;
    // trace[d][k + d]: the furthest x on diagonal k after d edits.
    let mut trace: Vec<Vec<i64>> = Vec::new();
    for d in 0..=limit {
        let width = usize::try_from(2 * d + 1).ok()?;
        if !work.spend(u64::try_from(width).ok()?) {
            return None;
        }
        let mut frontier = vec![NONE; width];
        let mut k = -d;
        while k <= d {
            let start = if d == 0 { Some((true, 0)) } else { entry(trace.last()?, d, k, n, m) };
            if let Some((down, before)) = start {
                let mut x = if d == 0 || down { before } else { before + 1 };
                let mut y = x - k;
                while x < n && y < m && same(usize::try_from(x).ok()?, usize::try_from(y).ok()?) {
                    x += 1;
                    y += 1;
                    if !work.spend(1) {
                        return None;
                    }
                }
                *frontier.get_mut(usize::try_from(k + d).ok()?)? = x;
                if x == n && y == m {
                    trace.push(frontier);
                    return backtrack(&trace, n, m);
                }
            }
            k += 2;
        }
        trace.push(frontier);
    }
    None
}

fn backtrack(trace: &[Vec<i64>], n: i64, m: i64) -> Option<Vec<Op>> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len()).rev() {
        let d_signed = i64::try_from(d).ok()?;
        let previous = trace.get(d - 1)?;
        let k = x - y;
        let (down, before) = entry(previous, d_signed, k, n, m)?;
        // Where the edit left the path, before its run of equal elements.
        let (edit_x, edit_y) = if down { (before, before - k) } else { (before + 1, before + 1 - k) };
        if x < edit_x || y < edit_y || x - edit_x != y - edit_y {
            return None;
        }
        while x > edit_x {
            x -= 1;
            y -= 1;
            ops.push(Op::Same(usize::try_from(x).ok()?, usize::try_from(y).ok()?));
        }
        if down {
            y -= 1;
            ops.push(Op::Insert(usize::try_from(y).ok()?));
        } else {
            x -= 1;
            ops.push(Op::Delete(usize::try_from(x).ok()?));
        }
    }
    if x != y || x < 0 {
        return None;
    }
    while x > 0 {
        x -= 1;
        y -= 1;
        ops.push(Op::Same(usize::try_from(x).ok()?, usize::try_from(y).ok()?));
    }
    ops.reverse();
    Some(ops)
}

/// Test helper: the script between two slices, with unbounded work.
#[cfg(test)]
pub(super) fn diff<T: PartialEq>(a: &[T], b: &[T], max_changes: usize) -> Option<Vec<Op>> {
    script(a.len(), b.len(), &|i, j| a.get(i) == b.get(j), max_changes, &mut Work::new(u64::MAX))
}
