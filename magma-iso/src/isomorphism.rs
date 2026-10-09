// SPDX-License-Identifier: Apache-2.0

use magma_core::Table;

fn permutations(p: &mut [usize], start: usize, visit: &mut impl FnMut(&[usize])) {
    if start == p.len() {
        visit(p);
        return;
    }
    for i in start..p.len() {
        p.swap(start, i);
        permutations(p, start + 1, visit);
        p.swap(start, i);
    }
}

pub fn canonical(t: &Table) -> Vec<usize> {
    let n = t.size();
    let rows = t.operation_table();
    let mut best = t.flat();
    let mut candidate = vec![0; n * n];
    let mut p: Vec<_> = (0..n).collect();
    permutations(&mut p, 0, &mut |p| {
        for x in 0..n {
            for y in 0..n {
                candidate[p[x] * n + p[y]] = p[rows[x][y]];
            }
        }
        if candidate < best {
            best.copy_from_slice(&candidate);
        }
    });
    best
}

#[cfg(test)]
mod tests {
    use super::canonical;
    use magma_core::Table;

    fn relabeling_exists(source: &Table, target: &Table) -> bool {
        [[0, 1], [1, 0]].into_iter().any(|permutation| {
            (0..2).all(|a| {
                (0..2).all(|b| {
                    permutation[source.apply(a, b)] == target.apply(permutation[a], permutation[b])
                })
            })
        })
    }

    #[test]
    fn canonical_classes_match_every_size_two_relabeling() {
        let tables: Vec<_> = (0..16)
            .map(|digits| {
                Table::from_flat(2, (0..4).map(|shift| (digits >> shift) & 1).collect()).unwrap()
            })
            .collect();
        for source in &tables {
            for target in &tables {
                assert_eq!(
                    canonical(source) == canonical(target),
                    relabeling_exists(source, target),
                    "source={source:?}, target={target:?}"
                );
            }
        }
    }
}
