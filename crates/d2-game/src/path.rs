//! Finding a way between two subtiles around walls.
//!
//! The engine's own path finders (`0x00649970` and the `PATH_*` family) are not ported. This is
//! a bounded A* over the collision map — eight directions, no cutting a wall's corner — good
//! enough for a monster to walk around a tree to reach a player. The client finds its own path
//! to the point a walk packet names, so the two only need to agree on where a walk ends.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// Collision bit that stops a walker.
pub const WALL: u8 = 0x01;

/// `max + min / 2` of the axis distances: the engine's subtile distance (`0x006417F0`).
#[must_use]
pub fn distance(a: (i32, i32), b: (i32, i32)) -> i32 {
    let (dx, dy) = ((a.0 - b.0).abs(), (a.1 - b.1).abs());
    dx.max(dy) + dx.min(dy) / 2
}

/// A path from `from` to a subtile within `reach` of `goal`, as the subtiles walked through after
/// `from`; empty when `from` is already there, `None` when no way is found within `limit`
/// subtiles of either end or `max_nodes` expansions. `open` says whether a walker can stand on a
/// subtile.
#[must_use]
pub fn find(from: (i32, i32), goal: (i32, i32), reach: i32, limit: i32, max_nodes: usize, open: &dyn Fn(i32, i32) -> bool) -> Option<Vec<(i32, i32)>> {
    if distance(from, goal) <= reach {
        return Some(Vec::new());
    }
    let (left, right) = (from.0.min(goal.0) - limit, from.0.max(goal.0) + limit);
    let (top, bottom) = (from.1.min(goal.1) - limit, from.1.max(goal.1) + limit);
    let inside = |(x, y): (i32, i32)| x >= left && x <= right && y >= top && y <= bottom;
    let heuristic = |p: (i32, i32)| (distance(p, goal) - reach).max(0) * 10;
    let mut best: HashMap<(i32, i32), (i32, (i32, i32))> = HashMap::new();
    let mut queue = BinaryHeap::new();
    best.insert(from, (0, from));
    queue.push(Reverse((heuristic(from), 0, from)));
    let mut expanded = 0;
    while let Some(Reverse((_, cost, at))) = queue.pop() {
        if best.get(&at).is_some_and(|&(c, _)| c < cost) {
            continue;
        }
        if distance(at, goal) <= reach {
            let mut path = vec![at];
            let mut step = at;
            while let Some(&(_, previous)) = best.get(&step) {
                if previous == from {
                    break;
                }
                path.push(previous);
                step = previous;
            }
            path.reverse();
            return Some(path);
        }
        expanded += 1;
        if expanded > max_nodes {
            return None;
        }
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let next = (at.0 + dx, at.1 + dy);
            if !inside(next) || !open(next.0, next.1) {
                continue;
            }
            if dx != 0 && dy != 0 && (!open(at.0 + dx, at.1) || !open(at.0, at.1 + dy)) {
                continue; // no squeezing past a wall's corner
            }
            let next_cost = cost + if dx != 0 && dy != 0 { 14 } else { 10 };
            if best.get(&next).map_or(true, |&(c, _)| next_cost < c) {
                best.insert(next, (next_cost, at));
                queue.push(Reverse((next_cost + heuristic(next), next_cost, next)));
            }
        }
    }
    None
}

/// Whether every subtile on the straight line from `a` to `b` is open.
#[must_use]
pub fn clear_line(a: (i32, i32), b: (i32, i32), open: &dyn Fn(i32, i32) -> bool) -> bool {
    let steps = (b.0 - a.0).abs().max((b.1 - a.1).abs());
    (1..=steps).all(|i| {
        let t = f64::from(i) / f64::from(steps);
        let x = a.0 + (f64::from(b.0 - a.0) * t).round() as i32;
        let y = a.1 + (f64::from(b.1 - a.1) * t).round() as i32;
        open(x, y)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_octagonal() {
        assert_eq!(distance((0, 0), (4, 0)), 4);
        assert_eq!(distance((0, 0), (4, 4)), 6);
        assert_eq!(distance((10, 3), (7, 9)), 7);
    }

    #[test]
    fn a_path_goes_around_a_wall() {
        // A wall on x = 5 from y = -3 to y = 3 between the walker and its goal.
        let open = |x: i32, y: i32| !(x == 5 && (-3..=3).contains(&y));
        let path = find((0, 0), (10, 0), 1, 8, 5000, &open).unwrap();
        assert!(distance(*path.last().unwrap(), (10, 0)) <= 1);
        assert!(path.iter().all(|&(x, y)| open(x, y)));
        assert!(path.iter().any(|&(_, y)| y.abs() > 3), "it has to get past the wall's end");
        let mut previous = (0, 0);
        for &step in &path {
            assert!(distance(previous, step) <= 1, "one subtile at a time");
            previous = step;
        }
        assert!(!clear_line((0, 0), (10, 0), &open));
        assert!(clear_line((0, 5), (10, 5), &open));
    }

    #[test]
    fn no_path_when_walled_in_or_already_there() {
        let open = |x: i32, y: i32| x.abs() < 3 && y.abs() < 3;
        assert_eq!(find((0, 0), (1, 1), 2, 8, 5000, &open), Some(Vec::new()));
        assert_eq!(find((0, 0), (20, 0), 1, 8, 5000, &open), None);
    }
}
