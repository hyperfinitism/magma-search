// SPDX-License-Identifier: Apache-2.0

mod isomorphism;
mod processes;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use magma_core::{
    Elements, Operation, SearchArgs, Table, Theory, TheorySpec, next_assignment, write_json,
};
use serde::Serialize;
use std::{collections::HashMap, fs, io::Write, num::NonZeroUsize, time::Instant};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Exhaustively enumerate finite binary magmas up to isomorphism"
)]
struct Args {
    #[command(flatten)]
    search: SearchArgs,
    /// Number of exhaustive search processes (default: available logical CPUs).
    #[arg(long, default_value_t = default_processes())]
    processes: NonZeroUsize,
}

fn default_processes() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

#[derive(Serialize)]
struct Magma {
    size: usize,
    class_id: usize,
    operation_table: Vec<Vec<usize>>,
    elements: Elements,
    number_of_magmas: u128,
}

struct Classes {
    canonical_ids: HashMap<Vec<usize>, usize>,
    tables: Vec<Table>,
    labeled_counts: Vec<u128>,
}

impl Classes {
    fn new() -> Self {
        Self {
            canonical_ids: HashMap::new(),
            tables: Vec::new(),
            labeled_counts: Vec::new(),
        }
    }

    fn insert(&mut self, table: Table) -> Result<()> {
        self.merge(table, 1)
    }

    fn merge(&mut self, table: Table, count: u128) -> Result<()> {
        ensure!(count > 0, "an isomorphism class must contain a table");
        let key = isomorphism::canonical(&table);
        if let Some(&id) = self.canonical_ids.get(&key) {
            self.labeled_counts[id] = self.labeled_counts[id]
                .checked_add(count)
                .context("isomorphism-class count exceeds u128")?;
        } else {
            let id = self.tables.len();
            self.canonical_ids.insert(key, id);
            self.tables.push(table);
            self.labeled_counts.push(count);
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct SizeSummary {
    size: usize,
    examined_magmas: u128,
    found_magmas: u128,
    isomorphism_classes: usize,
    processes: usize,
    elapsed_seconds: f64,
}

#[derive(Serialize)]
struct Summary {
    theory: TheorySpec,
    processes: usize,
    examined_magmas: u128,
    found_magmas: u128,
    isomorphism_classes: usize,
    elapsed_seconds: f64,
    sizes: Vec<SizeSummary>,
}

fn table_count(n: usize) -> Result<u128> {
    let cells = n.checked_mul(n).context("size squared overflows")?;
    let exp = u32::try_from(cells).context("size squared exceeds u32")?;
    (n as u128)
        .checked_pow(exp)
        .context("number of operation tables exceeds u128")
}

struct FlatOperation<'a> {
    n: usize,
    flat: &'a [usize],
}

impl Operation for FlatOperation<'_> {
    fn size(&self) -> usize {
        self.n
    }

    fn apply(&self, left: usize, right: usize) -> usize {
        self.flat[left * self.n + right]
    }
}

fn enumerate_range(
    n: usize,
    theory: &Theory,
    start: u128,
    end: u128,
) -> Result<(Classes, u128, u128)> {
    theory.validate_size(n)?;
    ensure!(
        start < end && end <= table_count(n)?,
        "require a nonempty range within the operation tables"
    );
    let mut classes = Classes::new();
    let mut flat = vec![0; n * n];
    let mut code = start;
    for value in flat.iter_mut().rev() {
        *value = (code % n as u128) as usize;
        code /= n as u128;
    }
    let mut found = 0;
    for ordinal in start..end {
        if theory.matches(&FlatOperation { n, flat: &flat }) {
            found += 1;
            classes.insert(Table::from_flat(n, flat.clone())?)?;
        }
        if ordinal + 1 < end {
            ensure!(
                next_assignment(&mut flat, n),
                "operation-table range wrapped"
            );
        }
    }
    Ok((classes, end - start, found))
}

fn search(n: usize, args: &Args, theory: &Theory) -> Result<SizeSummary> {
    let started = Instant::now();
    let dir = args.search.out.join(format!("size{n}"));
    fs::create_dir(&dir)?;
    let (classes, examined, found, processes) =
        processes::enumerate(n, theory, args.processes.get())?;
    for (id, table) in classes.tables.iter().enumerate() {
        let magma = Magma {
            size: n,
            class_id: id,
            operation_table: table.operation_table().to_vec(),
            elements: theory.elements(table),
            number_of_magmas: classes.labeled_counts[id],
        };
        write_json(&dir.join(format!("{id}.json")), &magma)?;
    }
    Ok(SizeSummary {
        size: n,
        examined_magmas: examined,
        found_magmas: found,
        isomorphism_classes: classes.tables.len(),
        processes,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    })
}

fn run(args: Args) -> Result<()> {
    let (theory, sizes_requested) = args.search.resolve()?;
    for n in sizes_requested.clone() {
        table_count(n)?;
    }
    ensure!(
        !args.search.out.join("summary.json").exists()
            && !sizes_requested
                .clone()
                .any(|n| args.search.out.join(format!("size{n}")).exists()),
        "output already contains summary.json or a requested size directory; choose a new --out"
    );
    fs::create_dir_all(&args.search.out)?;
    let started = Instant::now();
    let mut sizes = Vec::new();
    for n in sizes_requested {
        eprintln!(
            "size {n}: examining {} operation tables (up to {} processes)",
            table_count(n)?,
            args.processes
        );
        let result = search(n, &args, &theory)?;
        eprintln!(
            "size {n}: {} magmas, {} isomorphism classes ({:.3}s)",
            result.found_magmas, result.isomorphism_classes, result.elapsed_seconds
        );
        sizes.push(result);
    }
    let summary = Summary {
        theory: theory.spec(),
        processes: args.processes.get(),
        examined_magmas: sizes.iter().map(|s| s.examined_magmas).sum(),
        found_magmas: sizes.iter().map(|s| s.found_magmas).sum(),
        isomorphism_classes: sizes.iter().map(|s| s.isomorphism_classes).sum(),
        elapsed_seconds: started.elapsed().as_secs_f64(),
        sizes,
    };
    write_json(&args.search.out.join("summary.json"), &summary)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &summary)?;
    writeln!(out)?;
    Ok(())
}

fn main() {
    let result = if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "__worker")
    {
        processes::run_worker()
    } else {
        run(Args::parse())
    };
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
