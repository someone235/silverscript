# Script Validation Benchmark Report

This report summarizes the results produced by [`script_validation.rs`](./script_validation.rs).

## Setup

Two block-shaped workloads were benchmarked under the `500,000` compute-mass block limit:

- `chess_mix`
  - A repeated cycle of real chess-app transactions built from the silverscript test fixtures:
    - `pawn_apply`
    - `route`
    - `league_register_player`
    - `player_start_game`
    - `settle`
- `schnorr_2in1`
  - Repeated ordinary v0 2-input / 1-output schnorr transactions

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

Results:

| Mode | Chess mix | Schnorr 2:1 |
|---|---:|---:|
| single-thread | `5.2756 ms` | `8.1014 ms` |
| rayon 2 | `4.1305 ms` | `5.9937 ms` |
| rayon 4 | `4.1315 ms` | `7.2356 ms` |
| rayon 8 | `4.5441 ms` | `8.0068 ms` |
| rayon 16 | `4.3964 ms` | `8.1052 ms` |

Observation:

- At the current committed pricing, the chess block validates faster than the ordinary schnorr block in every measured mode.

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

- Current committed pricing already allows chess-heavy blocks to validate faster than equally mass-packed ordinary schnorr blocks.
- Increasing `SCRIPT_UNITS_PER_GRAM` to `100` fits more chess transactions into the same block mass budget.
- That squeeze does not make chess slower than schnorr, but it does increase total chess-block validation time by roughly `27%` to `35%` depending on the thread count.
