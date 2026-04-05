# Script Validation Benchmark Report

This report summarizes the results produced by [`script_validation.rs`](./script_validation.rs).

## Setup

The current benchmark configuration covers five block-shaped workloads under the `500,000` compute-mass block limit:

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
  - Repeated 1-input / 0-output transactions whose script path executes `243` `OP_DUP`s directly
- `op_dup_243_p2sh`
  - Repeated 1-input / 0-output transactions spending P2SH UTXOs whose redeem script is `1` followed by `243` `OP_DUP`s
  - Benchmarked with `covenants_enabled = true`
- `op_dup_one_tx`
  - Single-transaction workload used as a low-parallelism control case

Validation modes:

- single-threaded sequential execution
- rayon execution that parallelizes both transactions and inputs with:
  - `2` threads
  - `4` threads
  - `8` threads
  - `16` threads

The parallel execution model now exposes both transaction-level and input-level work to rayon.

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
- `op_dup_243_p2sh`
  - `1269` txs
  - `1269` inputs
  - `499,986` compute mass
- `op_dup_one_tx`
  - `1` tx
  - `1` input
  - `10,148` compute mass

Results:

| Mode | Chess mix | Schnorr 2:1 | OpDup 243 | OpDup 243 P2SH | OpDup one tx |
|---|---:|---:|---:|---:|---:|
| single-thread | `4.8966 ms` | `9.2447 ms` | `19.893 ms` | `7.6275 ms` | `3.4308 ms` |
| rayon 2 | `3.0924 ms` | `5.7065 ms` | `18.440 ms` | `7.2740 ms` | `3.5274 ms` |
| rayon 4 | `1.9060 ms` | `3.8327 ms` | `16.535 ms` | `6.3804 ms` | `3.5653 ms` |
| rayon 8 | `1.4038 ms` | `3.2152 ms` | `17.626 ms` | `6.0023 ms` | `3.5212 ms` |
| rayon 16 | `1.1011 ms` | `2.5905 ms` | `13.454 ms` | `5.9995 ms` | `3.5215 ms` |

Observation:

- `chess_mix` remains the fastest workload in every measured mode.
- `schnorr_2in1` is consistently faster than both op-dup block-filling workloads, but slower than `chess_mix`.
- `op_dup_243` remains slower than `op_dup_243_p2sh` in every measured mode.
- `op_dup_one_tx` improves versus the previous baseline in every mode, but because it contains only one transaction and one input, rayon provides no meaningful additional speedup over single-threaded execution.

Observed criterion change output versus the previous committed benchmark baseline:

- `chess_mix`
  - single-thread: regressed by `+1.9954%` to `+2.3179%`
  - rayon 2: improved by `-3.7505%` to `-1.8767%`
  - rayon 4: improved by `-5.3232%` to `-3.2813%`
  - rayon 8: no significant change
  - rayon 16: change within noise threshold, with point estimate `-2.2641%`
- `schnorr_2in1`
  - single-thread: regressed by `+1.3695%` to `+3.6818%`
  - rayon 2: improved by `-3.4989%` to `-2.3707%`
  - rayon 4: no significant change
  - rayon 8: improved by `-7.3532%` to `-5.4045%`
  - rayon 16: regressed by `+1.0978%` to `+2.7281%`
- `op_dup_243`
  - single-thread: regressed by `+9.0855%` to `+13.501%`
  - rayon 2: regressed by `+7.6848%` to `+9.4003%`
  - rayon 4: regressed by `+37.234%` to `+39.487%`
  - rayon 8: regressed by `+24.874%` to `+26.732%`
  - rayon 16: improved by `-10.214%` to `-9.0384%`
- `op_dup_243_p2sh`
  - single-thread: improved by `-35.245%` to `-34.697%`
  - rayon 2: improved by `-24.616%` to `-23.599%`
  - rayon 4: improved by `-11.957%` to `-10.982%`
  - rayon 8: improved by `-15.529%` to `-13.222%`
  - rayon 16: improved by `-8.0084%` to `-5.4867%`
- `op_dup_one_tx`
  - single-thread: improved by `-17.944%` to `-15.588%`
  - rayon 2: improved by `-22.257%` to `-20.537%`
  - rayon 4: improved by `-20.614%` to `-18.392%`
  - rayon 8: improved by `-18.605%` to `-16.932%`
  - rayon 16: improved by `-22.464%` to `-20.741%`

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

- With rayon parallelizing both transactions and inputs, the chess-heavy block still validates substantially faster than the equally mass-packed schnorr and op-dup blocks.
- In this configuration, direct `op_dup_243` is the slowest block-filling workload in every measured mode, and `op_dup_243_p2sh` is consistently faster than direct `op_dup_243`.
- The latest run is mixed rather than uniformly better or worse: chess and schnorr improve in some rayon modes while regressing slightly in single-threaded mode, direct `op_dup_243` regresses sharply except at `16` threads, and `op_dup_243_p2sh` plus `op_dup_one_tx` improve across all reported modes.
- Increasing `SCRIPT_UNITS_PER_GRAM` to `100` fits more chess transactions into the same block mass budget.
- That squeeze does not make chess slower than schnorr, but it does increase total chess-block validation time by roughly `27%` to `35%` depending on the thread count.
