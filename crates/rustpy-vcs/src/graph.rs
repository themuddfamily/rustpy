//! Commit graph lane assignment (roadmap §4.1 "Log graph").

/// One row of the commit graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphCommit {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    /// The full commit message.
    pub message: String,
    pub author: String,
    pub email: String,
    /// Seconds since the epoch.
    pub time: i64,
    /// Full ids of the parents (first parent first).
    pub parents: Vec<String>,
    /// Branch / tag names pointing at this commit.
    pub refs: Vec<String>,
    /// Lane (column) of the commit dot in this row.
    pub lane: usize,
    /// Edges to draw between this row and the next: `(lane in this row, lane in the next row)`.
    pub lanes_in: Vec<(usize, usize)>,
    /// Number of lanes needed to draw this row (dot plus pass-through lines).
    pub lane_count: usize,
}

impl GraphCommit {
    /// The row's commit as a [`crate::CommitInfo`] (Log list, details pane).
    pub fn to_info(&self) -> crate::CommitInfo {
        crate::CommitInfo {
            id: self.id.clone(),
            short_id: self.short_id.clone(),
            summary: self.summary.clone(),
            message: self.message.clone(),
            author: self.author.clone(),
            email: self.email.clone(),
            time: self.time,
            parents: self.parents.clone(),
            refs: self.refs.clone(),
        }
    }
}

/// Assigns lanes and edges to `commits`, which must be in topological order
/// (newest first, every commit before its parents).
///
/// Algorithm: a list of slots holds the commit expected next in each lane.
/// A commit is drawn in the first slot expecting it (or a free slot for a
/// branch tip); its first parent continues in the same lane, other parents
/// get their own lanes. Lanes converge when several slots expect the same
/// commit, and a lane that ends because its first parent already has a lane
/// to the right pulls that parent leftwards, like `git log --graph`.
pub(crate) fn assign_lanes(commits: &mut [GraphCommit]) {
    let mut slots: Vec<Option<String>> = Vec::new();
    let n = commits.len();
    for r in 0..n {
        let id = commits[r].id.clone();
        let lane = match slots.iter().position(|s| s.as_deref() == Some(id.as_str())) {
            Some(i) => i,
            None => {
                let i = slots.iter().position(Option::is_none).unwrap_or_else(|| {
                    slots.push(None);
                    slots.len() - 1
                });
                slots[i] = Some(id.clone());
                i
            }
        };
        // Lanes converging into this commit end here.
        for (j, s) in slots.iter_mut().enumerate() {
            if j != lane && s.as_deref() == Some(id.as_str()) {
                *s = None;
            }
        }
        // The dot consumes its slot; parents re-populate it.
        slots[lane] = None;
        let before: Vec<Option<String>> = slots.clone();
        let parents = commits[r].parents.clone();
        let mut parent_slots: Vec<usize> = Vec::with_capacity(parents.len());
        for (pi, p) in parents.iter().enumerate() {
            if let Some(k) = slots.iter().position(|s| s.as_deref() == Some(p.as_str())) {
                if pi == 0 && k > lane && slots[lane].is_none() {
                    // Pull the first parent into our lane (collapse to the left).
                    slots[k] = None;
                    slots[lane] = Some(p.clone());
                    parent_slots.push(lane);
                } else {
                    parent_slots.push(k);
                }
                continue;
            }
            let k = if pi == 0 && slots[lane].is_none() {
                lane
            } else {
                slots
                    .iter()
                    .enumerate()
                    .position(|(j, s)| s.is_none() && j != lane)
                    .unwrap_or_else(|| {
                        slots.push(None);
                        slots.len() - 1
                    })
            };
            slots[k] = Some(p.clone());
            parent_slots.push(k);
        }
        while slots.last().is_some_and(Option::is_none) {
            slots.pop();
        }
        // Edges to the next row.
        let next_id = commits.get(r + 1).map(|c| c.id.clone());
        let to_lane = |x: &str, slot: usize| -> usize {
            if next_id.as_deref() == Some(x) {
                slots
                    .iter()
                    .position(|s| s.as_deref() == Some(x))
                    .unwrap_or(slot)
            } else {
                slot
            }
        };
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for (pi, p) in parents.iter().enumerate() {
            let k = parent_slots[pi];
            edges.push((lane, to_lane(p, k)));
        }
        for (j, s) in before.iter().enumerate() {
            if let Some(x) = s {
                if j == lane {
                    continue;
                }
                // Lane continues below; the first parent may have moved it to our lane.
                let cur = slots
                    .iter()
                    .position(|t| t.as_deref() == Some(x.as_str()))
                    .unwrap_or(j);
                edges.push((j, to_lane(x, cur)));
            }
        }
        edges.sort_unstable();
        edges.dedup();
        let mut lane_count = slots.len().max(lane + 1);
        for (a, b) in &edges {
            lane_count = lane_count.max(a + 1).max(b + 1);
        }
        commits[r].lane = lane;
        commits[r].lanes_in = edges;
        commits[r].lane_count = lane_count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, parents: &[&str]) -> GraphCommit {
        GraphCommit {
            id: id.into(),
            short_id: id.into(),
            summary: id.into(),
            message: id.into(),
            author: String::new(),
            email: String::new(),
            time: 0,
            parents: parents.iter().map(|p| p.to_string()).collect(),
            refs: Vec::new(),
            lane: 0,
            lanes_in: Vec::new(),
            lane_count: 0,
        }
    }

    #[test]
    fn linear_history_uses_one_lane() {
        let mut v = vec![c("c3", &["c2"]), c("c2", &["c1"]), c("c1", &[])];
        assign_lanes(&mut v);
        assert!(v.iter().all(|x| x.lane == 0 && x.lane_count == 1));
        assert_eq!(v[0].lanes_in, vec![(0, 0)]);
        assert_eq!(v[1].lanes_in, vec![(0, 0)]);
        assert!(v[2].lanes_in.is_empty());
    }

    #[test]
    fn branch_and_merge_lanes() {
        // m merges c3 (feature) into c2 (main); both descend from c1.
        let mut v = vec![
            c("m", &["c2", "c3"]),
            c("c3", &["c1"]),
            c("c2", &["c1"]),
            c("c1", &[]),
        ];
        assign_lanes(&mut v);
        assert_eq!(v[0].lane, 0);
        assert_eq!(v[0].lanes_in, vec![(0, 0), (0, 1)]);
        assert_eq!(v[0].lane_count, 2);
        assert_eq!(v[1].lane, 1); // c3 on the side lane
        assert_eq!(v[1].lanes_in, vec![(0, 0), (1, 1)]);
        assert_eq!(v[2].lane, 0); // c2 on the main lane
        assert_eq!(v[2].lanes_in, vec![(0, 0), (1, 0)]); // both converge into c1
        assert_eq!(v[3].lane, 0);
        assert!(v[3].lanes_in.is_empty());
    }

    #[test]
    fn two_tips_without_merge() {
        let mut v = vec![c("b2", &["a1"]), c("a2", &["a1"]), c("a1", &[])];
        assign_lanes(&mut v);
        assert_eq!(v[0].lane, 0);
        assert_eq!(v[1].lane, 1);
        assert_eq!(v[1].lanes_in, vec![(0, 0), (1, 0)]);
        assert_eq!(v[2].lane, 0);
    }
}
