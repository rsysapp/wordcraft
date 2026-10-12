//! embed: Myers' shortest edit script, bounded by edits and by work.

/// One step of an edit script: a run of elements both sides share (`len` of them from `left` and
/// `right`), or an element of one side only. Runs keep a script's memory with its edits, not with
/// the length of what matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Same { left: usize, right: usize, len: usize },
    Delete(usize),
    Insert(usize),
}

/// One step with runs taken apart, for tests.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Op {
    Same(usize, usize),
    Delete(usize),
    Insert(usize),
}

use super::budget::{Budget, units};

/// The edit script between `n` elements and `m` elements, where `same(i, j)` says whether element
/// `i` of the first equals element `j` of the second. `None` when it needs more than `max_changes`
/// insertions and deletions, or more work than is left. The common prefix and suffix are matched
/// first, then Myers' greedy search runs on the rest: memory grows with the square of the edits,
/// never with the length.
pub(crate) fn script(n: usize, m: usize, same: &dyn Fn(usize, usize) -> bool, max_changes: usize, work: &mut Budget) -> Option<Vec<Step>> {
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
    // embed: what is left on one side only is the script itself; the search would find it after a
    // number of rows that grows with its square (a rejected paragraph-long insertion).
    let inner = if inner_n == 0 || inner_m == 0 {
        if inner_n + inner_m > max_changes || !work.spend(units(inner_n + inner_m)) {
            return None;
        }
        (0..inner_n).map(Step::Delete).chain((0..inner_m).map(Step::Insert)).collect()
    } else {
        myers(inner_n, inner_m, &|i, j| same(prefix + i, prefix + j), max_changes, work)?
    };
    let mut steps = Vec::with_capacity(inner.len() + 2);
    if prefix > 0 {
        steps.push(Step::Same { left: 0, right: 0, len: prefix });
    }
    steps.extend(inner.into_iter().map(|step| match step {
        Step::Same { left, right, len } => Step::Same { left: prefix + left, right: prefix + right, len },
        Step::Delete(i) => Step::Delete(prefix + i),
        Step::Insert(j) => Step::Insert(prefix + j),
    }));
    if suffix > 0 {
        steps.push(Step::Same { left: n - suffix, right: m - suffix, len: suffix });
    }
    Some(steps)
}

/// Memory charged for each row of the trace besides its positions: the row vector's header in the
/// trace, three times over (the trace grows by doubling, so old and new room meet while it moves).
const ROW_BYTES: u64 = 3 * std::mem::size_of::<Vec<i64>>() as u64;

/// The memory an edit script of a diff bounded by `max_changes` between `n` and `m` elements
/// can take: each edit and each run of matches between them, twice (the walk back's steps, made to
/// size, and their copy with the common prefix and suffix).
pub(crate) fn script_bytes(n: usize, m: usize, max_changes: usize) -> u64 {
    let steps = units(max_changes.min(n.saturating_add(m))).saturating_mul(2).saturating_add(3);
    steps.saturating_mul(2 * std::mem::size_of::<Step>() as u64)
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

fn myers(n: usize, m: usize, same: &dyn Fn(usize, usize) -> bool, max_changes: usize, work: &mut Budget) -> Option<Vec<Step>> {
    let (n, m) = (i64::try_from(n).ok()?, i64::try_from(m).ok()?);
    let limit = i64::try_from(max_changes).ok()?;
    // trace[d][k + d]: the furthest x on diagonal k after d edits.
    let mut trace: Vec<Vec<i64>> = Vec::new();
    let mut held: u64 = 0;
    let result = search(n, m, same, limit, work, &mut trace, &mut held);
    work.release(held);
    result
}

fn search(
    n: i64,
    m: i64,
    same: &dyn Fn(usize, usize) -> bool,
    limit: i64,
    work: &mut Budget,
    trace: &mut Vec<Vec<i64>>,
    held: &mut u64,
) -> Option<Vec<Step>> {
    for d in 0..=limit {
        let width = usize::try_from(2 * d + 1).ok()?;
        // The frontier is kept for the walk back: held, and worked, before it is allocated.
        // A row: its x positions and its vector's header in the trace.
        let row = units(width).saturating_mul(8).saturating_add(ROW_BYTES);
        if !work.spend(units(width)) || !work.try_hold(row) {
            return None;
        }
        *held = held.saturating_add(row);
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
                    return backtrack(trace, n, m);
                }
            }
            k += 2;
        }
        trace.push(frontier);
    }
    None
}

fn backtrack(trace: &[Vec<i64>], n: i64, m: i64) -> Option<Vec<Step>> {
    // An edit and the run before it per row, and the run before the first edit: made to size, so
    // the steps never hold more than `script_bytes` charged for them.
    let mut steps = Vec::with_capacity(trace.len().saturating_mul(2).saturating_add(1));
    let run = |x: i64, y: i64, len: i64| -> Option<Option<Step>> {
        Some((len > 0).then_some(Step::Same { left: usize::try_from(x).ok()?, right: usize::try_from(y).ok()?, len: usize::try_from(len).ok()? }))
    };
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
        steps.extend(run(edit_x, edit_y, x - edit_x)?);
        (x, y) = (edit_x, edit_y);
        if down {
            y -= 1;
            steps.push(Step::Insert(usize::try_from(y).ok()?));
        } else {
            x -= 1;
            steps.push(Step::Delete(usize::try_from(x).ok()?));
        }
    }
    if x != y || x < 0 {
        return None;
    }
    steps.extend(run(0, 0, x)?);
    steps.reverse();
    Some(steps)
}

/// Test helper: the script between two slices, with unbounded work.
#[cfg(test)]
pub(super) fn diff<T: PartialEq>(a: &[T], b: &[T], max_changes: usize) -> Option<Vec<Op>> {
    let steps = script(a.len(), b.len(), &|i, j| a.get(i) == b.get(j), max_changes, &mut Budget::new(u64::MAX, u64::MAX))?;
    Some(
        steps
            .into_iter()
            .flat_map(|step| match step {
                Step::Same { left, right, len } => (0..len).map(|k| Op::Same(left + k, right + k)).collect::<Vec<_>>(),
                Step::Delete(i) => vec![Op::Delete(i)],
                Step::Insert(j) => vec![Op::Insert(j)],
            })
            .collect(),
    )
}
