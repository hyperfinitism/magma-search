// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, anyhow, ensure};
use rustsat::{
    instances::Cnf,
    solvers::{ControlSignal, Solve, SolverResult, Terminate},
};
use rustsat_cadical::{CaDiCaL, Config};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

pub fn new_solver(worker: usize, stop: Arc<AtomicBool>) -> Result<CaDiCaL<'static, 'static>> {
    let mut solver = CaDiCaL::default();
    solver.set_configuration(match worker % 3 {
        0 => Config::Default,
        1 => Config::Sat,
        _ => Config::Unsat,
    })?;
    solver.set_option("seed", i32::try_from(worker).context("too many workers")?)?;
    if worker > 0 {
        solver.set_option("shuffle", 1)?;
        solver.set_option("shufflerandom", 1)?;
    }
    solver.attach_terminator(move || {
        if stop.load(Ordering::Relaxed) {
            ControlSignal::Terminate
        } else {
            ControlSignal::Continue
        }
    });
    Ok(solver)
}

pub fn signature() -> Result<&'static str> {
    Ok(new_solver(0, Arc::new(AtomicBool::new(false)))?.signature())
}

pub struct Outcome<T> {
    pub status: SolverResult,
    pub model: Option<T>,
    pub winning_worker: Option<usize>,
}

impl<T> Outcome<T> {
    fn interrupted() -> Self {
        Self {
            status: SolverResult::Interrupted,
            model: None,
            winning_worker: None,
        }
    }
}

pub fn solve_worker<T, F>(
    cnf: &Cnf,
    worker: usize,
    stop: Arc<AtomicBool>,
    decode: &F,
) -> Result<Outcome<T>>
where
    F: Fn(&CaDiCaL<'_, '_>) -> Result<T>,
{
    if stop.load(Ordering::Relaxed) {
        return Ok(Outcome::interrupted());
    }
    let mut solver = new_solver(worker, Arc::clone(&stop))?;
    for clause in cnf {
        if stop.load(Ordering::Relaxed) {
            return Ok(Outcome::interrupted());
        }
        solver.add_clause_ref(clause)?;
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(Outcome::interrupted());
    }
    let status = solver.solve()?;
    if status == SolverResult::Interrupted {
        return Ok(Outcome::interrupted());
    }
    let model = if status == SolverResult::Sat {
        Some(decode(&solver)?)
    } else {
        None
    };
    Ok(Outcome {
        status,
        model,
        winning_worker: Some(worker),
    })
}

pub fn solve_cnf<T: Send, F>(cnf: &Cnf, threads: usize, decode: F) -> Result<Outcome<T>>
where
    F: Fn(&CaDiCaL<'_, '_>) -> Result<T> + Sync,
{
    ensure!(threads > 0, "--threads must be at least 1");
    let stop = Arc::new(AtomicBool::new(false));
    let run_worker = |worker, stop_worker| {
        catch_unwind(AssertUnwindSafe(|| {
            solve_worker(cnf, worker, stop_worker, &decode)
        }))
        .unwrap_or_else(|_| Err(anyhow!("SAT worker {worker} panicked")))
        .with_context(|| format!("SAT worker {worker}"))
    };
    if threads == 1 {
        return run_worker(0, stop);
    }
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        for worker in 0..threads {
            let sender = sender.clone();
            let stop_worker = Arc::clone(&stop);
            let run_worker = &run_worker;
            let spawned = thread::Builder::new()
                .name(format!("sat-{worker}"))
                .spawn_scoped(scope, move || {
                    let _ = sender.send(run_worker(worker, stop_worker));
                });
            if let Err(error) = spawned {
                stop.store(true, Ordering::Relaxed);
                return Err(error).context("starting SAT worker");
            }
        }
        drop(sender);
        for outcome in receiver {
            match outcome {
                Ok(outcome) if outcome.status == SolverResult::Interrupted => continue,
                outcome => {
                    stop.store(true, Ordering::Relaxed);
                    return outcome;
                }
            }
        }
        Ok(Outcome::interrupted())
    })
}
