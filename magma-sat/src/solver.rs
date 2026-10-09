// SPDX-License-Identifier: Apache-2.0

use crate::{encoding::Encoding, sat};
use anyhow::{Context, Result, anyhow, ensure};
use magma_core::{Elements, Table};
use rustsat::{
    solvers::SolverResult,
    types::{Assignment, TernaryVal},
};

pub fn signature() -> Result<String> {
    sat::signature()
}

pub struct Outcome {
    pub status: SolverResult,
    pub operation_table: Option<Vec<Vec<usize>>>,
    pub elements: Option<Elements>,
}

impl Outcome {
    fn from_sat(outcome: sat::Outcome<(Vec<Vec<usize>>, Elements)>) -> Self {
        let (operation_table, elements) = match outcome.model {
            Some((rows, elements)) => (Some(rows), Some(elements)),
            None => (None, None),
        };
        Self {
            status: outcome.status,
            operation_table,
            elements,
        }
    }
}

fn decode_model(
    encoding: &Encoding,
    assignment: &Assignment,
) -> Result<(Vec<Vec<usize>>, Elements)> {
    let mut table = vec![vec![0; encoding.n]; encoding.n];
    for (a, row) in table.iter_mut().enumerate() {
        for (b, value) in row.iter_mut().enumerate() {
            let mut output = None;
            for c in 0..encoding.n {
                match assignment.lit_value(encoding.p(a, b, c)) {
                    TernaryVal::True => {
                        ensure!(
                            output.is_none(),
                            "invalid model: op({a},{b}) is not single-valued"
                        );
                        output = Some(c);
                    }
                    TernaryVal::False => {}
                    TernaryVal::DontCare => {
                        return Err(anyhow!(
                            "solver returned an incomplete operation-table assignment"
                        ));
                    }
                }
            }
            *value = output.context("invalid model: operation table is not total")?;
        }
    }
    let decoded = Table::new(table.clone())?;
    ensure!(
        encoding.theory.matches(&decoded),
        "decoded model fails the theory's presence constraints"
    );
    let elements = encoding.theory.elements(&decoded);
    Ok((table, elements))
}

pub fn solve(encoding: &Encoding, threads: usize) -> Result<Outcome> {
    Ok(Outcome::from_sat(sat::solve_cnf(
        &encoding.cnf,
        threads,
        |assignment| decode_model(encoding, assignment),
    )?))
}

#[cfg(test)]
mod tests {
    use super::solve;
    use crate::encoding::Encoding;
    use magma_core::Theory;
    use rustsat::solvers::SolverResult;

    fn fix_operation_table(encoding: &mut Encoding, rows: &[Vec<usize>]) {
        for (a, row) in rows.iter().enumerate() {
            for (b, &value) in row.iter().enumerate() {
                encoding.cnf.add_nary(&[encoding.p(a, b, value)]);
            }
        }
    }

    #[test]
    fn decoded_model_has_a_common_witness_for_multiple_equations() {
        let theory = Theory::from_json(
            r#"{"symbols":[{"symbol":"I","equations":["Ix = x","xI = x"],"presence":"present"}]}"#,
        )
        .unwrap();
        for threads in [1, 2] {
            let encoding = Encoding::new(2, theory.clone()).unwrap();
            let outcome = solve(&encoding, threads).unwrap();
            assert_eq!(outcome.status, SolverResult::Sat);
            let rows = outcome.operation_table.unwrap();
            let witnesses: Vec<_> = (0..2)
                .filter(|&a| (0..2).all(|x| rows[a][x] == x && rows[x][a] == x))
                .collect();
            assert!(!witnesses.is_empty());
            assert_eq!(outcome.elements.unwrap()["I"], witnesses);
        }
    }

    #[test]
    fn absence_allows_separate_witnesses_of_individual_equations() {
        let rows = vec![vec![0, 1], vec![1, 1]];
        for threads in [1, 2] {
            for (presence, expected) in [
                ("absent", SolverResult::Sat),
                ("present", SolverResult::Unsat),
            ] {
                let theory = Theory::from_json(&format!(
                    r#"{{"symbols":[{{"symbol":"Q","equations":["Qx = x","Qx = Q"],"presence":"{presence}"}}]}}"#
                ))
                .unwrap();
                let mut encoding = Encoding::new(2, theory).unwrap();
                fix_operation_table(&mut encoding, &rows);
                let outcome = solve(&encoding, threads).unwrap();
                assert_eq!(outcome.status, expected);
                if expected == SolverResult::Sat {
                    assert_eq!(outcome.operation_table.unwrap(), rows);
                    assert!(outcome.elements.unwrap()["Q"].is_empty());
                } else {
                    assert!(outcome.operation_table.is_none());
                    assert!(outcome.elements.is_none());
                }
            }
        }
    }

    #[test]
    fn fixed_list_requires_a_candidate_but_permits_other_witnesses() {
        let theory = Theory::from_json(
            r#"{"symbols":[{"symbol":"M","equations":["Mx = xx"],"values":[1,2]}]}"#,
        )
        .unwrap();
        let tables = [
            vec![vec![0, 0, 0], vec![0, 1, 0], vec![0, 1, 2]],
            vec![vec![0, 1, 2], vec![0, 1, 0], vec![0, 0, 2]],
            vec![vec![0, 1, 2], vec![0, 1, 2], vec![0, 1, 2]],
        ];
        for threads in [1, 2] {
            for rows in &tables {
                let witnesses: Vec<_> = (0..3)
                    .filter(|&a| (0..3).all(|x| rows[a][x] == rows[x][x]))
                    .collect();
                let expected = if witnesses.iter().any(|a| [1, 2].contains(a)) {
                    SolverResult::Sat
                } else {
                    SolverResult::Unsat
                };
                let mut encoding = Encoding::new(3, theory.clone()).unwrap();
                fix_operation_table(&mut encoding, rows);
                let outcome = solve(&encoding, threads).unwrap();
                assert_eq!(outcome.status, expected, "threads={threads}, rows={rows:?}");
                if expected == SolverResult::Sat {
                    assert_eq!(outcome.operation_table.unwrap(), *rows);
                    assert_eq!(outcome.elements.unwrap()["M"], witnesses);
                } else {
                    assert!(outcome.operation_table.is_none());
                    assert!(outcome.elements.is_none());
                }
            }
        }
    }
}
