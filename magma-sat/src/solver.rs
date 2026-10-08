// SPDX-License-Identifier: Apache-2.0

use crate::encoding::Encoding;
use anyhow::{Context, Result, anyhow, ensure};
use magma_core::{Elements, Table, sat};
use rustsat::{
    solvers::{Solve, SolverResult},
    types::TernaryVal,
};
use rustsat_cadical::CaDiCaL;

pub fn signature() -> Result<&'static str> {
    sat::signature()
}

pub struct Outcome {
    pub status: SolverResult,
    pub operation_table: Option<Vec<Vec<usize>>>,
    pub elements: Option<Elements>,
    pub winning_worker: Option<usize>,
}

impl Outcome {
    fn from_shared(outcome: sat::Outcome<(Vec<Vec<usize>>, Elements)>) -> Self {
        let (operation_table, elements) = match outcome.model {
            Some((rows, elements)) => (Some(rows), Some(elements)),
            None => (None, None),
        };
        Self {
            status: outcome.status,
            operation_table,
            elements,
            winning_worker: outcome.winning_worker,
        }
    }
}

fn decode_model(
    encoding: &Encoding,
    solver: &CaDiCaL<'_, '_>,
) -> Result<(Vec<Vec<usize>>, Elements)> {
    let mut table = vec![vec![0; encoding.n]; encoding.n];
    for (a, row) in table.iter_mut().enumerate() {
        for (b, value) in row.iter_mut().enumerate() {
            let mut output = None;
            for c in 0..encoding.n {
                match solver.lit_val(encoding.p(a, b, c))? {
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
    Ok(Outcome::from_shared(sat::solve_cnf(
        &encoding.cnf,
        threads,
        |solver| decode_model(encoding, solver),
    )?))
}
