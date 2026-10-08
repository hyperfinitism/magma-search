// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;
use magma_core::{Table, sat::solve_cnf};
use rustsat::{
    instances::Cnf,
    solvers::SolverResult,
    types::{Lit, Var},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Permutation,
    Sat,
}

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

fn element_signatures(t: &Table) -> Vec<Vec<usize>> {
    let n = t.size();
    let rows = t.operation_table();
    let mut outputs = vec![0; n];
    let mut diagonal_outputs = vec![0; n];
    for (a, row) in rows.iter().enumerate() {
        diagonal_outputs[row[a]] += 1;
        for &output in row {
            outputs[output] += 1;
        }
    }
    (0..n)
        .map(|a| {
            let mut row_frequencies = vec![0; n];
            let mut column_frequencies = vec![0; n];
            for x in 0..n {
                row_frequencies[rows[a][x]] += 1;
                column_frequencies[rows[x][a]] += 1;
            }
            row_frequencies.sort_unstable();
            column_frequencies.sort_unstable();
            let mut signature = vec![
                outputs[a],
                diagonal_outputs[a],
                usize::from(rows[a][a] == a),
                (0..n).filter(|&x| rows[a][x] == x).count(),
                (0..n).filter(|&x| rows[x][a] == x).count(),
                (0..n).filter(|&x| rows[a][x] == a).count(),
                (0..n).filter(|&x| rows[x][a] == a).count(),
            ];
            signature.extend(row_frequencies);
            signature.extend(column_frequencies);
            signature
        })
        .collect()
}

pub fn invariant(t: &Table) -> Vec<Vec<usize>> {
    let mut signatures = element_signatures(t);
    signatures.sort_unstable();
    signatures
}

fn encoding(source: &Table, target: &Table) -> Result<Cnf> {
    let n = source.size();
    ensure!(
        target.size() == n,
        "isomorphism encoding requires equal sizes"
    );
    let variables = n.checked_mul(n).context("n² overflows")?;
    ensure!(
        variables > 0 && variables - 1 <= Var::MAX_IDX as usize,
        "too many isomorphism variables for RustSAT"
    );
    let p = |a: usize, i: usize| Lit::positive((a * n + i) as u32);
    let mut cnf = Cnf::new();
    for a in 0..n {
        cnf.add_nary(&(0..n).map(|i| p(a, i)).collect::<Vec<_>>());
        cnf.add_nary(&(0..n).map(|i| p(i, a)).collect::<Vec<_>>());
        for i in 0..n {
            for j in i + 1..n {
                cnf.add_nary(&[!p(a, i), !p(a, j)]);
                cnf.add_nary(&[!p(i, a), !p(j, a)]);
            }
        }
    }
    let source_signatures = element_signatures(source);
    let target_signatures = element_signatures(target);
    for (a, source_signature) in source_signatures.iter().enumerate() {
        for (i, target_signature) in target_signatures.iter().enumerate() {
            if source_signature != target_signature {
                cnf.add_nary(&[!p(a, i)]);
            }
        }
    }
    let source_rows = source.operation_table();
    let target_rows = target.operation_table();
    for (a, source_row) in source_rows.iter().enumerate() {
        for (b, &output) in source_row.iter().enumerate() {
            for (i, target_row) in target_rows.iter().enumerate() {
                if source_signatures[a] != target_signatures[i] {
                    continue;
                }
                for (j, &mapped_output) in target_row.iter().enumerate() {
                    if source_signatures[b] != target_signatures[j] {
                        continue;
                    }
                    if (a == b) != (i == j) {
                        continue;
                    }
                    let output_lit = p(output, mapped_output);
                    if output_lit == p(a, i) || output_lit == p(b, j) {
                        continue;
                    }
                    cnf.add_nary(&[!p(a, i), !p(b, j), output_lit]);
                }
            }
        }
    }
    Ok(cnf)
}

pub fn isomorphic(source: &Table, target: &Table) -> Result<bool> {
    if source.size() != target.size() {
        return Ok(false);
    }
    if source == target {
        return Ok(true);
    }
    if invariant(source) != invariant(target) {
        return Ok(false);
    }
    let cnf = encoding(source, target)?;
    match solve_cnf(&cnf, 1, |_solver| Ok(()))?.status {
        SolverResult::Sat => Ok(true),
        SolverResult::Unsat => Ok(false),
        SolverResult::Interrupted => bail!("isomorphism SAT search was interrupted"),
    }
}
