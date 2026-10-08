// SPDX-License-Identifier: Apache-2.0

use crate::{Classes, enumerate_range, isomorphism::Method, table_count};
use anyhow::{Context, Result, ensure};
use magma_core::{Table, Theory, TheorySpec};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    io::{BufRead, BufReader, BufWriter, Write},
    process::{Child, ChildStdout, Command, Stdio},
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerRequest {
    n: usize,
    theory: TheorySpec,
    method: Method,
    start: u128,
    end: u128,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Header {
    examined: u128,
    found: u128,
    class_count: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClassRecord {
    flat: Vec<usize>,
    count: u128,
}

struct Partitions {
    count: usize,
    index: usize,
    width: u128,
    extra: u128,
    start: u128,
}

impl Partitions {
    fn new(total: u128, requested: usize) -> Result<Self> {
        ensure!(requested > 0, "--processes must be at least 1");
        ensure!(total > 0, "cannot partition an empty search");
        let count = requested.min(usize::try_from(total).unwrap_or(usize::MAX));
        Ok(Self {
            count,
            index: 0,
            width: total / count as u128,
            extra: total % count as u128,
            start: 0,
        })
    }
}

impl Iterator for Partitions {
    type Item = (u128, u128);

    fn next(&mut self) -> Option<Self::Item> {
        if self.index == self.count {
            return None;
        }
        let start = self.start;
        self.start += self.width + u128::from((self.index as u128) < self.extra);
        self.index += 1;
        Some((start, self.start))
    }
}

struct Workers {
    children: Vec<Child>,
}

impl Drop for Workers {
    fn drop(&mut self) {
        for child in &mut self.children {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
            }
        }
        for child in &mut self.children {
            let _ = child.wait();
        }
    }
}

fn read_record<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<T> {
    let mut line = String::new();
    ensure!(
        reader.read_line(&mut line)? > 0,
        "worker output ended early"
    );
    serde_json::from_str(&line).context("decoding worker output")
}

struct WorkerStream {
    reader: BufReader<ChildStdout>,
    remaining: usize,
    found: u128,
    count_sum: u128,
}

impl WorkerStream {
    fn next(&mut self, previous: Option<&[usize]>) -> Result<Option<ClassRecord>> {
        if self.remaining == 0 {
            ensure!(
                self.count_sum == self.found,
                "worker class counts disagree with its found count"
            );
            let mut extra = String::new();
            ensure!(
                self.reader.read_line(&mut extra)? == 0,
                "worker produced unexpected trailing output"
            );
            return Ok(None);
        }
        let record: ClassRecord = read_record(&mut self.reader)?;
        ensure!(
            record.count > 0,
            "worker returned an empty isomorphism class"
        );
        if let Some(previous) = previous {
            ensure!(
                previous < record.flat.as_slice(),
                "worker classes are not strictly sorted"
            );
        }
        self.count_sum = self
            .count_sum
            .checked_add(record.count)
            .context("worker class count overflow")?;
        ensure!(
            self.count_sum <= self.found,
            "worker class counts exceed its found count"
        );
        self.remaining -= 1;
        Ok(Some(record))
    }
}

pub fn enumerate(
    n: usize,
    theory: &Theory,
    method: Method,
    requested: usize,
) -> Result<(Classes, u128, u128, usize)> {
    ensure!(n > 0, "size must be positive");
    let total = table_count(n)?;
    let partitions = Partitions::new(total, requested)?;
    let count = partitions.count;
    if count == 1 {
        let (classes, examined, found) = enumerate_range(n, theory, method, 0, total)?;
        return Ok((classes, examined, found, 1));
    }

    let executable = std::env::current_exe().context("locating worker executable")?;
    let specification = theory.spec();
    let mut workers = Workers {
        children: Vec::new(),
    };
    let mut outputs = Vec::new();
    let mut ranges = Vec::new();
    for (index, (start, end)) in partitions.enumerate() {
        let child = Command::new(&executable)
            .arg("__worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting search process {index}"))?;
        workers.children.push(child);
        let child = workers.children.last_mut().expect("just inserted child");
        let mut input = child.stdin.take().context("opening worker input")?;
        serde_json::to_writer(
            &mut input,
            &WorkerRequest {
                n,
                theory: specification.clone(),
                method,
                start,
                end,
            },
        )
        .with_context(|| format!("sending input to search process {index}"))?;
        writeln!(input)?;
        drop(input);
        outputs.push(child.stdout.take().context("opening worker output")?);
        ranges.push((start, end));
    }

    let mut streams = Vec::new();
    let mut examined = 0_u128;
    let mut found = 0_u128;
    let mut pending = BinaryHeap::new();
    for (index, (output, (start, end))) in outputs.into_iter().zip(ranges).enumerate() {
        let mut reader = BufReader::new(output);
        let header: Header = read_record(&mut reader)
            .with_context(|| format!("reading search process {index} header"))?;
        ensure!(
            header.examined == end - start,
            "search process {index} examined an incorrect range length"
        );
        ensure!(
            header.found <= header.examined && header.class_count as u128 <= header.found,
            "search process {index} returned inconsistent counts"
        );
        examined = examined
            .checked_add(header.examined)
            .context("examined count overflow")?;
        found = found
            .checked_add(header.found)
            .context("found count overflow")?;
        let mut stream = WorkerStream {
            reader,
            remaining: header.class_count,
            found: header.found,
            count_sum: 0,
        };
        if let Some(record) = stream.next(None)? {
            pending.push(Reverse((record.flat, index, record.count)));
        }
        streams.push(stream);
    }

    let mut classes = Classes::new(method);
    while let Some(Reverse((flat, index, weight))) = pending.pop() {
        let next = streams[index]
            .next(Some(&flat))
            .with_context(|| format!("reading search process {index} classes"))?;
        classes.merge(Table::from_flat(n, flat)?, weight)?;
        if let Some(record) = next {
            pending.push(Reverse((record.flat, index, record.count)));
        }
    }
    ensure!(
        examined == total,
        "processes did not cover the complete search"
    );
    for (index, child) in workers.children.iter_mut().enumerate() {
        let status = child
            .wait()
            .with_context(|| format!("waiting for search process {index}"))?;
        ensure!(
            status.success(),
            "search process {index} exited with {status}"
        );
    }
    Ok((classes, examined, found, count))
}

pub fn run_worker() -> Result<()> {
    let request: WorkerRequest =
        serde_json::from_reader(std::io::stdin().lock()).context("reading worker request")?;
    ensure!(request.n > 0, "worker size must be positive");
    let total = table_count(request.n)?;
    ensure!(
        request.start < request.end && request.end <= total,
        "worker range must be nonempty and within the search space"
    );
    let theory = Theory::from_spec(request.theory)?;
    let (mut records, examined, found) = {
        let (classes, examined, found) = enumerate_range(
            request.n,
            &theory,
            request.method,
            request.start,
            request.end,
        )?;
        let records: Vec<_> = classes
            .tables
            .into_iter()
            .zip(classes.labeled_counts)
            .map(|(table, count)| ClassRecord {
                flat: table.flat(),
                count,
            })
            .collect();
        (records, examined, found)
    };
    records.sort_unstable_by(|left, right| left.flat.cmp(&right.flat));
    let mut output = BufWriter::new(std::io::stdout().lock());
    serde_json::to_writer(
        &mut output,
        &Header {
            examined,
            found,
            class_count: records.len(),
        },
    )?;
    writeln!(output)?;
    for record in records {
        serde_json::to_writer(&mut output, &record)?;
        writeln!(output)?;
    }
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Partitions;

    #[test]
    fn partitions_cover_the_space_without_empty_ranges() {
        assert_eq!(
            Partitions::new(16, 3).unwrap().collect::<Vec<_>>(),
            vec![(0, 6), (6, 11), (11, 16)]
        );
        assert_eq!(
            Partitions::new(2, 10).unwrap().collect::<Vec<_>>(),
            vec![(0, 1), (1, 2)]
        );
        assert_eq!(
            Partitions::new(1, usize::MAX).unwrap().collect::<Vec<_>>(),
            vec![(0, 1)]
        );
        assert!(Partitions::new(1, 0).is_err());
        assert!(Partitions::new(0, 1).is_err());
    }

    #[test]
    fn partitions_support_the_u128_boundary_without_multiplication() {
        for count in [1, 2, 3, 7] {
            let ranges: Vec<_> = Partitions::new(u128::MAX, count).unwrap().collect();
            assert_eq!(ranges.first().unwrap().0, 0);
            assert_eq!(ranges.last().unwrap().1, u128::MAX);
            for adjacent in ranges.windows(2) {
                assert_eq!(adjacent[0].1, adjacent[1].0);
            }
            let smallest = ranges.iter().map(|&(a, b)| b - a).min().unwrap();
            let largest = ranges.iter().map(|&(a, b)| b - a).max().unwrap();
            assert!(largest - smallest <= 1);
        }
    }
}
