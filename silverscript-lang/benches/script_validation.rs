use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use blake2b_simd::Params as Blake2bParams;
use criterion::{BenchmarkId, Criterion, SamplingMode, black_box, criterion_group, criterion_main};
use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::Hash;
use kaspa_consensus_core::config::params::MAINNET_PARAMS;
use kaspa_consensus_core::hashing::sighash::{SigHashReusedValuesSync, SigHashReusedValuesUnsync, calc_schnorr_signature_hash};
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::mass::{ComputeBudget, Gram, MassCalculator, ScriptUnits, free_script_units_per_input};
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput,
    TransactionOutpoint, TransactionOutput, TxInputMass, UtxoEntry, VerifiableTransaction,
};
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::{OpDrop, OpDup};
use kaspa_txscript::script_builder::ScriptBuilder;
use kaspa_txscript::{
    EngineCtx, EngineFlags, TxScriptEngine, pay_to_address_script, pay_to_script_hash_script, pay_to_script_hash_signature_script,
};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use silverscript_lang::ast::Expr;
use silverscript_lang::compiler::{CompileOptions, CompiledContract, compile_contract};

const BLOCK_COMPUTE_MASS_LIMIT: u64 = 500_000;

struct Player {
    keypair: Keypair,
    pubkey_bytes: Vec<u8>,
    owner_hash: Hash,
    player_id: Hash,
    player_ref: Hash,
}

struct TemplateFixture {
    source: &'static str,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    hash: Hash,
}

struct MuxChessFixture {
    mux: TemplateFixture,
    settle: TemplateFixture,
    pawn: TemplateFixture,
    knight: TemplateFixture,
    vert: TemplateFixture,
    horiz: TemplateFixture,
    diag: TemplateFixture,
    king: TemplateFixture,
    castle: TemplateFixture,
    castle_challenge: TemplateFixture,
}

struct GameStateArgs<'a> {
    board: &'a [u8],
    turn: i64,
    status: i64,
    castle_rights: [u8; 4],
    en_passant_idx: i64,
    pending_src_idx: i64,
    pending_dst_idx: i64,
    pending_promo: i64,
    recent_castle: i64,
    draw_state: i64,
}

struct PlayerStateArgs<'a> {
    league_template: &'a Hash,
    player_template: &'a Hash,
    mux_template: &'a Hash,
    routes_commitment: &'a Hash,
    owner_hash: &'a Hash,
    player_id: &'a Hash,
    open_games: i64,
    rating: i64,
    games: i64,
    wins: i64,
    draws: i64,
    losses: i64,
}

struct MoveArgs {
    from_x: i64,
    from_y: i64,
    to_x: i64,
    to_y: i64,
    promo_piece: i64,
}

struct BenchTx {
    tx: MutableTransaction<Transaction>,
    cov_ctx: CovenantsContext,
    covenants_enabled: bool,
}

struct BenchBlock {
    name: &'static str,
    txs: Vec<BenchTx>,
    tx_count: usize,
    input_count: usize,
    compute_mass: u64,
}

fn apps_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/apps/chess")
}

fn source_cache() -> &'static Mutex<HashMap<String, &'static str>> {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn compiled_contract_cache() -> &'static Mutex<HashMap<String, Arc<CompiledContract<'static>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<CompiledContract<'static>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn compile_cache_key(source: &'static str, ctor: &[Expr<'static>]) -> String {
    format!("{:p}:{}:{}", source.as_ptr(), source.len(), serde_json::to_string(ctor).expect("serialize ctor args"))
}

fn compile_cached(source: &'static str, ctor: &[Expr<'static>]) -> Arc<CompiledContract<'static>> {
    let key = compile_cache_key(source, ctor);
    {
        let cache = compiled_contract_cache().lock().expect("compile cache mutex poisoned");
        if let Some(compiled) = cache.get(&key) {
            return Arc::clone(compiled);
        }
    }

    let compiled = Arc::new(compile_contract(source, ctor, CompileOptions::default()).expect("compile contract succeeds"));
    let mut cache = compiled_contract_cache().lock().expect("compile cache mutex poisoned");
    cache.insert(key, Arc::clone(&compiled));
    compiled
}

fn contract_path(name: &str) -> PathBuf {
    apps_root().join(name)
}

fn load_contract_source(path: &Path) -> &'static str {
    let key = path.display().to_string();
    {
        let cache = source_cache().lock().expect("source cache mutex poisoned");
        if let Some(source) = cache.get(&key) {
            return source;
        }
    }

    let source = fs::read_to_string(path).unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    let leaked: &'static str = Box::leak(source.into_boxed_str());
    let mut cache = source_cache().lock().expect("source cache mutex poisoned");
    cache.insert(key, leaked);
    leaked
}

fn local_contract_source(name: &str) -> &'static str {
    load_contract_source(&contract_path(name))
}

fn mux_source() -> &'static str {
    local_contract_source("chess_mux.sil")
}

fn settle_source() -> &'static str {
    local_contract_source("chess_settle.sil")
}

fn league_source() -> &'static str {
    local_contract_source("league.sil")
}

fn player_source() -> &'static str {
    local_contract_source("player.sil")
}

fn pawn_source() -> &'static str {
    local_contract_source("chess_pawn.sil")
}

fn knight_source() -> &'static str {
    local_contract_source("chess_knight.sil")
}

fn vert_source() -> &'static str {
    local_contract_source("chess_vert.sil")
}

fn horiz_source() -> &'static str {
    local_contract_source("chess_horiz.sil")
}

fn diag_source() -> &'static str {
    local_contract_source("chess_diag.sil")
}

fn king_source() -> &'static str {
    local_contract_source("chess_king.sil")
}

fn castle_source() -> &'static str {
    local_contract_source("chess_castle.sil")
}

fn castle_challenge_source() -> &'static str {
    local_contract_source("chess_castle_challenge.sil")
}

fn blake2b_bytes(data: &[u8]) -> Hash {
    Hash::from_slice(Blake2bParams::new().hash_length(32).to_state().update(data).finalize().as_bytes())
}

fn hash_bytes(value: Hash) -> Vec<u8> {
    value.as_bytes().to_vec()
}

fn hash_pair(left: Hash, right: Hash) -> Hash {
    blake2b_bytes(&[left.as_bytes().as_slice(), right.as_bytes().as_slice()].concat())
}

fn hash_expr(value: Hash) -> Expr<'static> {
    Expr::bytes(hash_bytes(value))
}

fn repeated_hash(byte: u8) -> Hash {
    Hash::from_bytes([byte; 32])
}

fn player_ref(owner_hash: Hash, player_id: Hash) -> Hash {
    hash_pair(owner_hash, player_id)
}

fn player_from_seed(seed: u8) -> Player {
    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(&[seed; 32]).expect("valid deterministic secret");
    let keypair = Keypair::from_secret_key(&secp, &secret);
    let (x_only, _) = keypair.x_only_public_key();
    let pubkey_bytes = x_only.serialize().to_vec();
    let owner_hash = blake2b_bytes(&pubkey_bytes);
    let player_id = blake2b_bytes(&[b"bench-player-id".as_slice(), pubkey_bytes.as_slice()].concat());
    let player_ref = player_ref(owner_hash, player_id);
    Player { keypair, pubkey_bytes, owner_hash, player_id, player_ref }
}

fn standard_board() -> Vec<u8> {
    vec![
        0x04, 0x02, 0x03, 0x05, 0x06, 0x03, 0x02, 0x04, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x0c, 0x0a, 0x0b, 0x0d, 0x0e, 0x0b, 0x0a,
        0x0c,
    ]
}

fn square_idx(x: i64, y: i64) -> i64 {
    y * 8 + x
}

fn full_castle_rights() -> [u8; 4] {
    [1, 1, 1, 1]
}

fn castle_rights_expr(rights: [u8; 4]) -> Expr<'static> {
    Expr::bytes(rights.to_vec())
}

fn move_piece(board: &mut [u8], from_x: usize, from_y: usize, to_x: usize, to_y: usize) {
    let from_idx = from_y * 8 + from_x;
    let to_idx = to_y * 8 + to_x;
    let piece = board[from_idx];
    board[from_idx] = 0x00;
    board[to_idx] = piece;
}

fn mv(from_x: i64, from_y: i64, to_x: i64, to_y: i64) -> MoveArgs {
    MoveArgs { from_x, from_y, to_x, to_y, promo_piece: 0 }
}

fn packed_route_templates(fix: &MuxChessFixture) -> Vec<u8> {
    let player_template = player_template_hash(fix);
    let mut out = Vec::with_capacity(32 * 9);
    out.extend_from_slice(&fix.pawn.hash.as_bytes());
    out.extend_from_slice(&fix.knight.hash.as_bytes());
    out.extend_from_slice(&fix.vert.hash.as_bytes());
    out.extend_from_slice(&fix.horiz.hash.as_bytes());
    out.extend_from_slice(&fix.diag.hash.as_bytes());
    out.extend_from_slice(&fix.king.hash.as_bytes());
    out.extend_from_slice(&fix.castle.hash.as_bytes());
    out.extend_from_slice(&fix.castle_challenge.hash.as_bytes());
    let settle_commitment = Blake2bParams::new()
        .hash_length(32)
        .to_state()
        .update(&fix.settle.hash.as_bytes())
        .update(&player_template.as_bytes())
        .finalize()
        .as_bytes()
        .to_vec();
    out.extend_from_slice(&settle_commitment);
    out
}

fn routes_commitment(route_templates: &[u8]) -> Hash {
    blake2b_bytes(route_templates)
}

fn template_fixture(source: &'static str, ctor: &[Expr<'static>]) -> TemplateFixture {
    let compiled = compile_cached(source, ctor);
    let layout = compiled.state_layout;
    let prefix = compiled.script[..layout.start].to_vec();
    let suffix = compiled.script[layout.start + layout.len..].to_vec();
    let hash = blake2b_bytes(&[prefix.as_slice(), suffix.as_slice()].concat());
    TemplateFixture { source, prefix, suffix, hash }
}

fn fixture() -> &'static MuxChessFixture {
    static FIXTURE: OnceLock<MuxChessFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dummy_board = standard_board();
        let game_ctor = vec![
            Expr::bytes(vec![0x11u8; 32]),
            Expr::bytes(vec![0x33u8; 32 * 9]),
            Expr::bytes(vec![0x21u8; 32]),
            Expr::bytes(vec![0x22u8; 32]),
            Expr::bytes(dummy_board),
            Expr::int(0),
            Expr::int(0),
            Expr::int(600),
            castle_rights_expr(full_castle_rights()),
            Expr::int(-1),
            Expr::int(-1),
            Expr::int(-1),
            Expr::int(0),
            Expr::int(0),
            Expr::int(3),
        ];
        let settle_ctor =
            vec![Expr::bytes(vec![0x44u8; 32]), Expr::bytes(vec![0x21u8; 32]), Expr::bytes(vec![0x22u8; 32]), Expr::int(0)];

        MuxChessFixture {
            mux: template_fixture(mux_source(), &game_ctor),
            settle: template_fixture(settle_source(), &settle_ctor),
            pawn: template_fixture(pawn_source(), &game_ctor),
            knight: template_fixture(knight_source(), &game_ctor),
            vert: template_fixture(vert_source(), &game_ctor),
            horiz: template_fixture(horiz_source(), &game_ctor),
            diag: template_fixture(diag_source(), &game_ctor),
            king: template_fixture(king_source(), &game_ctor),
            castle: template_fixture(castle_source(), &game_ctor),
            castle_challenge: template_fixture(castle_challenge_source(), &game_ctor),
        }
    })
}

fn compile_state(
    source: &'static str,
    fix: &MuxChessFixture,
    white_hash: &Hash,
    black_hash: &Hash,
    state: GameStateArgs<'_>,
) -> Arc<CompiledContract<'static>> {
    let ctor = vec![
        hash_expr(fix.mux.hash),
        Expr::bytes(packed_route_templates(fix)),
        hash_expr(*white_hash),
        hash_expr(*black_hash),
        Expr::bytes(state.board.to_vec()),
        Expr::int(state.turn),
        Expr::int(state.status),
        Expr::int(600),
        castle_rights_expr(state.castle_rights),
        Expr::int(state.en_passant_idx),
        Expr::int(state.pending_src_idx),
        Expr::int(state.pending_dst_idx),
        Expr::int(state.pending_promo),
        Expr::int(state.recent_castle),
        Expr::int(state.draw_state),
    ];
    compile_cached(source, &ctor)
}

fn compile_settle_state(
    source: &'static str,
    player_template: &Hash,
    white_hash: &Hash,
    black_hash: &Hash,
    status: i64,
) -> Arc<CompiledContract<'static>> {
    let ctor = vec![hash_expr(*player_template), hash_expr(*white_hash), hash_expr(*black_hash), Expr::int(status)];
    compile_cached(source, &ctor)
}

fn compile_player_state(source: &'static str, state: PlayerStateArgs<'_>) -> Arc<CompiledContract<'static>> {
    let ctor = vec![
        hash_expr(*state.league_template),
        hash_expr(*state.player_template),
        hash_expr(*state.mux_template),
        hash_expr(*state.routes_commitment),
        hash_expr(*state.owner_hash),
        hash_expr(*state.player_id),
        Expr::int(state.open_games),
        Expr::int(state.rating),
        Expr::int(state.games),
        Expr::int(state.wins),
        Expr::int(state.draws),
        Expr::int(state.losses),
    ];
    compile_cached(source, &ctor)
}

fn player_template_hash(fix: &MuxChessFixture) -> Hash {
    let compiled = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &repeated_hash(0x11),
            player_template: &repeated_hash(0x22),
            mux_template: &fix.mux.hash,
            routes_commitment: &repeated_hash(0x33),
            owner_hash: &repeated_hash(0x44),
            player_id: &repeated_hash(0x55),
            open_games: 0,
            rating: 1200,
            games: 0,
            wins: 0,
            draws: 0,
            losses: 0,
        },
    );
    let layout = compiled.state_layout;
    blake2b_bytes(&[compiled.script[..layout.start].as_ref(), compiled.script[layout.start + layout.len..].as_ref()].concat())
}

fn entry_sigscript(compiled: &CompiledContract<'_>, function: &str, args: Vec<Expr<'_>>) -> Vec<u8> {
    let sigscript = compiled.build_sig_script(function, args).expect("sigscript builds");
    pay_to_script_hash_signature_script(compiled.script.clone(), sigscript).expect("wrap p2sh sigscript")
}

fn input_outpoint(index: u32, nonce: u32) -> TransactionOutpoint {
    TransactionOutpoint {
        transaction_id: TransactionId::from_slice(
            &blake2b_bytes(&[nonce.to_le_bytes().as_slice(), index.to_le_bytes().as_slice()].concat()).as_bytes(),
        ),
        index,
    }
}

fn tx_input(index: u32, nonce: u32, signature_script: Vec<u8>) -> TransactionInput {
    TransactionInput { previous_outpoint: input_outpoint(index, nonce), signature_script, sequence: 0, mass: ComputeBudget(0).into() }
}

fn covenant_output_with_value(
    compiled: &CompiledContract<'_>,
    authorizing_input: u16,
    covenant_id: Hash,
    value: u64,
) -> TransactionOutput {
    TransactionOutput {
        value,
        script_public_key: pay_to_script_hash_script(&compiled.script),
        covenant: Some(CovenantBinding { authorizing_input, covenant_id }),
    }
}

fn covenant_output(compiled: &CompiledContract<'_>, authorizing_input: u16, covenant_id: Hash) -> TransactionOutput {
    covenant_output_with_value(compiled, authorizing_input, covenant_id, 1_000)
}

fn covenant_utxo_with_value(compiled: &CompiledContract<'_>, covenant_id: Hash, value: u64) -> UtxoEntry {
    UtxoEntry::new(value, pay_to_script_hash_script(&compiled.script), 0, false, Some(covenant_id))
}

fn covenant_utxo(compiled: &CompiledContract<'_>, covenant_id: Hash) -> UtxoEntry {
    covenant_utxo_with_value(compiled, covenant_id, 1_000)
}

fn bench_flags(covenants_enabled: bool) -> EngineFlags {
    EngineFlags { covenants_enabled, sigop_script_units: Gram(1000).to_script_units().value() }
}

fn sign_tx_input_schnorr(tx: &Transaction, entries: &[UtxoEntry], input_idx: usize, player: &Player) -> Vec<u8> {
    let reused_values = SigHashReusedValuesUnsync::new();
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let sig_hash = calc_schnorr_signature_hash(&populated, input_idx, SIG_HASH_ALL, &reused_values);
    let msg = Message::from_digest_slice(sig_hash.as_bytes().as_slice()).expect("valid sighash");
    let sig = player.keypair.sign_schnorr(msg);
    let mut signature = Vec::with_capacity(65);
    signature.extend_from_slice(sig.as_ref());
    signature.push(SIG_HASH_ALL.to_u8());
    signature
}

fn budgeted_chess_tx(label: &str, tx: &Transaction, entries: &[UtxoEntry]) -> Transaction {
    let reused_values = SigHashReusedValuesUnsync::new();
    let sig_cache = Cache::new(10_000);
    let flags = bench_flags(true);
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let cov_ctx = CovenantsContext::from_tx(&populated).expect("covenants ctx");
    let mut budgeted_tx = tx.clone();

    for (input_idx, input) in tx.inputs.iter().enumerate() {
        let utxo = populated.utxo(input_idx).expect("input utxo");
        let mut vm = kaspa_txscript::TxScriptEngine::from_transaction_input_with_allowed_script_units(
            &populated,
            input,
            input_idx,
            utxo,
            EngineCtx::new(&sig_cache).with_reused(&reused_values).with_covenants_ctx(&cov_ctx),
            flags,
            u64::MAX,
        );
        vm.execute().unwrap_or_else(|err| panic!("failed to measure {label} input #{input_idx}: {err}"));
        let compute_budget = ComputeBudget::checked_covering_script_units(ScriptUnits(vm.used_script_units()))
            .unwrap_or_else(|| panic!("required compute budget does not fit for {label} input #{input_idx}"));
        budgeted_tx.inputs[input_idx].mass = compute_budget.into();
    }

    budgeted_tx
}

fn mass_calculator() -> &'static MassCalculator {
    static CALCULATOR: OnceLock<MassCalculator> = OnceLock::new();
    CALCULATOR.get_or_init(|| MassCalculator::new_with_consensus_params(&MAINNET_PARAMS))
}

fn prepare_bench_tx(tx: Transaction, entries: Vec<UtxoEntry>, covenants_enabled: bool) -> BenchTx {
    let tx = MutableTransaction::with_entries(tx, entries);
    let cov_ctx =
        if covenants_enabled { CovenantsContext::from_tx(&tx.as_verifiable()).expect("covenants ctx") } else { Default::default() };
    BenchTx { tx, cov_ctx, covenants_enabled }
}

fn compute_mass(tx: &Transaction) -> u64 {
    mass_calculator().calc_non_contextual_masses(tx).compute_mass
}

fn build_route_tx(nonce: u32) -> (Transaction, Vec<UtxoEntry>) {
    let fix = fixture();
    let white = player_from_seed(1);
    let black = player_from_seed(2);
    let board0 = standard_board();
    let covenant_id = repeated_hash((0x81u32.wrapping_add(nonce)) as u8);

    let mux0 = compile_state(
        fix.mux.source,
        fix,
        &white.player_ref,
        &black.player_ref,
        GameStateArgs {
            board: &board0,
            turn: 0,
            status: 0,
            castle_rights: full_castle_rights(),
            en_passant_idx: -1,
            pending_src_idx: -1,
            pending_dst_idx: -1,
            pending_promo: 0,
            recent_castle: 0,
            draw_state: 3,
        },
    );
    let pawn0 = compile_state(
        fix.pawn.source,
        fix,
        &white.player_ref,
        &black.player_ref,
        GameStateArgs {
            board: &board0,
            turn: 0,
            status: 0,
            castle_rights: full_castle_rights(),
            en_passant_idx: -1,
            pending_src_idx: square_idx(4, 1),
            pending_dst_idx: square_idx(4, 3),
            pending_promo: 0,
            recent_castle: 0,
            draw_state: 3,
        },
    );

    let mv = mv(4, 1, 4, 3);
    let placeholder_sigscript = entry_sigscript(
        &mux0,
        "route",
        vec![
            0.into(),
            mv.from_x.into(),
            mv.from_y.into(),
            mv.to_x.into(),
            mv.to_y.into(),
            mv.promo_piece.into(),
            0.into(),
            Expr::bytes(vec![0u8; 65]),
            Expr::bytes(white.pubkey_bytes.clone()),
            hash_expr(white.player_id),
            Expr::bytes(fix.pawn.prefix.clone()),
            Expr::bytes(fix.pawn.suffix.clone()),
        ],
    );
    let entries = vec![covenant_utxo(&mux0, covenant_id)];
    let outputs = vec![covenant_output(&pawn0, 0, covenant_id)];
    let mut tx = Transaction::new(1, vec![tx_input(0, nonce, placeholder_sigscript)], outputs, 0, Default::default(), 0, vec![]);
    let sig = sign_tx_input_schnorr(&tx, &entries, 0, &white);
    tx.inputs[0].signature_script = entry_sigscript(
        &mux0,
        "route",
        vec![
            0.into(),
            mv.from_x.into(),
            mv.from_y.into(),
            mv.to_x.into(),
            mv.to_y.into(),
            mv.promo_piece.into(),
            0.into(),
            Expr::bytes(sig),
            Expr::bytes(white.pubkey_bytes),
            hash_expr(white.player_id),
            Expr::bytes(fix.pawn.prefix.clone()),
            Expr::bytes(fix.pawn.suffix.clone()),
        ],
    );
    (budgeted_chess_tx("route", &tx, &entries), entries)
}

fn build_pawn_apply_tx(nonce: u32) -> (Transaction, Vec<UtxoEntry>) {
    let fix = fixture();
    let white = player_from_seed(1);
    let black = player_from_seed(2);
    let board0 = standard_board();
    let covenant_id = repeated_hash((0x82u32.wrapping_add(nonce)) as u8);

    let pawn0 = compile_state(
        fix.pawn.source,
        fix,
        &white.player_ref,
        &black.player_ref,
        GameStateArgs {
            board: &board0,
            turn: 0,
            status: 0,
            castle_rights: full_castle_rights(),
            en_passant_idx: -1,
            pending_src_idx: square_idx(4, 1),
            pending_dst_idx: square_idx(4, 3),
            pending_promo: 0,
            recent_castle: 0,
            draw_state: 3,
        },
    );
    let mut board1 = board0.clone();
    move_piece(&mut board1, 4, 1, 4, 3);
    let mux1 = compile_state(
        fix.mux.source,
        fix,
        &white.player_ref,
        &black.player_ref,
        GameStateArgs {
            board: &board1,
            turn: 1,
            status: 0,
            castle_rights: full_castle_rights(),
            en_passant_idx: square_idx(4, 2),
            pending_src_idx: -1,
            pending_dst_idx: -1,
            pending_promo: 0,
            recent_castle: 0,
            draw_state: 3,
        },
    );
    let sigscript = entry_sigscript(&pawn0, "apply", vec![Expr::bytes(fix.mux.prefix.clone()), Expr::bytes(fix.mux.suffix.clone())]);
    let entries = vec![covenant_utxo(&pawn0, covenant_id)];
    let outputs = vec![covenant_output(&mux1, 0, covenant_id)];
    let tx = Transaction::new(1, vec![tx_input(0, nonce, sigscript)], outputs, 0, Default::default(), 0, vec![]);
    (budgeted_chess_tx("pawn_apply", &tx, &entries), entries)
}

fn build_league_register_player_tx(nonce: u32) -> (Transaction, Vec<UtxoEntry>) {
    let owner = player_from_seed(7);
    let fix = fixture();
    let route_templates = packed_route_templates(fix);
    let routes_commitment = routes_commitment(&route_templates);
    let league_template = repeated_hash(0x11);
    let admin = repeated_hash(0x33);
    let base_rating = 1200i64;
    let covenant_id = repeated_hash((0x90u32.wrapping_add(nonce)) as u8);
    let player_id_domain = b"LeaguePlayerId".to_vec();

    let player_template_ctor = vec![
        hash_expr(league_template),
        hash_expr(repeated_hash(0x44)),
        hash_expr(fix.mux.hash),
        hash_expr(routes_commitment),
        hash_expr(repeated_hash(0x55)),
        hash_expr(repeated_hash(0x77)),
        Expr::int(0),
        Expr::int(900),
        Expr::int(1),
        Expr::int(2),
        Expr::int(3),
        Expr::int(4),
    ];
    let player_template_contract = compile_cached(player_source(), &player_template_ctor);
    let layout = player_template_contract.state_layout;
    let player_prefix = player_template_contract.script[..layout.start].to_vec();
    let player_suffix = player_template_contract.script[layout.start + layout.len..].to_vec();
    let player_template = blake2b_bytes(&[player_prefix.as_slice(), player_suffix.as_slice()].concat());

    let league_ctor = vec![
        hash_expr(league_template),
        hash_expr(player_template),
        hash_expr(fix.mux.hash),
        hash_expr(routes_commitment),
        Expr::int(base_rating),
        hash_expr(admin),
    ];
    let league = compile_cached(league_source(), &league_ctor);

    let league_input = TransactionInput {
        previous_outpoint: input_outpoint(7, nonce),
        signature_script: vec![],
        sequence: 0,
        mass: ComputeBudget(0).into(),
    };

    let player_id = blake2b_bytes(
        &[
            player_id_domain.as_slice(),
            &league_input.previous_outpoint.transaction_id.as_bytes(),
            &league_input.previous_outpoint.index.to_le_bytes(),
        ]
        .concat(),
    );
    let registered_player = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &owner.owner_hash,
            player_id: &player_id,
            open_games: 0,
            rating: base_rating,
            games: 0,
            wins: 0,
            draws: 0,
            losses: 0,
        },
    );

    let outputs = vec![covenant_output(&league, 0, covenant_id), covenant_output(&registered_player, 0, covenant_id)];
    let entries = vec![covenant_utxo(&league, covenant_id)];
    let mut tx = Transaction::new(1, vec![league_input], outputs, 0, Default::default(), 0, vec![]);
    tx.inputs[0].signature_script = entry_sigscript(
        &league,
        "register_player",
        vec![
            Expr::bytes(vec![0u8; 65]),
            Expr::bytes(owner.pubkey_bytes.clone()),
            Expr::bytes(player_prefix.clone()),
            Expr::bytes(player_suffix.clone()),
        ],
    );

    let sig = sign_tx_input_schnorr(&tx, &entries, 0, &owner);
    tx.inputs[0].signature_script = entry_sigscript(
        &league,
        "register_player",
        vec![Expr::bytes(sig), Expr::bytes(owner.pubkey_bytes), Expr::bytes(player_prefix), Expr::bytes(player_suffix)],
    );
    (budgeted_chess_tx("league_register_player", &tx, &entries), entries)
}

fn build_player_start_game_tx(nonce: u32) -> (Transaction, Vec<UtxoEntry>) {
    let fix = fixture();
    let route_templates = packed_route_templates(fix);
    let routes_commitment = routes_commitment(&route_templates);
    let white = player_from_seed(0x31);
    let black = player_from_seed(0x32);
    let league_template = repeated_hash(0x19);
    let covenant_id = repeated_hash((0xa0u32.wrapping_add(nonce)) as u8);
    let base_rating = 1200i64;

    let player_contract = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &repeated_hash(0x44),
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &repeated_hash(0x55),
            player_id: &repeated_hash(0x56),
            open_games: 0,
            rating: base_rating,
            games: 0,
            wins: 0,
            draws: 0,
            losses: 0,
        },
    );
    let player_layout = player_contract.state_layout;
    let player_template = blake2b_bytes(
        &[
            player_contract.script[..player_layout.start].as_ref(),
            player_contract.script[player_layout.start + player_layout.len..].as_ref(),
        ]
        .concat(),
    );
    let player_prefix_len = player_layout.start as i64;
    let player_suffix_len = (player_contract.script.len() - (player_layout.start + player_layout.len)) as i64;

    let white_player = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &white.owner_hash,
            player_id: &white.player_id,
            open_games: 0,
            rating: base_rating,
            games: 0,
            wins: 0,
            draws: 0,
            losses: 0,
        },
    );
    let black_player = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &black.owner_hash,
            player_id: &black.player_id,
            open_games: 0,
            rating: base_rating,
            games: 0,
            wins: 0,
            draws: 0,
            losses: 0,
        },
    );
    let next_white_player = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &white.owner_hash,
            player_id: &white.player_id,
            open_games: 1,
            rating: base_rating,
            games: 0,
            wins: 0,
            draws: 0,
            losses: 0,
        },
    );
    let next_black_player = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &black.owner_hash,
            player_id: &black.player_id,
            open_games: 1,
            rating: base_rating,
            games: 0,
            wins: 0,
            draws: 0,
            losses: 0,
        },
    );
    let opening_mux = compile_state(
        fix.mux.source,
        fix,
        &white.player_ref,
        &black.player_ref,
        GameStateArgs {
            board: &standard_board(),
            turn: 0,
            status: 0,
            castle_rights: full_castle_rights(),
            en_passant_idx: -1,
            pending_src_idx: -1,
            pending_dst_idx: -1,
            pending_promo: 0,
            recent_castle: 0,
            draw_state: 3,
        },
    );

    let white_placeholder = entry_sigscript(
        &white_player,
        "start_game",
        vec![
            Expr::bytes(vec![0u8; 65]),
            Expr::bytes(white.pubkey_bytes.clone()),
            Expr::int(0),
            Expr::int(player_prefix_len),
            Expr::int(player_suffix_len),
            Expr::bytes(route_templates.clone()),
            Expr::int(600),
            Expr::bytes(fix.mux.prefix.clone()),
            Expr::bytes(fix.mux.suffix.clone()),
        ],
    );
    let black_placeholder = entry_sigscript(
        &black_player,
        "delegate_start_game",
        vec![
            Expr::bytes(vec![0u8; 65]),
            Expr::bytes(black.pubkey_bytes.clone()),
            Expr::int(600),
            Expr::int(player_prefix_len),
            Expr::int(player_suffix_len),
        ],
    );

    let outputs = vec![
        covenant_output(&next_white_player, 0, covenant_id),
        covenant_output(&next_black_player, 0, covenant_id),
        covenant_output(&opening_mux, 0, covenant_id),
    ];
    let entries = vec![covenant_utxo(&white_player, covenant_id), covenant_utxo(&black_player, covenant_id)];
    let mut tx = Transaction::new(
        1,
        vec![tx_input(0, nonce * 2, white_placeholder), tx_input(1, nonce * 2 + 1, black_placeholder)],
        outputs,
        0,
        Default::default(),
        0,
        vec![],
    );
    let white_sig = sign_tx_input_schnorr(&tx, &entries, 0, &white);
    let black_sig = sign_tx_input_schnorr(&tx, &entries, 1, &black);
    tx.inputs[0].signature_script = entry_sigscript(
        &white_player,
        "start_game",
        vec![
            Expr::bytes(white_sig),
            Expr::bytes(white.pubkey_bytes),
            Expr::int(0),
            Expr::int(player_prefix_len),
            Expr::int(player_suffix_len),
            Expr::bytes(route_templates.clone()),
            Expr::int(600),
            Expr::bytes(fix.mux.prefix.clone()),
            Expr::bytes(fix.mux.suffix.clone()),
        ],
    );
    tx.inputs[1].signature_script = entry_sigscript(
        &black_player,
        "delegate_start_game",
        vec![
            Expr::bytes(black_sig),
            Expr::bytes(black.pubkey_bytes),
            Expr::int(600),
            Expr::int(player_prefix_len),
            Expr::int(player_suffix_len),
        ],
    );
    (budgeted_chess_tx("player_start_game", &tx, &entries), entries)
}

fn approx_expected_score(diff: i64) -> i64 {
    if diff < -800 {
        990
    } else if diff < -600 {
        970
    } else if diff < -400 {
        910
    } else if diff < -250 {
        820
    } else if diff < -150 {
        700
    } else if diff < -75 {
        600
    } else if diff < 75 {
        500
    } else if diff < 150 {
        400
    } else if diff < 250 {
        300
    } else if diff < 400 {
        180
    } else if diff < 600 {
        90
    } else if diff < 800 {
        30
    } else {
        10
    }
}

fn approx_updated_rating(self_rating: i64, opp_rating: i64, actual_score: i64) -> i64 {
    let expected = approx_expected_score(opp_rating - self_rating);
    let delta = (32 * (actual_score - expected)) / 1000;
    self_rating + delta
}

fn build_settle_tx(nonce: u32) -> (Transaction, Vec<UtxoEntry>) {
    let fix = fixture();
    let route_templates = packed_route_templates(fix);
    let routes_commitment = routes_commitment(&route_templates);
    let base_rating = 1200;
    let league_template = repeated_hash(0x33);
    let covenant_id = repeated_hash((0xb0u32.wrapping_add(nonce)) as u8);
    let white = player_from_seed(0x21);
    let black = player_from_seed(0x22);

    let player_contract = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &repeated_hash(0x44),
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &repeated_hash(0x55),
            player_id: &repeated_hash(0x56),
            open_games: 0,
            rating: base_rating,
            games: 0,
            wins: 0,
            draws: 0,
            losses: 0,
        },
    );
    let player_layout = player_contract.state_layout;
    let player_template = blake2b_bytes(
        &[
            player_contract.script[..player_layout.start].as_ref(),
            player_contract.script[player_layout.start + player_layout.len..].as_ref(),
        ]
        .concat(),
    );
    let white_player = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &white.owner_hash,
            player_id: &white.player_id,
            open_games: 1,
            rating: base_rating,
            games: 10,
            wins: 6,
            draws: 2,
            losses: 2,
        },
    );
    let black_player = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &black.owner_hash,
            player_id: &black.player_id,
            open_games: 1,
            rating: base_rating,
            games: 10,
            wins: 2,
            draws: 2,
            losses: 6,
        },
    );

    let white_rating = approx_updated_rating(base_rating, base_rating, 1000);
    let black_rating = approx_updated_rating(base_rating, base_rating, 0);
    let routed_settle = compile_settle_state(fix.settle.source, &player_template, &white.player_ref, &black.player_ref, 1);
    let settled_white = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &white.owner_hash,
            player_id: &white.player_id,
            open_games: 0,
            rating: white_rating,
            games: 11,
            wins: 7,
            draws: 2,
            losses: 2,
        },
    );
    let settled_black = compile_player_state(
        player_source(),
        PlayerStateArgs {
            league_template: &league_template,
            player_template: &player_template,
            mux_template: &fix.mux.hash,
            routes_commitment: &routes_commitment,
            owner_hash: &black.owner_hash,
            player_id: &black.player_id,
            open_games: 0,
            rating: black_rating,
            games: 11,
            wins: 2,
            draws: 2,
            losses: 7,
        },
    );

    let settle_prefix = player_contract.script[..player_layout.start].to_vec();
    let settle_suffix = player_contract.script[player_layout.start + player_layout.len..].to_vec();
    let settle_sigscript = entry_sigscript(&routed_settle, "settle", vec![Expr::bytes(settle_prefix), Expr::bytes(settle_suffix)]);
    let settle_prefix_len = fix.settle.prefix.len() as i64;
    let settle_suffix_len = fix.settle.suffix.len() as i64;
    let white_delegate_sigscript = entry_sigscript(
        &white_player,
        "delegate_settle",
        vec![
            Expr::int(settle_prefix_len),
            Expr::int(settle_suffix_len),
            hash_expr(fix.settle.hash),
            Expr::bytes(route_templates.clone()),
        ],
    );
    let black_delegate_sigscript = entry_sigscript(
        &black_player,
        "delegate_settle",
        vec![Expr::int(settle_prefix_len), Expr::int(settle_suffix_len), hash_expr(fix.settle.hash), Expr::bytes(route_templates)],
    );

    let outputs = vec![
        covenant_output_with_value(&settled_white, 0, covenant_id, 2_000),
        covenant_output_with_value(&settled_black, 0, covenant_id, 1_000),
    ];
    let entries = vec![
        covenant_utxo(&routed_settle, covenant_id),
        covenant_utxo(&white_player, covenant_id),
        covenant_utxo(&black_player, covenant_id),
    ];
    let tx = Transaction::new(
        1,
        vec![
            tx_input(0, nonce * 3, settle_sigscript),
            tx_input(1, nonce * 3 + 1, white_delegate_sigscript),
            tx_input(2, nonce * 3 + 2, black_delegate_sigscript),
        ],
        outputs,
        0,
        Default::default(),
        0,
        vec![],
    );
    (budgeted_chess_tx("settle", &tx, &entries), entries)
}

fn schnorr_keypair(seed: u32) -> Keypair {
    let secp = Secp256k1::new();
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&seed.to_le_bytes());
    bytes[4] = 1;
    let secret = SecretKey::from_slice(&bytes).expect("valid schnorr secret");
    Keypair::from_secret_key(&secp, &secret)
}

fn build_schnorr_2in1_tx(nonce: u32) -> (Transaction, Vec<UtxoEntry>) {
    let kp1 = schnorr_keypair(nonce.wrapping_mul(2).wrapping_add(1));
    let kp2 = schnorr_keypair(nonce.wrapping_mul(2).wrapping_add(2));
    let out_kp = schnorr_keypair(nonce.wrapping_mul(2).wrapping_add(3));
    let dummy_out1 = input_outpoint(0, nonce.wrapping_mul(2));
    let dummy_out2 = input_outpoint(1, nonce.wrapping_mul(2).wrapping_add(1));

    let addr1 = Address::new(Prefix::Mainnet, Version::PubKey, &kp1.x_only_public_key().0.serialize());
    let addr2 = Address::new(Prefix::Mainnet, Version::PubKey, &kp2.x_only_public_key().0.serialize());
    let out_addr = Address::new(Prefix::Mainnet, Version::PubKey, &out_kp.x_only_public_key().0.serialize());

    let utxos = vec![
        UtxoEntry::new(20_000, pay_to_address_script(&addr1), 0, false, None),
        UtxoEntry::new(20_000, pay_to_address_script(&addr2), 0, false, None),
    ];
    let outputs = vec![TransactionOutput { value: 30_000, script_public_key: pay_to_address_script(&out_addr), covenant: None }];
    let mut tx = Transaction::new(
        0,
        vec![
            TransactionInput {
                previous_outpoint: dummy_out1,
                signature_script: vec![],
                sequence: 0,
                mass: TxInputMass::SigopCount(1.into()),
            },
            TransactionInput {
                previous_outpoint: dummy_out2,
                signature_script: vec![],
                sequence: 0,
                mass: TxInputMass::SigopCount(1.into()),
            },
        ],
        outputs,
        0,
        Default::default(),
        0,
        vec![],
    );

    for (input_idx, kp) in [kp1, kp2].iter().enumerate() {
        let populated = PopulatedTransaction::new(&tx, utxos.clone());
        let sig_hash = calc_schnorr_signature_hash(&populated, input_idx, SIG_HASH_ALL, &SigHashReusedValuesUnsync::new());
        let msg = Message::from_digest_slice(sig_hash.as_bytes().as_slice()).expect("valid sighash");
        let sig = kp.sign_schnorr(msg);
        tx.inputs[input_idx].signature_script = std::iter::once(65u8).chain(*sig.as_ref()).chain([SIG_HASH_ALL.to_u8()]).collect();
    }

    (tx, utxos)
}

fn build_op_dup_script_public_key() -> ScriptPublicKey {
    let mut builder = ScriptBuilder::new();
    builder.add_i64(1).expect("push integer 1");
    for _ in 0..243 {
        builder.add_op(OpDup).expect("append OP_DUP");
    }
    ScriptPublicKey::new(0, builder.drain().into())
}

fn try_build_budgeted_single_input_tx(
    nonce: u32,
    input_spk: ScriptPublicKey,
    signature_script: Vec<u8>,
) -> Result<(Transaction, Vec<UtxoEntry>), String> {
    let outpoint = input_outpoint(0, nonce);

    let utxos = vec![UtxoEntry::new(20_000, input_spk, 0, false, None)];
    let mut tx = Transaction::new(
        1,
        vec![TransactionInput {
            previous_outpoint: outpoint,
            signature_script,
            sequence: 0,
            mass: TxInputMass::ComputeBudget(0.into()),
        }],
        vec![],
        0,
        Default::default(),
        0,
        vec![],
    );

    let reused_values = SigHashReusedValuesUnsync::new();
    let sig_cache = Cache::new(1);
    let populated = PopulatedTransaction::new(&tx, utxos.clone());
    let mut vm = TxScriptEngine::from_transaction_input_with_allowed_script_units(
        &populated,
        &tx.inputs[0],
        0,
        &utxos[0],
        EngineCtx::new(&sig_cache).with_reused(&reused_values),
        bench_flags(true),
        u64::MAX,
    );
    vm.execute().map_err(|err| format!("failed to measure op_dup input #0: {err}"))?;
    let compute_budget = ComputeBudget::checked_covering_script_units(ScriptUnits(vm.used_script_units()))
        .ok_or_else(|| "required compute budget does not fit for op_dup input #0".to_string())?;
    tx.inputs[0].mass = compute_budget.into();

    Ok((tx, utxos))
}

fn build_budgeted_single_input_tx(nonce: u32, input_spk: ScriptPublicKey, signature_script: Vec<u8>) -> (Transaction, Vec<UtxoEntry>) {
    try_build_budgeted_single_input_tx(nonce, input_spk, signature_script).unwrap_or_else(|err| panic!("{err}"))
}

fn build_op_dup_tx(nonce: u32) -> (Transaction, Vec<UtxoEntry>) {
    build_budgeted_single_input_tx(nonce, build_op_dup_script_public_key(), vec![])
}

fn build_op_dup_p2sh_tx(nonce: u32) -> (Transaction, Vec<UtxoEntry>) {
    let redeem_script = build_op_dup_script_public_key();
    let signature_script =
        pay_to_script_hash_signature_script(redeem_script.script().to_vec(), vec![]).expect("build p2sh redeeming sigscript");
    build_budgeted_single_input_tx(nonce, pay_to_script_hash_script(redeem_script.script()), signature_script)
}

fn build_op_dup_one_tx_script_public_key(nonce: u32) -> ScriptPublicKey {
    let mut script = ScriptBuilder::new();
    script.add_i64(1).expect("push integer 1");
    for _ in 0..243 {
        script.add_op(OpDup).expect("append OP_DUP");
    }

    let mut best_script = ScriptPublicKey::new(0, script.drain().into());
    let mut pair_count = 0usize;
    loop {
        // eprintln!("building candidate op_dup_one_tx script with {} OP_DUP/OP_DROP pairs", pair_count + 1);
        let mut candidate_builder = ScriptBuilder::new();
        candidate_builder.add_i64(1).expect("push integer 1");
        for _ in 0..243 {
            candidate_builder.add_op(OpDup).expect("append OP_DUP");
        }
        for _ in 0..pair_count + 1 {
            candidate_builder.add_op(OpDrop).expect("append OP_DROP");
            candidate_builder.add_op(OpDup).expect("append OP_DUP");
        }
        let candidate_script = ScriptPublicKey::new(0, candidate_builder.drain().into());
        let Ok((candidate_tx, _)) = try_build_budgeted_single_input_tx(nonce, candidate_script.clone(), vec![]) else {
            break;
        };
        if compute_mass(&candidate_tx) > BLOCK_COMPUTE_MASS_LIMIT {
            break;
        }
        best_script = candidate_script;
        pair_count += 20;
    }

    best_script
}

fn build_chess_mix_block() -> BenchBlock {
    type Builder = fn(u32) -> (Transaction, Vec<UtxoEntry>);
    let builders: [Builder; 5] =
        [build_pawn_apply_tx, build_route_tx, build_league_register_player_tx, build_player_start_game_tx, build_settle_tx];

    let mut txs = Vec::new();
    let mut total_mass = 0u64;
    let mut total_inputs = 0usize;
    let mut nonce = 0u32;
    let mut misses = 0usize;
    while misses < builders.len() {
        let builder = builders[(nonce as usize) % builders.len()];
        let (tx, entries) = builder(nonce);
        let tx_mass = compute_mass(&tx);
        if total_mass + tx_mass <= BLOCK_COMPUTE_MASS_LIMIT {
            total_inputs += tx.inputs.len();
            total_mass += tx_mass;
            txs.push(prepare_bench_tx(tx, entries, true));
            misses = 0;
        } else {
            misses += 1;
        }
        nonce = nonce.wrapping_add(1);
    }

    BenchBlock { name: "chess_mix", tx_count: txs.len(), input_count: total_inputs, compute_mass: total_mass, txs }
}

fn build_schnorr_block() -> BenchBlock {
    let mut txs = Vec::new();
    let mut total_mass = 0u64;
    let mut total_inputs = 0usize;
    let mut nonce = 0u32;
    loop {
        let (tx, entries) = build_schnorr_2in1_tx(nonce);
        let tx_mass = compute_mass(&tx);
        if total_mass + tx_mass > BLOCK_COMPUTE_MASS_LIMIT {
            break;
        }
        total_inputs += tx.inputs.len();
        total_mass += tx_mass;
        txs.push(prepare_bench_tx(tx, entries, true));
        nonce = nonce.wrapping_add(1);
    }
    BenchBlock { name: "schnorr_2in1", tx_count: txs.len(), input_count: total_inputs, compute_mass: total_mass, txs }
}

fn build_op_dup_block() -> BenchBlock {
    let mut txs = Vec::new();
    let mut total_mass = 0u64;
    let mut total_inputs = 0usize;
    let mut nonce = 0u32;
    loop {
        let (tx, entries) = build_op_dup_tx(nonce);
        let tx_mass = compute_mass(&tx);
        if total_mass + tx_mass > BLOCK_COMPUTE_MASS_LIMIT {
            break;
        }
        total_inputs += tx.inputs.len();
        total_mass += tx_mass;
        txs.push(prepare_bench_tx(tx, entries, true));
        nonce = nonce.wrapping_add(1);
    }
    BenchBlock { name: "op_dup_243", tx_count: txs.len(), input_count: total_inputs, compute_mass: total_mass, txs }
}

fn build_op_dup_one_tx_block() -> BenchBlock {
    let nonce = 0u32;
    let output_spk = build_op_dup_one_tx_script_public_key(nonce);
    let (tx, entries) = build_budgeted_single_input_tx(nonce, output_spk, vec![]);
    let tx_mass = compute_mass(&tx);
    assert!(tx_mass <= BLOCK_COMPUTE_MASS_LIMIT, "op_dup_one_tx mass {tx_mass} exceeds block limit {BLOCK_COMPUTE_MASS_LIMIT}");
    BenchBlock {
        name: "op_dup_one_tx",
        tx_count: 1,
        input_count: tx.inputs.len(),
        compute_mass: tx_mass,
        txs: vec![prepare_bench_tx(tx, entries, true)],
    }
}

fn build_op_dup_p2sh_block() -> BenchBlock {
    let mut txs = Vec::new();
    let mut total_mass = 0u64;
    let mut total_inputs = 0usize;
    let mut nonce = 0u32;
    loop {
        let (tx, entries) = build_op_dup_p2sh_tx(nonce);
        let tx_mass = compute_mass(&tx);
        if total_mass + tx_mass > BLOCK_COMPUTE_MASS_LIMIT {
            break;
        }
        total_inputs += tx.inputs.len();
        total_mass += tx_mass;
        txs.push(prepare_bench_tx(tx, entries, true));
        nonce = nonce.wrapping_add(1);
    }
    BenchBlock { name: "op_dup_243_p2sh", tx_count: txs.len(), input_count: total_inputs, compute_mass: total_mass, txs }
}

fn bench_blocks() -> &'static Vec<BenchBlock> {
    static BLOCKS: OnceLock<Vec<BenchBlock>> = OnceLock::new();
    BLOCKS.get_or_init(|| {
        let blocks = vec![
            build_chess_mix_block(),
            build_schnorr_block(),
            // build_op_dup_block(),
            build_op_dup_p2sh_block(),
            // build_op_dup_one_tx_block(),
        ];
        for block in &blocks {
            eprintln!(
                "bench block {}: {} txs, {} inputs, compute mass {}",
                block.name, block.tx_count, block.input_count, block.compute_mass
            );
        }
        blocks
    })
}

fn input_allowed_script_units(input: &TransactionInput, flags: EngineFlags) -> u64 {
    match input.mass {
        TxInputMass::SigopCount(count) => {
            (u8::from(count) as u64).saturating_mul(flags.sigop_script_units).saturating_add(free_script_units_per_input().value())
        }
        TxInputMass::ComputeBudget(compute_budget) => compute_budget.allowed_script_units().value(),
    }
}

fn validate_block_sequential(block: &BenchBlock) {
    let cache = Cache::new(block.input_count as u64);
    for bench_tx in &block.txs {
        let verifiable = bench_tx.tx.as_verifiable();
        let reused_values = SigHashReusedValuesUnsync::new();
        let ctx = EngineCtx::new(&cache).with_reused(&reused_values).with_covenants_ctx(&bench_tx.cov_ctx);
        let flags = bench_flags(bench_tx.covenants_enabled);
        for (input_idx, (input, utxo)) in verifiable.populated_inputs().enumerate() {
            let allowed_script_units = input_allowed_script_units(input, flags);
            let mut vm = TxScriptEngine::from_transaction_input_with_allowed_script_units(
                &verifiable,
                input,
                input_idx,
                utxo,
                ctx,
                flags,
                allowed_script_units,
            );
            vm.execute().unwrap();
        }
    }
}

fn validate_block_parallel(block: &BenchBlock, pool: &rayon::ThreadPool) {
    let cache = Cache::new(block.input_count as u64);
    for bench_tx in &block.txs {
        let verifiable = bench_tx.tx.as_verifiable();
        let reused_values = SigHashReusedValuesSync::new();
        let ctx = EngineCtx::new(&cache).with_reused(&reused_values).with_covenants_ctx(&bench_tx.cov_ctx);
        let flags = bench_flags(bench_tx.covenants_enabled);
        pool.install(|| {
            (0..verifiable.inputs().len()).into_par_iter().try_for_each(|input_idx| {
                let (input, utxo) = verifiable.populated_input(input_idx);
                let allowed_script_units = input_allowed_script_units(input, flags);
                let mut vm = TxScriptEngine::from_transaction_input_with_allowed_script_units(
                    &verifiable,
                    input,
                    input_idx,
                    utxo,
                    ctx,
                    flags,
                    allowed_script_units,
                );
                vm.execute()
            })
        })
        .unwrap();
    }
}

fn benchmark_script_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_script_validation");
    group.sampling_mode(SamplingMode::Flat);
    group.measurement_time(Duration::from_secs(15));

    for block in bench_blocks() {
        group.bench_with_input(BenchmarkId::new("single_thread", block.name), block, |b, block| {
            b.iter(|| validate_block_sequential(black_box(block)));
        });

        for threads in [2usize, 4, 8, 16] {
            let pool = ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            group.bench_with_input(BenchmarkId::new(format!("rayon_threads_{threads}"), block.name), block, |b, block| {
                b.iter(|| validate_block_parallel(black_box(block), black_box(&pool)));
            });
        }
    }
}

criterion_group!(
    name = benches;
    config = Criterion::default().with_output_color(true);
    targets = benchmark_script_validation
);
criterion_main!(benches);
