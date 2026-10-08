// SPDX-License-Identifier: Apache-2.0

mod encoding;
mod solver;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use encoding::Encoding;
use magma_core::{Elements, SearchArgs, Theory, TheorySpec, write_json};
use rustsat::solvers::SolverResult;
use serde::Serialize;
use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    num::NonZeroUsize,
    time::Instant,
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Find one finite magma per size using parallel SAT searches"
)]
struct Args {
    #[command(flatten)]
    search: SearchArgs,
    /// Number of parallel searches of the same size (default: available logical CPUs).
    #[arg(long, default_value_t = default_threads())]
    threads: NonZeroUsize,
    /// Write the CNF sent to the solver as sizeN.cnf (DIMACS).
    #[arg(long)]
    emit_cnf: bool,
}

fn default_threads() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Sat,
    Unsat,
    Unknown,
}

#[derive(Serialize)]
struct SizeResult {
    size: usize,
    theory: TheorySpec,
    status: Status,
    operation_table: Option<Vec<Vec<usize>>>,
    elements: Option<Elements>,
    threads: usize,
    winning_worker: Option<usize>,
    operation_variables: usize,
    variables: u32,
    clauses: usize,
    encoding_seconds: f64,
    solving_seconds: f64,
    elapsed_seconds: f64,
}

#[derive(Serialize)]
struct SizeSummary {
    size: usize,
    status: Status,
    result_file: String,
    variables: u32,
    clauses: usize,
    elapsed_seconds: f64,
}

#[derive(Serialize)]
struct Summary {
    solver: &'static str,
    threads: usize,
    theory: TheorySpec,
    complete: bool,
    sat_sizes: usize,
    unsat_sizes: usize,
    unknown_sizes: usize,
    elapsed_seconds: f64,
    sizes: Vec<SizeSummary>,
}

fn solve_size(n: usize, theory: &Theory, args: &Args) -> Result<SizeResult> {
    let started = Instant::now();
    let encoding = Encoding::new(n, theory.clone())?;
    let encoding_seconds = started.elapsed().as_secs_f64();
    if args.emit_cnf {
        let path = args.search.out.join(format!("size{n}.cnf"));
        let mut writer = BufWriter::new(File::create_new(path)?);
        writeln!(
            writer,
            "c P_a_b_c DIMACS variable = 1 + (a * {n} + b) * {n} + c"
        )?;
        encoding.cnf.write_dimacs(&mut writer, encoding.n_vars)?;
        writer.flush()?;
    }
    let variables = encoding.n_vars;
    let clauses = encoding.cnf.len();
    let solving_started = Instant::now();
    let outcome = solver::solve(&encoding, args.threads.get())?;
    let status = match outcome.status {
        SolverResult::Sat => Status::Sat,
        SolverResult::Unsat => Status::Unsat,
        SolverResult::Interrupted => Status::Unknown,
    };
    Ok(SizeResult {
        size: n,
        theory: theory.spec(),
        status,
        operation_table: outcome.operation_table,
        elements: outcome.elements,
        threads: args.threads.get(),
        winning_worker: outcome.winning_worker,
        operation_variables: n * n * n,
        variables,
        clauses,
        encoding_seconds,
        solving_seconds: solving_started.elapsed().as_secs_f64(),
        elapsed_seconds: started.elapsed().as_secs_f64(),
    })
}

fn run(args: Args) -> Result<()> {
    let (theory, range) = args.search.resolve()?;
    let max_count = range.end().checked_pow(3).context("n^3 overflows")?;
    ensure!(
        max_count <= rustsat::types::Var::MAX_IDX as usize,
        "too many operation variables for RustSAT"
    );
    ensure!(
        !args.search.out.join("summary.json").exists(),
        "summary.json already exists; choose a new --out"
    );
    for n in range.clone() {
        ensure!(
            !args.search.out.join(format!("size{n}.json")).exists()
                && (!args.emit_cnf || !args.search.out.join(format!("size{n}.cnf")).exists()),
            "output for size {n} already exists; choose a new --out"
        );
    }
    fs::create_dir_all(&args.search.out)?;
    let solver_signature = solver::signature()?;
    eprintln!(
        "{solver_signature}: {} parallel searches per size",
        args.threads
    );
    let started = Instant::now();
    let mut sizes = Vec::new();
    for n in range {
        eprintln!("size {n}: encoding and solving");
        let result = solve_size(n, &theory, &args)?;
        let file = format!("size{n}.json");
        write_json(&args.search.out.join(&file), &result)?;
        eprintln!(
            "size {n}: {:?}, {} variables, {} clauses ({:.3}s)",
            result.status, result.variables, result.clauses, result.elapsed_seconds
        );
        sizes.push(SizeSummary {
            size: n,
            status: result.status,
            result_file: file,
            variables: result.variables,
            clauses: result.clauses,
            elapsed_seconds: result.elapsed_seconds,
        });
    }
    let unknown_sizes = sizes.iter().filter(|s| s.status == Status::Unknown).count();
    let summary = Summary {
        solver: solver_signature,
        threads: args.threads.get(),
        theory: theory.spec(),
        complete: unknown_sizes == 0,
        sat_sizes: sizes.iter().filter(|s| s.status == Status::Sat).count(),
        unsat_sizes: sizes.iter().filter(|s| s.status == Status::Unsat).count(),
        unknown_sizes,
        elapsed_seconds: started.elapsed().as_secs_f64(),
        sizes,
    };
    write_json(&args.search.out.join("summary.json"), &summary)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &summary)?;
    writeln!(stdout)?;
    Ok(())
}

fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
