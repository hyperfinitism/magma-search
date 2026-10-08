// SPDX-License-Identifier: Apache-2.0

use crate::{Presence, Theory};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufWriter, Write},
    num::NonZeroUsize,
    ops::RangeInclusive,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolValues {
    pub symbol: String,
    pub values: Vec<usize>,
}

fn parse_symbol_values(source: &str) -> std::result::Result<SymbolValues, String> {
    let (symbol, values) = source
        .rsplit_once('=')
        .ok_or_else(|| "expected SYMBOL=INDEX[,INDEX...]".to_owned())?;
    if symbol.is_empty() || symbol.chars().any(char::is_control) {
        return Err("symbol names must be nonempty and contain no control characters".into());
    }
    let mut values = values
        .split(',')
        .map(|value| {
            value
                .trim()
                .parse::<usize>()
                .map_err(|_| "fixed values must be nonnegative integer indices".to_owned())
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    values.sort_unstable();
    values.dedup();
    Ok(SymbolValues {
        symbol: symbol.into(),
        values,
    })
}

#[derive(Clone, Debug, Args)]
pub struct SearchArgs {
    /// Minimum size of the underlying set; requires --max.
    #[arg(long = "min", required_unless_present = "size", requires = "max_size")]
    pub min_size: Option<NonZeroUsize>,
    /// Maximum size of the underlying set; requires --min.
    #[arg(long = "max", requires = "min_size")]
    pub max_size: Option<NonZeroUsize>,
    /// Search one size, instead of --min/--max.
    #[arg(long, conflicts_with_all = ["min_size", "max_size"])]
    pub size: Option<NonZeroUsize>,
    /// Output directory.
    #[arg(long, default_value = "out")]
    pub out: PathBuf,
    /// JSON specification of symbols and their equations.
    #[arg(long)]
    pub spec: Option<PathBuf>,
    /// Require witnesses of these symbols, overriding their file presence.
    #[arg(long, value_delimiter = ',', num_args = 0..)]
    pub include: Vec<String>,
    /// Forbid witnesses of these symbols, overriding their file presence.
    #[arg(long, value_delimiter = ',', num_args = 0..)]
    pub exclude: Vec<String>,
    /// Require a witness at one of these indices; repeat for different symbols.
    #[arg(long = "fix", value_name = "SYMBOL=INDEX[,INDEX...]", value_parser = parse_symbol_values)]
    pub fixed_values: Vec<SymbolValues>,
}

impl SearchArgs {
    pub fn resolve(&self) -> Result<(Theory, RangeInclusive<usize>)> {
        let min = self
            .size
            .or(self.min_size)
            .expect("clap requires --size or --min/--max")
            .get();
        let max = self
            .size
            .or(self.max_size)
            .expect("clap requires --size or --min/--max")
            .get();
        ensure!(min <= max, "require --min <= --max");
        let mut theory = if let Some(path) = &self.spec {
            Theory::from_json(
                &fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
            )
            .with_context(|| format!("loading {}", path.display()))?
        } else {
            Theory::builtin()
        };
        let normalize = |name: &str| -> String {
            if self.spec.is_none() {
                name.to_ascii_uppercase()
            } else {
                name.into()
            }
        };
        let include: Vec<_> = self.include.iter().map(|name| normalize(name)).collect();
        let exclude: Vec<_> = self.exclude.iter().map(|name| normalize(name)).collect();
        ensure!(
            !include.iter().any(|name| exclude.contains(name)),
            "the same symbol cannot be included and excluded"
        );
        for name in include {
            theory.set_presence(&name, Presence::Present)?;
        }
        for name in exclude {
            theory.set_presence(&name, Presence::Absent)?;
        }
        let mut fixed_symbols = BTreeSet::new();
        for constraint in &self.fixed_values {
            let name = normalize(&constraint.symbol);
            ensure!(
                fixed_symbols.insert(name.clone()),
                "fixed values for symbol {name:?} were specified more than once"
            );
            theory.set_values(&name, constraint.values.clone())?;
        }
        theory.validate_size(min)?;
        Ok((theory, min..=max))
    }
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut writer = BufWriter::new(
        File::create_new(path).with_context(|| format!("creating {}", path.display()))?,
    );
    serde_json::to_writer_pretty(&mut writer, value)?;
    writeln!(writer)?;
    writer.flush()?;
    Ok(())
}
