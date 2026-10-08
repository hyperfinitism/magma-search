# Magma Search: Searching finite magmas with specific elements

![SemVer: pre-release](https://img.shields.io/badge/magma--search-pre--release-blue)
![MSRV: 1.88.0](https://img.shields.io/badge/MSRV-1.88.0-brown.svg)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-red.svg)](https://www.apache.org/licenses/LICENSE-2.0)

This repository searches finite sets equipped with a total binary operation for elements satisfying user-defined equations.
It consists of three Rust crates:

- `magma-core` (lib): term and equation types, specification parsing, presence constraints, and model evaluation.
- `magma-iso` (bin): exhaustive enumeration of operation tables and their isomorphism classes.
- `magma-sat` (bin): SAT satisfiability checks and model output using the same specification.

The built-in symbols include the following equations:

- $Bxyz = x(yz)$
- $Cxyz = xzy$
- $Ix = x$
- $Kxy = x$
- $Mx = xx$
- $Sxyz = xz(yz)$
- $Wxy = xyy$
- $Yx = x(Yx)$

Here, the binary operation is left associative: $abc = (ab)c$.

The tools use the SAT solver [CaDiCaL](https://github.com/arminbiere/cadical) through [RustSAT](https://github.com/chrjabs/rustsat).

## Installation

### Install dependencies

```bash
# Rust toolchain
curl https://sh.rustup.rs -sSf | sh
source $HOME/.cargo/bin/env

# CaDiCaL's dependencies
sudo apt update
sudo apt install -y g++ libclang-dev
```

### Build

```bash
git clone https://github.com/hyperfinitism/magma-search
pushd magma-search
    cargo build --all-targets --release
popd
```

## Specifications

Both `magma-iso` and `magma-sat` require either `--size N` for the size of the underlying set, or both `--min N` and `--max N` for its range.
Use `--spec FILE` to provide a specification of magmas in JSON format:

```json
{
  "symbols": [
    {
      "symbol": "B",
      "equations": ["Bxyz = x(yz)"],
      "presence": "present"
    },
    {
      "symbol": "M",
      "equations": ["Mx = xx"],
      "presence": "present"
    },
    {
      "symbol": "Y",
      "equations": ["Yx = x(Yx)"],
      "presence": "absent"
    }
  ]
}
```

The `presence` field is `"present"`, `"absent"` or `"any"` (the default).
All equations of a symbol must hold for the same candidate element, under every assignment of their variables.
`"absent"` means that no element satisfies all of those equations simultaneously.

Use `--fix SYMBOL=INDEX[,INDEX...]` to require a witness at one of the specified element indices.
For example, both tools accept `--fix B=0 --fix M=2,3`, requiring element `0` to satisfy the equations of `B` and at least one of elements `2` and `3` to satisfy those of `M`.
Other elements may also satisfy these equations.
Element indices start at `0` and must be smaller than every requested size, so this example requires `--size 4` or a range with `--min` at least `4`.

The equivalent JSON field is an optional, nonempty `values` list:

```json
{
  "symbols": [
    {"symbol": "B", "equations": ["Bxyz = x(yz)"], "values": [0]},
    {"symbol": "M", "equations": ["Mx = xx"], "values": [2, 3]}
  ]
}
```

`values` implies `"presence": "present"`; combining it with `"absent"` or excluding that symbol is invalid.
`--fix` overrides a symbol's file `values` list.
If neither a `values` list nor `--fix` is specified for a symbol, its witnesses are unrestricted apart from its presence condition.

Terms consist of uppercase single-letter constants, lowercase single-letter variables, applications, and parentheses.
For long identifiers, use `[name]` for constants and `{name}` for variables.

`--include` requires presence and `--exclude` requires absence.
Each accepts a comma-separated list of constant symbols.
Without a specification file, built-in rules are used.

## magma-iso

The `magma-iso` command enumerates isomorphism classes of magmas that satisfy the specified conditions and have the specified size, using an exhaustive search.

```bash
# Search for BMI-algebras of size 1 to 4
magma-iso --min 1 --max 4 --include B,M,I --out out/bm

# Search for BCI-algebras of size 1 to 4 w/o Y
magma-iso --min 1 --max 4 --include B,C,I --exclude Y --out out/bc-wo-y

# Use a specification at one specific size
magma-iso --size 3 --spec samples/bm-wo-y.json --out out/bm-wo-y-spec

# Use SAT to decide whether two tables are isomorphic
magma-iso --size 3 --spec samples/bm-wo-y.json --isomorphism sat --out out/bm-wo-y-sat
```

Directories named `size%d` are created under the directory specified by `--out`, containing one JSON operation table per isomorphism class and the candidate elements of each symbol.
`summary.json` includes the specification, the counts of examined and matching tables, and the number of isomorphism classes.

`--isomorphism permutation|sat` is used to select the method for determining isomorphism between two tables; the default is `permutation`.
The permutation method (`permutation`) simply checks every possible permutation.
The SAT method (`sat`) reduces the existence of an isomorphism to a SAT problem by treating a mapping as $n^{2}$ Boolean variables.
Both methods enumerate the same classes. Every SAT isomorphism check uses a single solver thread.
Fixed witness indices filter the labeled operation tables; isomorphism remains an isomorphism of the binary operation, without distinguished constants.
Each output representative is the lexicographically smallest table in its class that satisfies the search conditions, which can differ from the unrestricted canonical table when indices are fixed.
`labeled_magmas_in_class` counts only the labeled tables that satisfy those conditions.

`--processes N` divides all possible magmas into batches and performs a parallel search using multiple processes.
The default value is the maximum number of logical cores.

## magma-sat

The `magma-sat` command translates constraints on finite magmas into SAT problems and solves them using an SAT solver.

```bash
# Find BM-algebras without Y
magma-sat --min 1 --max 10 --include B,M --exclude Y --out out/bm-wo-y-sat

# Also output CNF files
magma-sat --min 1 --max 10 --include B,M --exclude Y --out out/bm-wo-y-cnf --emit-cnf

# Use specification files
magma-sat --size 2 --spec samples/bm-wo-y.json --out out/bm-wo-y-spec
```

The results are saved in `size%d.json` under `--out`.
Each output file contains the specification, the SAT result (satisfiable, unsatisfiable or unknown), the size of the translated SAT problem and, if the problem is satisfiable, the operation table and satisfying elements of each symbol.
`summary.json` includes the specification and results for all requested sizes.

If the `--emit-cnf` flag is specified, the translated SAT problems in conjunctive normal form (CNF) are also output to `size%d.cnf`, which can be used by other SAT solvers.

`--threads N` runs parallel searches of the same SAT problem (portfolio parallelism); the default is the number of available logical CPUs.
