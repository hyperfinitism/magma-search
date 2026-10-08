// SPDX-License-Identifier: Apache-2.0

use anyhow::{Result, bail, ensure};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Term {
    Variable(usize),
    Constant,
    Apply(Box<Term>, Box<Term>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Equation {
    pub left: Term,
    pub right: Term,
    pub variables: Vec<String>,
}

impl Equation {
    pub fn format(&self, symbol: &str) -> String {
        fn term(value: &Term, symbol: &str, variables: &[String]) -> String {
            match value {
                Term::Constant => constant_name(symbol),
                Term::Variable(index) => variable_name(&variables[*index]),
                Term::Apply(left, right) => format!(
                    "({} {})",
                    term(left, symbol, variables),
                    term(right, symbol, variables)
                ),
            }
        }
        format!(
            "{} = {}",
            term(&self.left, symbol, &self.variables),
            term(&self.right, symbol, &self.variables)
        )
    }

    pub fn parse(input: &str, symbol: &str) -> Result<Self> {
        let mut parser = Parser {
            input: input.chars().collect(),
            position: 0,
            symbol,
            variables: Vec::new(),
        };
        let left = parser.term()?;
        parser.whitespace();
        ensure!(parser.peek() == Some('='), "expected '=' between two terms");
        parser.position += 1;
        let right = parser.term()?;
        parser.whitespace();
        ensure!(
            parser.peek().is_none(),
            "unexpected character after right-hand term"
        );
        Ok(Self {
            left,
            right,
            variables: parser.variables,
        })
    }
}

struct Parser<'a> {
    input: Vec<char>,
    position: usize,
    symbol: &'a str,
    variables: Vec<String>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.input.get(self.position).copied()
    }
    fn whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.position += 1;
        }
    }

    fn term(&mut self) -> Result<Term> {
        self.whitespace();
        let mut term = self.atom()?;
        loop {
            self.whitespace();
            match self.peek() {
                None | Some(')' | '=') => return Ok(term),
                _ => term = Term::Apply(Box::new(term), Box::new(self.atom()?)),
            }
        }
    }

    fn atom(&mut self) -> Result<Term> {
        self.whitespace();
        let c = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("expected a term"))?;
        self.position += 1;
        match c {
            '(' => {
                let term = self.term()?;
                self.whitespace();
                ensure!(self.peek() == Some(')'), "unclosed parenthesis");
                self.position += 1;
                Ok(term)
            }
            '[' => {
                let name = self.name(']')?;
                self.constant(&name)
            }
            '{' => {
                let name = self.name('}')?;
                Ok(self.variable(name))
            }
            c if c.is_ascii_uppercase() => self.constant(&c.to_string()),
            c if c.is_ascii_lowercase() => Ok(self.variable(c.to_string())),
            _ => bail!("unexpected character {c:?} at position {}", self.position),
        }
    }

    fn name(&mut self, closing: char) -> Result<String> {
        let mut name = String::new();
        loop {
            let c = self
                .peek()
                .ok_or_else(|| anyhow::anyhow!("unclosed named identifier"))?;
            self.position += 1;
            if c == closing {
                break;
            }
            if c == '\\' {
                let escaped = self
                    .peek()
                    .ok_or_else(|| anyhow::anyhow!("unfinished identifier escape"))?;
                self.position += 1;
                name.push(escaped);
            } else {
                name.push(c);
            }
        }
        ensure!(!name.is_empty(), "named identifiers must be nonempty");
        ensure!(
            !name.chars().any(char::is_control),
            "named identifiers cannot contain control characters"
        );
        Ok(name)
    }

    fn constant(&self, name: &str) -> Result<Term> {
        ensure!(
            name == self.symbol,
            "constant {name:?} differs from the defined symbol {:?}; cross-symbol interpretations are not defined for independent element properties",
            self.symbol
        );
        Ok(Term::Constant)
    }

    fn variable(&mut self, name: String) -> Term {
        let index =
            if let Some(index) = self.variables.iter().position(|variable| variable == &name) {
                index
            } else {
                self.variables.push(name);
                self.variables.len() - 1
            };
        Term::Variable(index)
    }
}

fn escaped(name: &str, closing: char) -> String {
    name.chars()
        .flat_map(|c| {
            if c == closing || c == '\\' {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

pub(crate) fn constant_name(name: &str) -> String {
    if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() {
        name.into()
    } else {
        format!("[{}]", escaped(name, ']'))
    }
}

pub(crate) fn variable_name(name: &str) -> String {
    if name.len() == 1 && name.as_bytes()[0].is_ascii_lowercase() {
        name.into()
    } else {
        format!("{{{}}}", escaped(name, '}'))
    }
}
