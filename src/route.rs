//! Planning of a farming route over the markers of the map.
//!
//! The places to visit are put in an order that costs the least time, where the time to get from
//! one to the next is the cheaper of walking (a straight line made longer by a factor for the
//! detours, and a fixed time for changing the floor of an underground place) and, when
//! teleports are counted, teleporting to the nearest waypoint of the next place and walking from
//! it.  The result is cut into stages: each stage begins with a teleport (or, for the first one,
//! with the walk from where the player is) and walks over the places that follow it.

use std::time::{Duration, Instant};

/// How fast the player runs, metres per second.
pub const WALK_MPS: f32 = 7.0;
/// How much longer a way is than the straight line.
pub const DETOUR: f32 = 1.35;
/// What a teleport costs: opening the map, choosing, the loading.
pub const TELEPORT_SECONDS: f32 = 9.;
/// What going from the surface into an underground place, or from one floor to another, costs.
pub const FLOOR_SECONDS: f32 = 20.;
/// Places that take no longer than this to walk between belong to one group, and a group is
/// collected whole before the route goes on.
pub const GROUP_LINK_SECONDS: f32 = 30.;
/// What leaving a group costs in the planning, so that the groups are not interleaved.  It is only
/// a preference: it does not appear in the time of the route.
const GROUP_SWITCH_SECONDS: f32 = 90.;
/// More places than this are not planned.
pub const MAX_STOPS: usize = 1500;

/// A place on a map: a marker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub id: u32,
    pub label: u32,
    /// From the origin of the map.
    pub at: [f32; 2],
    /// 0 is the surface.
    pub floor: u16,
}

/// One part of the route.
#[derive(Clone, Debug)]
pub struct Stage {
    /// The waypoint the stage begins with; none when it begins by walking.
    pub teleport: Option<Spot>,
    pub stops: Vec<Spot>,
    pub seconds: f32,
    /// How far is walked, metres.
    pub metres: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Route {
    pub stages: Vec<Stage>,
    pub seconds: f32,
    pub metres: f32,
}

impl Route {
    pub fn stops(&self) -> usize {
        self.stages.iter().map(|stage| stage.stops.len()).sum()
    }
}

pub struct Params<'a> {
    /// The waypoints that may be teleported to (on the same map); none if teleports are not counted.
    pub waypoints: &'a [Spot],
    /// Where the player is, if the route starts from there.
    pub start: Option<Spot>,
}

fn metres(a: &Spot, b: &Spot) -> f32 {
    ((a.at[0] - b.at[0]).powi(2) + (a.at[1] - b.at[1]).powi(2)).sqrt() * DETOUR
}

fn walk(a: &Spot, b: &Spot) -> f32 {
    metres(a, b) / WALK_MPS + if a.floor != b.floor { FLOOR_SECONDS } else { 0. }
}

/// The cheapest waypoint to teleport to for getting to a place: its cost (the teleport and the
/// walk from it) and which one.
fn teleport_to(stop: &Spot, waypoints: &[Spot]) -> Option<(f32, usize)> {
    waypoints
        .iter()
        .enumerate()
        .map(|(index, waypoint)| (TELEPORT_SECONDS + walk(waypoint, stop), index))
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// Plans a route over the places, spending at most about `budget` on improving it.  Stops are
/// all visited; none is left out.
pub fn plan(stops: &[Spot], params: &Params, budget: Duration) -> Route {
    let n = stops.len();
    if n == 0 {
        return Route::default();
    }
    let deadline = Instant::now() + budget;
    let group = groups(stops);
    let tp: Vec<Option<(f32, usize)>> = stops.iter().map(|stop| teleport_to(stop, params.waypoints)).collect();
    // Getting to j from i: the cheaper of walking and teleporting; and to the first place.
    let mut m = vec![0f32; n * n];
    for i in 0..n {
        for j in 0..n {
            if i != j {
                let walking = walk(&stops[i], &stops[j]);
                let cost = tp[j].map_or(walking, |(cost, _)| walking.min(cost));
                m[i * n + j] = if group[i] == group[j] { cost } else { cost + GROUP_SWITCH_SECONDS };
            }
        }
    }
    let start_cost: Vec<f32> = (0..n)
        .map(|j| {
            let walking = params.start.map(|start| walk(&start, &stops[j]));
            match (walking, tp[j]) {
                (Some(walking), Some((cost, _))) => walking.min(cost),
                (Some(walking), None) => walking,
                (None, Some((cost, _))) => cost,
                (None, None) => 0.,
            }
        })
        .collect();

    let mut order = nearest_neighbour(n, &m, &start_cost);
    while Instant::now() < deadline {
        let changed = two_opt(&mut order, &m, &start_cost, deadline) | or_opt(&mut order, &m, &start_cost, deadline);
        if !changed {
            break;
        }
    }

    // Cut into stages.
    let mut route = Route::default();
    let mut previous: Option<&Spot> = None;
    for &j in &order {
        let stop = &stops[j];
        let walking = match previous {
            Some(previous) => Some(walk(previous, stop)),
            None => params.start.map(|start| walk(&start, stop)),
        };
        let teleporting = tp[j].map(|(cost, index)| (cost, params.waypoints[index]));
        let use_teleport = match (walking, teleporting) {
            (Some(walking), Some((cost, _))) => cost < walking,
            (None, Some(_)) => true,
            _ => false,
        };
        if use_teleport || route.stages.is_empty() {
            let (teleport, seconds, distance) = match teleporting {
                Some((cost, waypoint)) if use_teleport => (Some(waypoint), cost, metres(&waypoint, stop)),
                _ => (None, walking.unwrap_or(0.), previous.map_or_else(|| params.start.map_or(0., |start| metres(&start, stop)), |p| metres(p, stop))),
            };
            route.stages.push(Stage { teleport, stops: vec![*stop], seconds, metres: distance });
        } else if let Some(stage) = route.stages.last_mut() {
            stage.stops.push(*stop);
            stage.seconds += walking.unwrap_or(0.);
            stage.metres += previous.map_or(0., |p| metres(p, stop));
        }
        previous = Some(stop);
    }
    route.seconds = route.stages.iter().map(|stage| stage.seconds).sum();
    route.metres = route.stages.iter().map(|stage| stage.metres).sum();
    route
}

/// The group of each place: places near each other (a chain of them counts) are in one group.
fn groups(stops: &[Spot]) -> Vec<usize> {
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let n = stops.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in i + 1..n {
            if walk(&stops[i], &stops[j]) <= GROUP_LINK_SECONDS {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    (0..n).map(|i| root(&mut parent, i)).collect()
}

fn nearest_neighbour(n: usize, m: &[f32], start_cost: &[f32]) -> Vec<usize> {
    let mut left: Vec<usize> = (0..n).collect();
    let first = (0..n).min_by(|a, b| start_cost[*a].total_cmp(&start_cost[*b])).unwrap_or(0);
    left.retain(|index| *index != first);
    let mut order = vec![first];
    while !left.is_empty() {
        let last = order[order.len() - 1];
        let (at, _) = left
            .iter()
            .enumerate()
            .min_by(|a, b| m[last * n + *a.1].total_cmp(&m[last * n + *b.1]))
            .expect("not empty");
        order.push(left.swap_remove(at));
    }
    order
}

/// The cost of getting to `to` after `from` (none: from the start).
fn link(m: &[f32], s: &[f32], n: usize, from: Option<usize>, to: Option<usize>) -> f32 {
    match (from, to) {
        (Some(from), Some(to)) => m[from * n + to],
        (None, Some(to)) => s[to],
        _ => 0.,
    }
}

/// Reverses parts of the path while that makes it cheaper.
fn two_opt(order: &mut [usize], m: &[f32], s: &[f32], deadline: Instant) -> bool {
    let n = order.len();
    let mut changed = false;
    for i in 0..n.saturating_sub(1) {
        if Instant::now() > deadline {
            break;
        }
        let before_start = i.checked_sub(1).map(|k| order[k]);
        let (mut forward, mut backward) = (0f32, 0f32);
        for j in i + 1..n {
            forward += m[order[j - 1] * n + order[j]];
            backward += m[order[j] * n + order[j - 1]];
            let after = order.get(j + 1).copied();
            let old = link(m, s, n, before_start, Some(order[i])) + forward + link(m, s, n, Some(order[j]), after);
            let new = link(m, s, n, before_start, Some(order[j])) + backward + link(m, s, n, Some(order[i]), after);
            if new < old - 1e-3 {
                order[i..=j].reverse();
                changed = true;
                // The sums are of the old order: start over from here.
                break;
            }
        }
    }
    changed
}

/// Moves runs of one to three places elsewhere in the path while that makes it cheaper.
fn or_opt(order: &mut Vec<usize>, m: &[f32], s: &[f32], deadline: Instant) -> bool {
    let n = order.len();
    let mut changed = false;
    for length in 1..=3usize.min(n.saturating_sub(1)) {
        let mut i = 0;
        while i + length <= order.len() {
            if Instant::now() > deadline {
                return changed;
            }
            let first = order[i];
            let last = order[i + length - 1];
            let before = i.checked_sub(1).map(|k| order[k]);
            let after = order.get(i + length).copied();
            let removed = link(m, s, n, before, Some(first)) + link(m, s, n, Some(last), after);
            let joined = if before.is_none() && after.is_none() { 0. } else { link(m, s, n, before, after) };
            let rest: Vec<usize> = order[..i].iter().chain(&order[i + length..]).copied().collect();
            let mut best: Option<(f32, usize)> = None;
            for k in 0..=rest.len() {
                if k == i {
                    continue;
                }
                let left = k.checked_sub(1).map(|index| rest[index]);
                let right = rest.get(k).copied();
                let delta = joined - removed + link(m, s, n, left, Some(first)) + link(m, s, n, Some(last), right)
                    - link(m, s, n, left, right);
                if delta < -1e-3 && best.is_none_or(|(current, _)| delta < current) {
                    best = Some((delta, k));
                }
            }
            if let Some((_, k)) = best {
                let moved: Vec<usize> = order[i..i + length].to_vec();
                let mut next = rest;
                for (offset, item) in moved.into_iter().enumerate() {
                    next.insert(k + offset, item);
                }
                *order = next;
                changed = true;
            }
            i += 1;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(id: u32, x: f32, y: f32, floor: u16) -> Spot {
        Spot { id, label: 1, at: [x, y], floor }
    }

    #[test]
    fn all_the_places_are_visited_once() {
        let stops: Vec<Spot> = (0..40).map(|i| spot(i, (i * 37 % 11) as f32 * 30., (i * 13 % 7) as f32 * 30., 0)).collect();
        let route = plan(&stops, &Params { waypoints: &[], start: None }, Duration::from_millis(200));
        let mut seen: Vec<u32> = route.stages.iter().flat_map(|stage| stage.stops.iter().map(|stop| stop.id)).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..40).collect::<Vec<_>>());
        assert_eq!(route.stages.len(), 1);
        assert!(route.stages[0].teleport.is_none());
    }

    #[test]
    fn a_line_of_places_is_walked_in_order() {
        let stops: Vec<Spot> = [3., 0., 4., 1., 2.].iter().enumerate().map(|(i, x)| spot(i as u32, x * 50., 0., 0)).collect();
        let route = plan(&stops, &Params { waypoints: &[], start: None }, Duration::from_millis(100));
        let xs: Vec<f32> = route.stages[0].stops.iter().map(|stop| stop.at[0]).collect();
        assert!(xs == [0., 50., 100., 150., 200.] || xs == [200., 150., 100., 50., 0.], "{xs:?}");
    }

    #[test]
    fn far_clusters_are_reached_by_teleports() {
        let mut stops = Vec::new();
        for i in 0..5 {
            stops.push(spot(i, 100. + i as f32 * 20., 100., 0));
            stops.push(spot(10 + i, 6000. + i as f32 * 20., 100., 0));
        }
        let waypoints = [spot(100, 90., 100., 0), spot(101, 5990., 100., 0)];
        let route = plan(&stops, &Params { waypoints: &waypoints, start: None }, Duration::from_millis(200));
        assert_eq!(route.stages.len(), 2, "{route:?}");
        assert!(route.stages.iter().all(|stage| stage.teleport.is_some() && stage.stops.len() == 5));
        // Walking only is much longer.
        let walking = plan(&stops, &Params { waypoints: &[], start: None }, Duration::from_millis(200));
        assert!(walking.seconds > route.seconds * 3.);
    }

    #[test]
    fn a_walk_from_the_player_needs_no_teleport_when_near() {
        let stops = [spot(1, 120., 100., 0), spot(2, 150., 100., 0)];
        let waypoints = [spot(100, 5000., 5000., 0)];
        let start = spot(0, 100., 100., 0);
        let route = plan(&stops, &Params { waypoints: &waypoints, start: Some(start) }, Duration::from_millis(50));
        assert_eq!(route.stages.len(), 1);
        assert!(route.stages[0].teleport.is_none());
    }

    #[test]
    fn a_big_choice_is_planned_within_the_budget() {
        let mut seed = 12345u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let stops: Vec<Spot> = (0..1200).map(|i| spot(i, next() * 6000., next() * 6000., 0)).collect();
        let waypoints: Vec<Spot> = (0..300).map(|i| spot(5000 + i, next() * 6000., next() * 6000., 0)).collect();
        let started = Instant::now();
        let route = plan(&stops, &Params { waypoints: &waypoints, start: None }, Duration::from_millis(1500));
        assert!(started.elapsed() < Duration::from_secs(6));
        assert_eq!(route.stops(), 1200);
    }

    #[test]
    fn a_group_is_collected_whole_before_going_on() {
        // Four clusters of six places in a ring, given in a scrambled order.
        let centres = [(0., 0.), (600., 0.), (600., 600.), (0., 600.)];
        let mut stops = Vec::new();
        for k in 0..6u32 {
            for (c, (cx, cy)) in centres.iter().enumerate() {
                stops.push(spot(c as u32 * 10 + k, cx + (k % 3) as f32 * 40., cy + (k / 3) as f32 * 40., 0));
            }
        }
        let route = plan(&stops, &Params { waypoints: &[], start: None }, Duration::from_millis(300));
        let order: Vec<u32> = route.stages.iter().flat_map(|stage| stage.stops.iter().map(|stop| stop.id / 10)).collect();
        let changes = order.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert_eq!(changes, 3, "{order:?}");
    }

    #[test]
    fn changing_floors_costs_time() {
        let up = walk(&spot(1, 0., 0., 0), &spot(2, 0., 0., 5));
        assert_eq!(up, FLOOR_SECONDS);
    }
}
