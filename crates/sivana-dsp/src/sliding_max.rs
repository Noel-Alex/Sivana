//! O(n) sliding-window maximum via monotonic deques (§9.1).
//!
//! The frozen peak detector compares every cell against its full 2D
//! neighborhood: `O(T*F*Wt*Wf)`. Separable max filtering reduces this to
//! two linear passes: max along frequency, then max along time.
//!
//! A cell equals its centered sliding max exactly when it is the window
//! maximum — combined with tie-breaking this recovers the legacy local-max
//! semantics at linear cost. Edges use truncated windows, matching the
//! legacy detector's saturating bounds.

use std::collections::VecDeque;

/// Causal sliding max: `out[i] = max input[i+1-w ..= i]`, width `w >= 1`.
pub fn sliding_max(input: &[f32], w: usize) -> Vec<f32> {
    let mut out = Vec::new();
    sliding_max_into(input, w, &mut out);
    out
}

/// Allocation-reusing causal variant.
pub fn sliding_max_into(input: &[f32], w: usize, out: &mut Vec<f32>) {
    let n = input.len();
    out.clear();
    out.resize(n, f32::NEG_INFINITY);
    if n == 0 {
        return;
    }
    let w = w.max(1);
    let mut deque: VecDeque<usize> = VecDeque::with_capacity(w.min(n));

    for (i, slot) in out.iter_mut().enumerate() {
        while let Some(&back) = deque.back() {
            if input[back] <= input[i] {
                deque.pop_back();
            } else {
                break;
            }
        }
        deque.push_back(i);
        let left = i.saturating_sub(w - 1);
        while deque.front().is_some_and(|&f| f < left) {
            deque.pop_front();
        }
        *slot = input[*deque.front().expect("deque non-empty after push")];
    }
}

/// Centered sliding max: `out[i] = max input[i-r ..= i+r]` with truncated
/// windows at the edges — exactly the neighborhood the legacy detector scans.
///
/// Composed from two causal monotonic-deque passes (left-ending and
/// right-starting): `centered(i) = max(max input[i-r..i], max input[i..i+r])`.
/// The right-starting pass walks the buffer backwards so no reversed copy
/// is materialized.
pub fn sliding_max_centered(input: &[f32], radius: usize) -> Vec<f32> {
    let mut out = Vec::new();
    sliding_max_centered_into(input, radius, &mut out);
    out
}

/// Allocation-reusing centered variant.
pub fn sliding_max_centered_into(input: &[f32], radius: usize, out: &mut Vec<f32>) {
    let n = input.len();
    out.clear();
    if n == 0 {
        return;
    }
    let w = radius.max(0) + 1;
    out.resize(n, f32::NEG_INFINITY);

    // Forward pass: out[i] = max input[max(0, i-w+1)..=i].
    let mut fwd: VecDeque<usize> = VecDeque::with_capacity(w.min(n));
    for i in 0..n {
        while let Some(&back) = fwd.back() {
            if input[back] <= input[i] {
                fwd.pop_back();
            } else {
                break;
            }
        }
        fwd.push_back(i);
        let left = i.saturating_sub(w - 1);
        while fwd.front().is_some_and(|&f| f < left) {
            fwd.pop_front();
        }
        out[i] = input[*fwd.front().expect("deque non-empty after push")];
    }

    // Backward pass: right[i] = max input[i..=min(n, i+w)-1]; fold into out.
    // Iterating downwards puts out-of-window (large) indices at the FRONT
    // and keeps the running maximum at the front too.
    let mut bwd: VecDeque<usize> = VecDeque::with_capacity(w.min(n));
    for i in (0..n).rev() {
        while let Some(&back) = bwd.back() {
            if input[back] <= input[i] {
                bwd.pop_back();
            } else {
                break;
            }
        }
        bwd.push_back(i);
        let right_end = (i + w).min(n);
        while bwd.front().is_some_and(|&f| f >= right_end) {
            bwd.pop_front();
        }
        out[i] = out[i].max(input[*bwd.front().expect("deque non-empty after push")]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sivana_audio::rng::XorShift64Star;

    fn brute_force_centered(input: &[f32], r: usize) -> Vec<f32> {
        (0..input.len())
            .map(|i| {
                let lo = i.saturating_sub(r);
                let hi = (i + r + 1).min(input.len());
                input[lo..hi]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max)
            })
            .collect()
    }

    #[test]
    fn centered_matches_brute_force_on_random_buffers() {
        let mut rng = XorShift64Star::new(321);
        for len in [1usize, 2, 7, 50, 333] {
            let buf: Vec<f32> = (0..len).map(|_| rng.next_bipolar() * 100.0).collect();
            for r in [0usize, 1, 2, 5, 31] {
                assert_eq!(
                    sliding_max_centered(&buf, r),
                    brute_force_centered(&buf, r),
                    "len={len} r={r}"
                );
            }
        }
    }

    #[test]
    fn causal_matches_brute_force() {
        let mut rng = XorShift64Star::new(999);
        for len in [1usize, 5, 40, 200] {
            let buf: Vec<f32> = (0..len).map(|_| rng.next_bipolar()).collect();
            for w in [1usize, 2, 8] {
                let expect: Vec<f32> = (0..len)
                    .map(|i| {
                        let lo = i.saturating_sub(w - 1);
                        buf[lo..=i]
                            .iter()
                            .copied()
                            .fold(f32::NEG_INFINITY, f32::max)
                    })
                    .collect();
                assert_eq!(sliding_max(&buf, w), expect, "len={len} w={w}");
            }
        }
    }

    #[test]
    fn plateau_and_edge_semantics() {
        let buf = [1.0, 2.0, 2.0, 2.0, 1.0];
        assert_eq!(sliding_max_centered(&buf, 1), vec![2.0, 2.0, 2.0, 2.0, 2.0]);
        assert_eq!(sliding_max_centered(&[7.0], 4), vec![7.0]);
        assert_eq!(
            sliding_max_centered(&[1.0, 9.0, 2.0], 10),
            vec![9.0, 9.0, 9.0]
        );
    }

    #[test]
    fn tiny_buffers_with_large_radius() {
        // Guards the tail-loop subtraction.
        assert_eq!(sliding_max_centered(&[3.0, 1.0], 5), vec![3.0, 3.0]);
        assert_eq!(sliding_max_centered(&[1.0], 0), vec![1.0]);
    }

    #[test]
    fn empty_input_is_safe() {
        assert!(sliding_max_centered(&[], 3).is_empty());
        assert!(sliding_max(&[], 3).is_empty());
    }

    #[test]
    fn centered_into_matches_allocating_variant() {
        let mut rng = XorShift64Star::new(55);
        for len in [1usize, 3, 17, 129] {
            let buf: Vec<f32> = (0..len).map(|_| rng.next_bipolar() * 10.0).collect();
            for r in [0usize, 1, 3, 9] {
                let mut out = Vec::new();
                sliding_max_centered_into(&buf, r, &mut out);
                assert_eq!(out, sliding_max_centered(&buf, r), "len={len} r={r}");
            }
        }
    }
}
