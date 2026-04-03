# Script Validation Benchmark Report

This report summarizes the results produced by [`script_validation.rs`](./script_validation.rs).

## Setup

Four block-shaped workloads were benchmarked under the `500,000` compute-mass block limit:

- `chess_mix`
  - A repeated cycle of real chess-app transactions built from the silverscript test fixtures:
    - `pawn_apply`
    - `route`
    - `league_register_player`
    - `player_start_game`
    - `settle`
- `schnorr_2in1`
  - Repeated ordinary v0 2-input / 1-output schnorr transactions
- `op_dup_243`
  - Repeated 1-input / 0-output transactions spending UTXOs whose script pub key is `1` followed by `243` `OP_DUP`s
  - Benchmarked with `covenants_enabled = true`
- `op_dup_one_tx`
  - One 1-input / 0-output transaction spending a UTXO whose script pub key starts with `1`, then `243` `OP_DUP`s, then repeated `OP_DROP OP_DUP`
  - Growth stops at the last script that still executes under txscript's opcode limit
  - Benchmarked with `covenants_enabled = true`

Validation modes:

- single-threaded sequential execution
- rayon per-input parallel execution with:
  - `2` threads
  - `4` threads
  - `8` threads
  - `16` threads

The parallel execution model mirrors the per-input rayon shape used by rusty-kaspa transaction validation.

## Current Committed Pricing

Pricing parameters:

- `GRAMS_PER_COMPUTE_BUDGET_UNIT = 100`
- `SCRIPT_UNITS_PER_GRAM = 10`

Packed blocks:

- `chess_mix`
  - `49` txs
  - `77` inputs
  - `498,452` compute mass
- `schnorr_2in1`
  - `182` txs
  - `364` inputs
  - `499,044` compute mass
- `op_dup_243`
  - `3378` txs
  - `3378` inputs
  - `499,944` compute mass
- `op_dup_one_tx`
  - `1` tx
  - `1` input
  - `1,148` compute mass

Results:

| Mode | Chess mix | Schnorr 2:1 | OpDup 243 | OpDup One Tx |
|---|---:|---:|---:|---:|
| single-thread | `5.1131 ms` | `9.1976 ms` | `33.329 ms` | `407.56 µs` |
| rayon 2 | `6.9869 ms` | `7.5316 ms` | `61.591 ms` | `481.20 µs` |
| rayon 4 | `4.9948 ms` | `9.3913 ms` | `75.785 ms` | `437.48 µs` |
| rayon 8 | `5.6962 ms` | `11.459 ms` | `63.925 ms` | `453.10 µs` |
| rayon 16 | `5.5362 ms` | `12.209 ms` | `65.919 ms` | `468.41 µs` |

Observation:

- `op_dup_243` is by far the slowest equally mass-packed workload in every measured mode.
- `chess_mix` is faster than `schnorr_2in1` in every measured mode.
- `op_dup_one_tx` is the fastest in absolute time, but it is not an equally mass-packed block: it only reaches `1,148` compute mass before hitting txscript's opcode limit.

## Squeezed Pricing

Follow-up run after changing:

- `SCRIPT_UNITS_PER_GRAM = 100`

and rerunning the same benchmark.

Packed blocks:

- `chess_mix`
  - `67` txs
  - `104` inputs
  - `498,854` compute mass
- `schnorr_2in1`
  - `182` txs
  - `364` inputs
  - `499,044` compute mass

Results:

| Mode | Chess mix | Schnorr 2:1 |
|---|---:|---:|
| single-thread | `7.0639 ms` | `8.1064 ms` |
| rayon 2 | `5.5008 ms` | `5.8978 ms` |
| rayon 4 | `5.5445 ms` | `7.2776 ms` |
| rayon 8 | `5.7584 ms` | `7.8232 ms` |
| rayon 16 | `5.9207 ms` | `8.2916 ms` |

Observed criterion change output for `chess_mix` vs the current committed pricing run:

- single-thread
  - `+33.568%` to `+34.243%`
- rayon 2
  - `+30.702%` to `+35.650%`
- rayon 4
  - `+31.253%` to `+37.148%`
- rayon 8
  - `+22.614%` to `+30.900%`
- rayon 16
  - `+30.391%` to `+39.065%`

Observation:

- With `SCRIPT_UNITS_PER_GRAM = 100`, chess is still faster than the ordinary schnorr block in every mode.
- However, the chess block becomes denser under the same mass cap, and total validation time increases materially.

## Summary

- Current committed pricing makes the equally mass-packed `op_dup_243` workload dramatically slower than both chess-heavy blocks and ordinary schnorr blocks.
- Current committed pricing still allows chess-heavy blocks to validate faster than equally mass-packed ordinary schnorr blocks in this run.
- The `op_dup_one_tx` variant does not approach the block mass limit because txscript's opcode limit stops script growth at `1,148` compute mass.
- Increasing `SCRIPT_UNITS_PER_GRAM` to `100` fits more chess transactions into the same block mass budget.
- That squeeze does not make chess slower than schnorr, but it does increase total chess-block validation time by roughly `27%` to `35%` depending on the thread count.
