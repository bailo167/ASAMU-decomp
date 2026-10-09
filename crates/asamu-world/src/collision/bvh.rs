//! A deterministic bounding volume hierarchy over axis-aligned boxes.
//!
//! Built by recursive median splits along the longest axis of the centroid
//! bounds; primitives are ordered by `(centroid, index)` with a total order,
//! so the tree depends only on the input (no hashing, no randomness, no
//! dependence on the sort algorithm). Leaves hold at most [`LEAF_SIZE`]
//! primitives; median splits halve the primitive count per level, so the
//! depth stays below [`MAX_DEPTH`] for any input that fits in memory.

use glam::Vec3;

/// Maximum primitives per leaf.
pub const LEAF_SIZE: usize = 4;
/// Traversal stack size; median splits keep the depth far below it.
pub const MAX_DEPTH: usize = 64;

/// An `f32` axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb3 {
    /// Minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
}

impl Aabb3 {
    /// The empty box (inverted, so any union replaces it).
    pub const EMPTY: Self = Self {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    /// Smallest box containing both.
    #[must_use]
    pub fn union(self, o: Self) -> Self {
        Self {
            min: self.min.min(o.min),
            max: self.max.max(o.max),
        }
    }

    /// `true` when `min <= max` on every axis and both corners are finite.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.min.is_finite() && self.max.is_finite() && self.min.cmple(self.max).all()
    }

    /// Box centre.
    #[must_use]
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// `true` when the boxes overlap (touching counts).
    #[must_use]
    pub fn overlaps(&self, o: &Self) -> bool {
        self.min.cmple(o.max).all() && o.min.cmple(self.max).all()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Node {
    bounds: Aabb3,
    /// Leaf: first index into `order`; inner: index of the left child (the
    /// right child follows it).
    first: u32,
    /// Leaf: primitive count (> 0); inner: 0.
    count: u32,
}

/// The hierarchy. Primitive ids are the indices of the boxes passed to
/// [`Bvh::build`]; boxes that are not [`Aabb3::is_valid`] are left out.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Bvh {
    nodes: Vec<Node>,
    order: Vec<u32>,
}

impl Bvh {
    /// Builds the hierarchy over `boxes` (primitive `i` = `boxes[i]`).
    #[must_use]
    pub fn build(boxes: &[Aabb3]) -> Self {
        let mut order: Vec<u32> = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.is_valid())
            .filter_map(|(i, _)| u32::try_from(i).ok())
            .collect();
        if order.is_empty() {
            return Self::default();
        }
        let centers: Vec<Vec3> = boxes.iter().map(Aabb3::center).collect();
        let mut nodes = Vec::with_capacity(order.len() / LEAF_SIZE * 2 + 1);
        nodes.push(Node {
            bounds: Aabb3::EMPTY,
            first: 0,
            count: 0,
        });
        // Work list of (node index, start, end) ranges into `order`.
        let mut work: Vec<(usize, usize, usize)> = vec![(0, 0, order.len())];
        while let Some((node, start, end)) = work.pop() {
            let range = &mut order[start..end];
            let mut bounds = Aabb3::EMPTY;
            let mut cbounds = Aabb3::EMPTY;
            for &i in range.iter() {
                let b = boxes[i as usize];
                bounds = bounds.union(b);
                let c = centers[i as usize];
                cbounds = cbounds.union(Aabb3 { min: c, max: c });
            }
            let extent = cbounds.max - cbounds.min;
            let n = end - start;
            let leaf = n <= LEAF_SIZE || extent.max_element() <= 0.0;
            if leaf {
                nodes[node] = Node {
                    bounds,
                    first: start as u32,
                    count: n as u32,
                };
                continue;
            }
            let axis = if extent.x >= extent.y && extent.x >= extent.z {
                0
            } else if extent.y >= extent.z {
                1
            } else {
                2
            };
            let mid = n / 2;
            range.select_nth_unstable_by(mid, |&a, &b| {
                centers[a as usize][axis]
                    .total_cmp(&centers[b as usize][axis])
                    .then(a.cmp(&b))
            });
            let left = nodes.len();
            nodes.push(Node {
                bounds: Aabb3::EMPTY,
                first: 0,
                count: 0,
            });
            nodes.push(Node {
                bounds: Aabb3::EMPTY,
                first: 0,
                count: 0,
            });
            nodes[node] = Node {
                bounds,
                first: left as u32,
                count: 0,
            };
            // Push right first so the left subtree is built first (the
            // order of construction does not change the tree).
            work.push((left + 1, start + mid, end));
            work.push((left, start, start + mid));
        }
        Self { nodes, order }
    }

    /// `true` when no primitive was accepted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Bounds of everything, if anything.
    #[must_use]
    pub fn bounds(&self) -> Option<Aabb3> {
        self.nodes.first().map(|n| n.bounds).filter(Aabb3::is_valid)
    }

    /// Number of primitives in the tree.
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Visits every primitive whose leaf box passes `test`, descending only
    /// into nodes whose box passes `test`. `visit` returns `false` to stop.
    pub fn query(&self, mut test: impl FnMut(&Aabb3) -> bool, mut visit: impl FnMut(u32) -> bool) {
        if self.order.is_empty() {
            return;
        }
        let mut stack = [0u32; MAX_DEPTH];
        let mut top = 1usize;
        while top > 0 {
            top -= 1;
            let Some(node) = self.nodes.get(stack[top] as usize) else {
                continue;
            };
            if !test(&node.bounds) {
                continue;
            }
            if node.count > 0 {
                let s = node.first as usize;
                let e = s + node.count as usize;
                for &p in self.order.get(s..e).unwrap_or(&[]) {
                    if !visit(p) {
                        return;
                    }
                }
            } else if top + 2 <= MAX_DEPTH {
                stack[top] = node.first + 1;
                stack[top + 1] = node.first;
                top += 2;
            }
        }
    }

    /// Ordered traversal for segment-like queries: `entry(box)` returns the
    /// entry parameter of the query into the box (or `None` to skip it);
    /// children are visited nearest first and nodes whose entry exceeds the
    /// current `limit()` are skipped. `visit` handles a primitive.
    pub fn query_ordered(
        &self,
        mut entry: impl FnMut(&Aabb3) -> Option<f64>,
        mut limit: impl FnMut() -> f64,
        mut visit: impl FnMut(u32),
    ) {
        let Some(root) = self.nodes.first() else {
            return;
        };
        let Some(t_root) = entry(&root.bounds) else {
            return;
        };
        let mut stack = [(0u32, 0.0f64); MAX_DEPTH];
        stack[0] = (0, t_root);
        let mut top = 1usize;
        while top > 0 {
            top -= 1;
            let (index, t_enter) = stack[top];
            if t_enter > limit() {
                continue;
            }
            let Some(node) = self.nodes.get(index as usize) else {
                continue;
            };
            if node.count > 0 {
                let s = node.first as usize;
                let e = s + node.count as usize;
                for &p in self.order.get(s..e).unwrap_or(&[]) {
                    visit(p);
                }
                continue;
            }
            let l = node.first;
            let r = node.first + 1;
            let tl = self.nodes.get(l as usize).and_then(|n| entry(&n.bounds));
            let tr = self.nodes.get(r as usize).and_then(|n| entry(&n.bounds));
            let mut push = |item: (u32, f64)| {
                if top < MAX_DEPTH {
                    stack[top] = item;
                    top += 1;
                }
            };
            match (tl, tr) {
                (Some(a), Some(b)) => {
                    // Far child first so the near one is popped next.
                    if a <= b {
                        push((r, b));
                        push((l, a));
                    } else {
                        push((l, a));
                        push((r, b));
                    }
                }
                (Some(a), None) => push((l, a)),
                (None, Some(b)) => push((r, b)),
                (None, None) => {}
            }
        }
    }
}

/// Entry parameter of the segment `origin + t·delta`, `t ∈ [0, limit]`,
/// into the box `[min − pad, max + pad]` (slab test in `f64`). `inv` is
/// `1/delta` per axis (infinite for zero components).
#[must_use]
pub fn segment_box_entry(
    origin: glam::DVec3,
    inv: glam::DVec3,
    b: &Aabb3,
    pad: glam::DVec3,
    limit: f64,
) -> Option<f64> {
    let mut t0 = 0.0f64;
    let mut t1 = limit;
    for axis in 0..3 {
        let lo = f64::from(b.min[axis]) - pad[axis];
        let hi = f64::from(b.max[axis]) + pad[axis];
        let o = origin[axis];
        let iv = inv[axis];
        if iv.is_infinite() {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let mut a = (lo - o) * iv;
        let mut c = (hi - o) * iv;
        if a > c {
            core::mem::swap(&mut a, &mut c);
        }
        if a > t0 {
            t0 = a;
        }
        if c < t1 {
            t1 = c;
        }
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

/// Per-axis reciprocal with exact zeros mapped to infinity (for
/// [`segment_box_entry`]).
#[must_use]
pub fn reciprocal(d: glam::DVec3) -> glam::DVec3 {
    let r = |v: f64| if v == 0.0 { f64::INFINITY } else { 1.0 / v };
    glam::DVec3::new(r(d.x), r(d.y), r(d.z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;

    fn boxes(n: usize) -> Vec<Aabb3> {
        // A deterministic scatter of unit boxes.
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let x = (s % 1000) as f32;
                let y = ((s >> 20) % 1000) as f32;
                let z = ((s >> 40) % 100) as f32;
                let min = Vec3::new(x, y, z);
                Aabb3 {
                    min,
                    max: min + Vec3::ONE,
                }
            })
            .collect()
    }

    #[test]
    fn query_finds_exactly_the_overlapping_boxes() {
        let b = boxes(500);
        let bvh = Bvh::build(&b);
        assert_eq!(bvh.len(), 500);
        let q = Aabb3 {
            min: Vec3::new(200.0, 300.0, 0.0),
            max: Vec3::new(400.0, 500.0, 50.0),
        };
        let mut found = Vec::new();
        bvh.query(
            |n| n.overlaps(&q),
            |p| {
                if b[p as usize].overlaps(&q) {
                    found.push(p);
                }
                true
            },
        );
        found.sort_unstable();
        let expected: Vec<u32> = (0..500u32)
            .filter(|&i| b[i as usize].overlaps(&q))
            .collect();
        assert_eq!(found, expected);
        assert!(!expected.is_empty());
    }

    #[test]
    fn build_is_deterministic_and_skips_invalid_boxes() {
        let mut b = boxes(200);
        b[3].min.x = f32::NAN;
        b[7] = Aabb3::EMPTY;
        let a = Bvh::build(&b);
        let c = Bvh::build(&b);
        assert_eq!(a, c);
        assert_eq!(a.len(), 198);
        assert!(Bvh::build(&[]).is_empty());
        assert!(Bvh::build(&[]).bounds().is_none());
        // Identical boxes (zero centroid extent) end in one leaf without recursing forever.
        let same = vec![
            Aabb3 {
                min: Vec3::ZERO,
                max: Vec3::ONE
            };
            1000
        ];
        let t = Bvh::build(&same);
        let mut n = 0;
        t.query(
            |_| true,
            |_| {
                n += 1;
                true
            },
        );
        assert_eq!(n, 1000);
    }

    #[test]
    fn ordered_query_and_slab_entry() {
        let b = Aabb3 {
            min: Vec3::splat(10.0),
            max: Vec3::splat(20.0),
        };
        let o = DVec3::ZERO;
        let d = DVec3::new(30.0, 30.0, 30.0);
        let t = segment_box_entry(o, reciprocal(d), &b, DVec3::ZERO, 1.0).unwrap();
        assert!((t - 1.0 / 3.0).abs() < 1e-12);
        let t = segment_box_entry(o, reciprocal(d), &b, DVec3::splat(5.0), 1.0).unwrap();
        assert!((t - 1.0 / 6.0).abs() < 1e-12);
        assert!(segment_box_entry(o, reciprocal(DVec3::X * 30.0), &b, DVec3::ZERO, 1.0).is_none());
        let all = boxes(300);
        let bvh = Bvh::build(&all);
        let origin = DVec3::new(-10.0, 500.0, 50.0);
        let delta = DVec3::new(1100.0, 0.0, 0.0);
        let inv = reciprocal(delta);
        let mut visited = Vec::new();
        bvh.query_ordered(
            |n| segment_box_entry(origin, inv, n, DVec3::splat(2.0), 1.0),
            || 1.0,
            |p| visited.push(p),
        );
        let expected: Vec<u32> = (0..300u32)
            .filter(|&i| {
                segment_box_entry(origin, inv, &all[i as usize], DVec3::splat(2.0), 1.0).is_some()
            })
            .collect();
        for e in &expected {
            assert!(visited.contains(e));
        }
    }
}
