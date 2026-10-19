//! Graph algorithms shared by the analyses: a min-heap entry for Dijkstra
//! and a max-flow network for minimum cuts.

use std::cmp::Ordering;
use std::collections::VecDeque;

/// Min-heap entry: (distance, node).
#[derive(PartialEq)]
pub(crate) struct HeapItem(pub f32, pub usize);

impl Eq for HeapItem {}

impl Ord for HeapItem {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0).then_with(|| self.1.cmp(&o.1))
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Max-flow network (Edmonds-Karp) over regions, for minimum cuts of
/// chokepoints.
pub(crate) struct FlowNet {
    edges: Vec<Vec<FlowEdge>>,
}

#[derive(Clone, Copy)]
struct FlowEdge {
    to: usize,
    cap: f64,
    /// Index of the reverse edge in `edges[to]`.
    rev: usize,
    choke: Option<usize>,
}

impl FlowNet {
    pub fn new(n: usize) -> Self {
        FlowNet { edges: vec![Vec::new(); n] }
    }

    /// Undirected edge with capacity `cap` both ways.
    pub fn add_edge(&mut self, a: usize, b: usize, cap: f64, choke: Option<usize>) {
        let (ia, ib) = (self.edges[a].len(), self.edges[b].len());
        self.edges[a].push(FlowEdge { to: b, cap, rev: ib, choke });
        self.edges[b].push(FlowEdge { to: a, cap, rev: ia, choke });
    }

    /// The chokepoints of a minimum cut between `s` and `t`; `None` if it
    /// is infinite.
    pub fn min_cut(&mut self, s: usize, t: usize) -> Option<Vec<usize>> {
        loop {
            // BFS for an augmenting path.
            let mut prev: Vec<Option<(usize, usize)>> = vec![None; self.edges.len()];
            let mut queue = VecDeque::from([s]);
            let mut seen = vec![false; self.edges.len()];
            seen[s] = true;
            while let Some(u) = queue.pop_front() {
                for (i, e) in self.edges[u].iter().enumerate() {
                    if e.cap > 1e-9 && !seen[e.to] {
                        seen[e.to] = true;
                        prev[e.to] = Some((u, i));
                        queue.push_back(e.to);
                    }
                }
            }
            if !seen[t] {
                // Cut: edges from the source side to the sink side.
                let mut chokes: Vec<usize> = Vec::new();
                for u in (0..self.edges.len()).filter(|&u| seen[u]) {
                    for e in &self.edges[u] {
                        if !seen[e.to]
                            && let Some(c) = e.choke
                        {
                            chokes.push(c);
                        }
                    }
                }
                chokes.sort_unstable();
                chokes.dedup();
                return Some(chokes);
            }
            // Bottleneck.
            let mut bottleneck = f64::INFINITY;
            let mut v = t;
            while let Some((u, i)) = prev[v] {
                bottleneck = bottleneck.min(self.edges[u][i].cap);
                v = u;
            }
            if !bottleneck.is_finite() {
                return None;
            }
            let mut v = t;
            while let Some((u, i)) = prev[v] {
                self.edges[u][i].cap -= bottleneck;
                let rev = self.edges[u][i].rev;
                self.edges[v][rev].cap += bottleneck;
                v = u;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_cut_prefers_narrow_chokes() {
        let mut f = FlowNet::new(4);
        // Two routes 0 -> 3; the cheapest cut takes the narrow choke on
        // each (7 and 8), not the wide entries (1 and 2).
        f.add_edge(0, 1, 100.0, Some(1));
        f.add_edge(1, 3, 5.0, Some(7));
        f.add_edge(0, 2, 100.0, Some(2));
        f.add_edge(2, 3, 50.0, Some(8));
        assert_eq!(f.min_cut(0, 3).unwrap(), vec![7, 8]);

        let mut g = FlowNet::new(3);
        g.add_edge(0, 1, 3.0, Some(4));
        g.add_edge(1, 2, f64::INFINITY, None);
        assert_eq!(g.min_cut(0, 2).unwrap(), vec![4]);

        let mut h = FlowNet::new(2);
        h.add_edge(0, 1, f64::INFINITY, None);
        assert!(h.min_cut(0, 1).is_none());
    }
}
