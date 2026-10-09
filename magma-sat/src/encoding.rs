// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, ensure};
use magma_core::{Equation, Presence, Symbol, Term, Theory, next_assignment};
use rustsat::{
    instances::Cnf,
    types::{Lit, Var},
};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Value {
    Concrete(usize),
    Selected(Vec<Lit>),
}

pub struct Encoding {
    pub n: usize,
    pub theory: Theory,
    pub cnf: Cnf,
    pub n_vars: u32,
    pub symbol_elements: HashMap<String, Vec<Lit>>,
    applications: HashMap<(Value, Value), Value>,
    equalities: HashMap<(Value, Value), Lit>,
    conjunctions: HashMap<Vec<Lit>, Lit>,
    true_lit: Lit,
}

impl Encoding {
    pub fn new(n: usize, theory: Theory) -> Result<Self> {
        theory.validate_size(n)?;
        let count = n
            .checked_pow(3)
            .ok_or_else(|| anyhow::anyhow!("n^3 overflows"))?;
        ensure!(
            count <= Var::MAX_IDX as usize,
            "too many operation variables for RustSAT"
        );
        let mut result = Self {
            n,
            theory,
            cnf: Cnf::new(),
            n_vars: count as u32,
            symbol_elements: HashMap::new(),
            applications: HashMap::new(),
            equalities: HashMap::new(),
            conjunctions: HashMap::new(),
            true_lit: Lit::positive(0),
        };
        for a in 0..n {
            for b in 0..n {
                result.exactly_one(&result.cell(a, b));
            }
        }
        result.true_lit = result.new_lit()?;
        result.cnf.add_nary(&[result.true_lit]);
        for symbol in result.theory.symbols.clone() {
            match symbol.presence {
                Presence::Present => {
                    let witnesses = result.encode_symbol(&symbol)?;
                    if let Some(values) = &symbol.values {
                        let candidates: Vec<_> =
                            values.iter().map(|&value| witnesses[value]).collect();
                        result.cnf.add_nary(&candidates);
                    } else {
                        result.cnf.add_nary(&witnesses);
                    }
                }
                Presence::Absent => {
                    for witness in result.encode_symbol(&symbol)? {
                        result.cnf.add_nary(&[!witness]);
                    }
                }
                Presence::Any => {}
            }
        }
        Ok(result)
    }

    pub fn p(&self, a: usize, b: usize, c: usize) -> Lit {
        Lit::positive(((a * self.n + b) * self.n + c) as u32)
    }

    fn cell(&self, a: usize, b: usize) -> Vec<Lit> {
        (0..self.n).map(|c| self.p(a, b, c)).collect()
    }

    fn new_lit(&mut self) -> Result<Lit> {
        ensure!(
            self.n_vars <= Var::MAX_IDX,
            "too many auxiliary variables for RustSAT"
        );
        let lit = Lit::positive(self.n_vars);
        self.n_vars += 1;
        Ok(lit)
    }

    fn exactly_one(&mut self, values: &[Lit]) {
        self.cnf.add_nary(values);
        for (i, &a) in values.iter().enumerate() {
            for &b in &values[i + 1..] {
                self.cnf.add_nary(&[!a, !b]);
            }
        }
    }

    fn apply(&mut self, left: Value, right: Value) -> Result<Value> {
        if let (Value::Concrete(a), Value::Concrete(b)) = (&left, &right) {
            return Ok(Value::Selected(self.cell(*a, *b)));
        }
        let key = (left, right);
        if let Some(value) = self.applications.get(&key) {
            return Ok(value.clone());
        }
        let output: Vec<_> = (0..self.n).map(|_| self.new_lit()).collect::<Result<_>>()?;
        self.exactly_one(&output);
        match &key {
            (Value::Concrete(a), Value::Selected(right)) => {
                for (b, &selected) in right.iter().enumerate() {
                    for (c, &value) in output.iter().enumerate() {
                        let p = self.p(*a, b, c);
                        self.cnf.add_nary(&[!selected, !p, value]);
                        self.cnf.add_nary(&[!selected, p, !value]);
                    }
                }
            }
            (Value::Selected(left), Value::Concrete(b)) => {
                for (a, &selected) in left.iter().enumerate() {
                    for (c, &value) in output.iter().enumerate() {
                        let p = self.p(a, *b, c);
                        self.cnf.add_nary(&[!selected, !p, value]);
                        self.cnf.add_nary(&[!selected, p, !value]);
                    }
                }
            }
            (Value::Selected(left), Value::Selected(right)) => {
                for (a, &selected_left) in left.iter().enumerate() {
                    for (b, &selected_right) in right.iter().enumerate() {
                        for (c, &value) in output.iter().enumerate() {
                            let p = self.p(a, b, c);
                            self.cnf
                                .add_nary(&[!selected_left, !selected_right, !p, value]);
                            self.cnf
                                .add_nary(&[!selected_left, !selected_right, p, !value]);
                        }
                    }
                }
            }
            (Value::Concrete(_), Value::Concrete(_)) => unreachable!(),
        }
        let value = Value::Selected(output);
        self.applications.insert(key, value.clone());
        Ok(value)
    }

    fn term(&mut self, term: &Term, candidate: usize, variables: &[usize]) -> Result<Value> {
        match term {
            Term::Variable(index) => Ok(Value::Concrete(
                *variables
                    .get(*index)
                    .context("equation contains an undeclared variable")?,
            )),
            Term::Constant => Ok(Value::Concrete(candidate)),
            Term::Apply(left, right) => {
                let left = self.term(left, candidate, variables)?;
                let right = self.term(right, candidate, variables)?;
                self.apply(left, right)
            }
        }
    }

    fn equality(&mut self, left: Value, right: Value) -> Result<Lit> {
        if left == right {
            return Ok(self.true_lit);
        }
        match (&left, &right) {
            (Value::Concrete(a), Value::Concrete(b)) => {
                return Ok(if a == b {
                    self.true_lit
                } else {
                    !self.true_lit
                });
            }
            (Value::Selected(selected), Value::Concrete(c))
            | (Value::Concrete(c), Value::Selected(selected)) => return Ok(selected[*c]),
            _ => {}
        }
        let key = (left, right);
        if let Some(&eq) = self.equalities.get(&key) {
            return Ok(eq);
        }
        if let Some(&eq) = self.equalities.get(&(key.1.clone(), key.0.clone())) {
            return Ok(eq);
        }
        let eq = self.new_lit()?;
        let (Value::Selected(left), Value::Selected(right)) = &key else {
            unreachable!();
        };
        for (&a, &b) in left.iter().zip(right) {
            self.cnf.add_nary(&[!eq, !a, b]);
            self.cnf.add_nary(&[!eq, a, !b]);
            self.cnf.add_nary(&[!a, !b, eq]);
        }
        self.equalities.insert(key, eq);
        Ok(eq)
    }

    fn conjunction(&mut self, mut laws: Vec<Lit>) -> Result<Lit> {
        if laws.contains(&!self.true_lit) {
            return Ok(!self.true_lit);
        }
        laws.retain(|&law| law != self.true_lit);
        laws.sort_unstable();
        laws.dedup();
        match laws.len() {
            0 => return Ok(self.true_lit),
            1 => return Ok(laws[0]),
            _ => {}
        }
        if let Some(&gate) = self.conjunctions.get(&laws) {
            return Ok(gate);
        }
        let gate = self.new_lit()?;
        for &law in &laws {
            self.cnf.add_nary(&[!gate, law]);
        }
        let mut reverse = vec![gate];
        reverse.extend(laws.iter().map(|&law| !law));
        self.cnf.add_nary(&reverse);
        self.conjunctions.insert(laws, gate);
        Ok(gate)
    }

    fn equation_laws(
        &mut self,
        equation: &Equation,
        candidate: usize,
        laws: &mut Vec<Lit>,
    ) -> Result<()> {
        let mut variables = vec![0; equation.variables.len()];
        loop {
            let left = self.term(&equation.left, candidate, &variables)?;
            let right = self.term(&equation.right, candidate, &variables)?;
            laws.push(self.equality(left, right)?);
            if !next_assignment(&mut variables, self.n) {
                return Ok(());
            }
        }
    }

    pub fn encode_symbol(&mut self, symbol: &Symbol) -> Result<Vec<Lit>> {
        if let Some(lits) = self.symbol_elements.get(&symbol.name) {
            return Ok(lits.clone());
        }
        let mut witnesses = Vec::with_capacity(self.n);
        for candidate in 0..self.n {
            let mut laws = Vec::new();
            for equation in &symbol.equations {
                self.equation_laws(equation, candidate, &mut laws)?;
            }
            witnesses.push(self.conjunction(laws)?);
        }
        self.symbol_elements
            .insert(symbol.name.clone(), witnesses.clone());
        Ok(witnesses)
    }
}

#[cfg(test)]
mod tests {
    use super::Encoding;
    use crate::sat;
    use magma_core::Theory;
    use rustsat::solvers::SolverResult;

    fn two_sided_unit(table: &[[usize; 2]; 2], candidate: usize) -> bool {
        (0..2).all(|x| table[candidate][x] == x && table[x][candidate] == x)
    }

    fn mocker(table: &[[usize; 2]; 2], candidate: usize) -> bool {
        (0..2).all(|x| table[candidate][x] == table[x][x])
    }

    #[test]
    fn candidate_constraints_match_every_size_two_table() {
        struct Case {
            name: &'static str,
            json: &'static str,
            expected: fn(&[[usize; 2]; 2]) -> bool,
        }
        let cases = [
            Case {
                name: "two-sided unit fixed to zero",
                json: r#"{"symbols":[{"symbol":"I","equations":["Ix = x","xI = x"],"values":[0]}]}"#,
                expected: |table| two_sided_unit(table, 0),
            },
            Case {
                name: "two-sided unit in either candidate",
                json: r#"{"symbols":[{"symbol":"I","equations":["Ix = x","xI = x"],"values":[0,1]}]}"#,
                expected: |table| (0..2).any(|a| two_sided_unit(table, a)),
            },
            Case {
                name: "mocker fixed to one",
                json: r#"{"symbols":[{"symbol":"M","equations":["Mx = xx"],"values":[1]}]}"#,
                expected: |table| mocker(table, 1),
            },
            Case {
                name: "independent fixed witnesses",
                json: r#"{"symbols":[{"symbol":"I","equations":["Ix = x","xI = x"],"values":[0]},{"symbol":"M","equations":["Mx = xx"],"presence":"present","values":[1]}]}"#,
                expected: |table| two_sided_unit(table, 0) && mocker(table, 1),
            },
            Case {
                name: "optional symbol without candidates",
                json: r#"{"symbols":[{"symbol":"I","equations":["Ix = x","xI = x"]}]}"#,
                expected: |_| true,
            },
            Case {
                name: "witnesses outside candidates remain allowed",
                json: r#"{"symbols":[{"symbol":"I","equations":["I = I"],"values":[1]}]}"#,
                expected: |_| true,
            },
        ];
        for case in cases {
            let theory = Theory::from_json(case.json).unwrap();
            for digits in 0..16 {
                let table = [
                    [(digits >> 3) & 1, (digits >> 2) & 1],
                    [(digits >> 1) & 1, digits & 1],
                ];
                let mut encoding = Encoding::new(2, theory.clone()).unwrap();
                for (a, row) in table.iter().enumerate() {
                    for (b, &value) in row.iter().enumerate() {
                        encoding.cnf.add_nary(&[encoding.p(a, b, value)]);
                    }
                }
                let result = sat::solve_cnf(&encoding.cnf, 1, |_| Ok(())).unwrap();
                let expected = if (case.expected)(&table) {
                    SolverResult::Sat
                } else {
                    SolverResult::Unsat
                };
                assert_eq!(result.status, expected, "{}: {table:?}", case.name);
            }
        }
    }

    #[test]
    fn encoding_rejects_candidate_outside_the_size() {
        let theory = Theory::from_json(
            r#"{"symbols":[{"symbol":"I","equations":["Ix = x"],"values":[2]}]}"#,
        )
        .unwrap();
        assert!(Encoding::new(2, theory).is_err());
    }
}
