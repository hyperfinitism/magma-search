// SPDX-License-Identifier: Apache-2.0

mod input;
#[cfg(feature = "sat")]
pub mod sat;
mod term;

pub use input::{SearchArgs, SymbolValues, write_json};
pub use term::{Equation, Term};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Whether at least one, no, or any number of witnesses are allowed.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Present,
    Absent,
    #[default]
    Any,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolSpec {
    pub symbol: String,
    pub equations: Vec<String>,
    #[serde(default)]
    pub presence: Presence,
    /// Allowed witnesses for a required symbol; other witnesses are permitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<usize>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TheorySpec {
    pub symbols: Vec<SymbolSpec>,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub equations: Vec<Equation>,
    pub presence: Presence,
    pub values: Option<Vec<usize>>,
}

/// Parsed and validated definitions, shared by evaluation and SAT encoding.
#[derive(Clone, Debug)]
pub struct Theory {
    pub symbols: Vec<Symbol>,
    specification: TheorySpec,
}

pub type Elements = BTreeMap<String, Vec<usize>>;

impl Theory {
    pub fn from_spec(mut specification: TheorySpec) -> Result<Self> {
        let mut symbols = Vec::with_capacity(specification.symbols.len());
        for definition in &mut specification.symbols {
            ensure!(
                !definition.symbol.is_empty() && !definition.symbol.chars().any(char::is_control),
                "symbol names must be nonempty and contain no control characters"
            );
            ensure!(
                !symbols.iter().any(|s: &Symbol| s.name == definition.symbol),
                "duplicate symbol {:?}; put all its equations in one definition",
                definition.symbol
            );
            ensure!(
                !definition.equations.is_empty(),
                "symbol {:?} needs at least one equation",
                definition.symbol
            );
            if let Some(values) = &mut definition.values {
                ensure!(
                    !values.is_empty(),
                    "symbol {:?} needs at least one allowed value",
                    definition.symbol
                );
                ensure!(
                    definition.presence != Presence::Absent,
                    "absent symbol {:?} cannot have fixed values",
                    definition.symbol
                );
                values.sort_unstable();
                values.dedup();
                definition.presence = Presence::Present;
            }
            let equations = definition
                .equations
                .iter()
                .map(|equation| {
                    Equation::parse(equation, &definition.symbol).with_context(|| {
                        format!("symbol {:?}, equation {equation:?}", definition.symbol)
                    })
                })
                .collect::<Result<_>>()?;
            symbols.push(Symbol {
                name: definition.symbol.clone(),
                equations,
                presence: definition.presence,
                values: definition.values.clone(),
            });
        }
        Ok(Self {
            symbols,
            specification,
        })
    }

    pub fn from_json(json: &str) -> Result<Self> {
        Self::from_spec(serde_json::from_str(json).context("parsing theory JSON")?)
    }

    pub fn builtin() -> Self {
        let symbols = [
            ("B", "Bxyz = x(yz)"),
            ("C", "Cxyz = xzy"),
            ("I", "Ix = x"),
            ("K", "Kxy = x"),
            ("M", "Mx = xx"),
            ("S", "Sxyz = xz(yz)"),
            ("W", "Wxy = xyy"),
            ("Y", "Yx = x(Yx)"),
        ]
        .into_iter()
        .map(|(symbol, equation)| SymbolSpec {
            symbol: symbol.into(),
            equations: vec![equation.into()],
            presence: Presence::Any,
            values: None,
        })
        .collect();
        Self::from_spec(TheorySpec { symbols }).expect("valid built-in equations")
    }

    pub fn spec(&self) -> TheorySpec {
        TheorySpec {
            symbols: self
                .symbols
                .iter()
                .map(|symbol| {
                    let original = self.specification.symbols.iter().find(|definition| {
                        definition.symbol == symbol.name
                            && definition.equations.len() == symbol.equations.len()
                            && definition.equations.iter().zip(&symbol.equations).all(
                                |(source, parsed)| {
                                    Equation::parse(source, &symbol.name)
                                        .is_ok_and(|equation| equation == *parsed)
                                },
                            )
                    });
                    SymbolSpec {
                        symbol: symbol.name.clone(),
                        equations: original
                            .map(|definition| definition.equations.clone())
                            .unwrap_or_else(|| {
                                symbol
                                    .equations
                                    .iter()
                                    .map(|equation| equation.format(&symbol.name))
                                    .collect()
                            }),
                        presence: symbol.presence,
                        values: symbol.values.clone(),
                    }
                })
                .collect(),
        }
    }

    pub fn set_presence(&mut self, name: &str, presence: Presence) -> Result<()> {
        let symbol = self
            .symbols
            .iter_mut()
            .find(|s| s.name == name)
            .with_context(|| format!("unknown symbol {name:?}"))?;
        ensure!(
            presence != Presence::Absent || symbol.values.is_none(),
            "absent symbol {name:?} cannot have fixed values"
        );
        symbol.presence = if symbol.values.is_some() {
            Presence::Present
        } else {
            presence
        };
        Ok(())
    }

    pub fn set_values(&mut self, name: &str, mut values: Vec<usize>) -> Result<()> {
        let symbol = self
            .symbols
            .iter_mut()
            .find(|s| s.name == name)
            .with_context(|| format!("unknown symbol {name:?}"))?;
        ensure!(
            !values.is_empty(),
            "symbol {name:?} needs at least one allowed value"
        );
        ensure!(
            symbol.presence != Presence::Absent,
            "absent symbol {name:?} cannot have fixed values"
        );
        values.sort_unstable();
        values.dedup();
        symbol.values = Some(values);
        symbol.presence = Presence::Present;
        Ok(())
    }

    pub fn validate_size(&self, n: usize) -> Result<()> {
        ensure!(n > 0, "size must be at least one");
        for symbol in &self.symbols {
            if let Some(values) = &symbol.values {
                ensure!(
                    values.iter().all(|&value| value < n),
                    "fixed values for symbol {:?} must be less than size {n}",
                    symbol.name
                );
            }
        }
        Ok(())
    }

    pub fn elements(&self, table: &impl Operation) -> Elements {
        self.symbols
            .iter()
            .map(|symbol| {
                (
                    symbol.name.clone(),
                    (0..table.size())
                        .filter(|&a| table.satisfies(symbol, a))
                        .collect(),
                )
            })
            .collect()
    }

    pub fn matches(&self, table: &impl Operation) -> bool {
        self.symbols.iter().all(|symbol| match symbol.presence {
            Presence::Any => true,
            Presence::Present => match &symbol.values {
                Some(values) => {
                    values.iter().all(|&a| a < table.size())
                        && values.iter().any(|&a| table.satisfies(symbol, a))
                }
                None => (0..table.size()).any(|a| table.satisfies(symbol, a)),
            },
            Presence::Absent => !(0..table.size()).any(|a| table.satisfies(symbol, a)),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    rows: Vec<Vec<usize>>,
}

pub trait Operation {
    fn size(&self) -> usize;
    fn apply(&self, left: usize, right: usize) -> usize;

    fn evaluate(&self, term: &Term, candidate: usize, variables: &[usize]) -> usize {
        match term {
            Term::Constant => candidate,
            Term::Variable(index) => variables[*index],
            Term::Apply(left, right) => self.apply(
                self.evaluate(left, candidate, variables),
                self.evaluate(right, candidate, variables),
            ),
        }
    }

    fn satisfies(&self, symbol: &Symbol, candidate: usize) -> bool {
        assert!(candidate < self.size(), "candidate outside operation table");
        symbol.equations.iter().all(|equation| {
            // Common laws have only a few variables. Exhaustive search visits
            // billions of tables, so keep these assignments on the stack.
            let mut local = [0; 8];
            let mut large;
            let assignment = if equation.variables.len() <= local.len() {
                &mut local[..equation.variables.len()]
            } else {
                large = vec![0; equation.variables.len()];
                large.as_mut_slice()
            };
            loop {
                if self.evaluate(&equation.left, candidate, assignment)
                    != self.evaluate(&equation.right, candidate, assignment)
                {
                    return false;
                }
                if !next_assignment(assignment, self.size()) {
                    return true;
                }
            }
        })
    }
}

impl Operation for Table {
    fn size(&self) -> usize {
        self.rows.len()
    }
    fn apply(&self, left: usize, right: usize) -> usize {
        self.rows[left][right]
    }
}

impl Table {
    pub fn new(rows: Vec<Vec<usize>>) -> Result<Self> {
        let n = rows.len();
        ensure!(n > 0, "an operation table must be nonempty");
        ensure!(
            rows.iter().all(|row| row.len() == n),
            "an operation table must be square"
        );
        ensure!(
            rows.iter().flatten().all(|&value| value < n),
            "operation-table entries must be less than its size"
        );
        Ok(Self { rows })
    }

    pub fn from_flat(n: usize, values: Vec<usize>) -> Result<Self> {
        ensure!(n > 0, "size must be at least one");
        ensure!(
            Some(values.len()) == n.checked_mul(n),
            "operation-table length must equal size squared"
        );
        Self::new(values.chunks(n).map(<[usize]>::to_vec).collect())
    }

    pub fn size(&self) -> usize {
        self.rows.len()
    }
    pub fn operation_table(&self) -> &[Vec<usize>] {
        &self.rows
    }
    pub fn flat(&self) -> Vec<usize> {
        self.rows.iter().flatten().copied().collect()
    }
    pub fn apply(&self, left: usize, right: usize) -> usize {
        self.rows[left][right]
    }

    pub fn evaluate(&self, term: &Term, candidate: usize, variables: &[usize]) -> usize {
        Operation::evaluate(self, term, candidate, variables)
    }

    pub fn satisfies(&self, symbol: &Symbol, candidate: usize) -> bool {
        Operation::satisfies(self, symbol, candidate)
    }
}

pub fn next_assignment(values: &mut [usize], n: usize) -> bool {
    assert!(n > 0);
    for value in values.iter_mut().rev() {
        if *value < n - 1 {
            *value += 1;
            return true;
        }
        *value = 0;
    }
    false
}
