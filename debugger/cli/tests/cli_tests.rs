use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use kaspa_consensus_core::Hash;
use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::hashing::sighash::calc_schnorr_signature_hash;
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, Transaction, TransactionId, TransactionInput, TransactionOutpoint, TransactionOutput,
    TxInputMass, UtxoEntry,
};
use kaspa_txscript::{pay_to_script_hash_script, pay_to_script_hash_signature_script};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use silverscript_lang::ast::{Expr, format_contract_ast};
use silverscript_lang::compiler::{CompileOptions, CovenantDeclCallOptions, compile_contract, struct_object};

const COV_A: Hash = Hash::from_bytes(*b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("write to string");
    }
    out
}

fn compile_dog20_state<'a>(source: &'a str, owner: Vec<u8>, amount: i64) -> silverscript_lang::compiler::CompiledContract<'a> {
    compile_contract(
        source,
        &[Expr::bytes(owner), Expr::int(amount), Expr::byte(0), Expr::bool(false), Expr::int(2), Expr::int(2)],
        CompileOptions::default(),
    )
    .expect("compile dog20 state")
}

fn sign_tx_input(tx: Transaction, entries: Vec<UtxoEntry>, input_idx: usize, keypair: &Keypair) -> Vec<u8> {
    let tx = MutableTransaction::with_entries(tx, entries);
    let reused_values = SigHashReusedValuesUnsync::new();
    let sig_hash = calc_schnorr_signature_hash(&tx.as_verifiable(), input_idx, SIG_HASH_ALL, &reused_values);
    let msg = secp256k1::Message::from_digest_slice(sig_hash.as_bytes().as_slice()).expect("valid sighash message");
    let sig = keypair.sign_schnorr(msg);
    let mut signature = sig.as_ref().to_vec();
    signature.push(SIG_HASH_ALL.to_u8());
    signature
}

fn build_dog20_handoff_fixture_content() -> (String, String) {
    let source =
        std::fs::read_to_string("/home/ori/silverscript/silverscript-lang/tests/examples/dog20.sil").expect("read dog20 source");

    let secp = Secp256k1::new();
    let genesis_secret = SecretKey::from_slice(&[1u8; 32]).expect("valid genesis secret key");
    let handoff_secret = SecretKey::from_slice(&[2u8; 32]).expect("valid handoff secret key");
    let genesis_owner = Keypair::from_secret_key(&secp, &genesis_secret);
    let handoff_owner = Keypair::from_secret_key(&secp, &handoff_secret);

    let genesis_owner_bytes = genesis_owner.x_only_public_key().0.serialize().to_vec();
    let handoff_owner_bytes = handoff_owner.x_only_public_key().0.serialize().to_vec();

    let genesis = compile_dog20_state(&source, genesis_owner_bytes.clone(), 1_000);
    let handoff = compile_dog20_state(&source, handoff_owner_bytes.clone(), 1_000);
    let lowered_source = format_contract_ast(&genesis.ast);

    let handoff_outputs = vec![TransactionOutput {
        value: 1_000,
        script_public_key: pay_to_script_hash_script(&handoff.script),
        covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: COV_A }),
    }];

    let handoff_entries = vec![UtxoEntry::new(1_000, pay_to_script_hash_script(&genesis.script), 0, false, Some(COV_A))];
    let handoff_unsigned_tx = Transaction::new(
        1,
        vec![TransactionInput {
            previous_outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes([1u8; 32]), index: 0 },
            signature_script: vec![],
            sequence: 0,
                    mass: TxInputMass::ComputeMass(0),
        }],
        handoff_outputs.clone(),
        0,
        Default::default(),
        0,
        vec![],
    );

    let handoff_sig = sign_tx_input(handoff_unsigned_tx, handoff_entries, 0, &genesis_owner);
    let handoff_action_script = genesis
        .build_sig_script_for_covenant_decl(
            "transfer",
            vec![
                vec![struct_object(vec![
                    ("ownerIdentifier", Expr::bytes(handoff_owner_bytes.clone())),
                    ("identifierType", Expr::byte(0)),
                    ("amount", Expr::int(1_000)),
                    ("isMinter", Expr::bool(false)),
                ])]
                .into(),
                vec![Expr::bytes(handoff_sig.clone())].into(),
                Expr::bytes(vec![]),
            ],
            CovenantDeclCallOptions { is_leader: true },
        )
        .expect("build handoff leader action script");
    let handoff_sigscript =
        pay_to_script_hash_signature_script(genesis.script.clone(), handoff_action_script).expect("build p2sh handoff sigscript");

    let handoff_sigscript_hex = hex_encode(&handoff_sigscript);
    let cov_a_hex = hex_encode(&COV_A.as_bytes());
    let prev_txid_hex = hex_encode(&[1u8; 32]);

    let test_file = format!(
        r#"{{
    "tests": [
        {{
            "name": "dog20_handoff_until_line_803",
            "function": "__leader_transfer",
            "constructor_args": [
                "0x{genesis_owner_hex}",
                1000,
                0,
                false,
                2,
                2
            ],
            "args": [
                [
                    {{
                        "ownerIdentifier": "0x{handoff_owner_hex}",
                        "identifierType": 0,
                        "amount": 1000,
                        "isMinter": false
                    }}
                ],
                [
                    "0x{handoff_sig_hex}"
                ],
                "0x"
            ],
            "expect": "pass",
            "tx": {{
                "version": 1,
                "lock_time": 0,
                "active_input_index": 0,
                "inputs": [
                    {{
                        "prev_txid": "0x{prev_txid_hex}",
                        "prev_index": 0,
                        "sequence": 0,
                        "sig_op_count": 100,
                        "utxo_value": 1000,
                        "covenant_id": "0x{cov_a_hex}",
                        "signature_script_hex": "0x{handoff_sigscript_hex}"
                    }}
                ],
                "outputs": [
                    {{
                        "value": 1000,
                        "covenant_id": "0x{cov_a_hex}",
                        "authorizing_input": 0,
                        "constructor_args": [
                            "0x{handoff_owner_hex}",
                            1000,
                            0,
                              false,
                              2,
                              2
                        ]
                    }}
                ]
            }}
        }}
    ]
}}
"#,
        genesis_owner_hex = hex_encode(&genesis_owner_bytes),
        handoff_owner_hex = hex_encode(&handoff_owner_bytes),
        handoff_sig_hex = hex_encode(&handoff_sig),
        handoff_sigscript_hex = handoff_sigscript_hex,
        cov_a_hex = cov_a_hex,
        prev_txid_hex = prev_txid_hex,
    );

    (lowered_source, test_file)
}

fn write_dog20_handoff_fixture_to(dir: &Path) -> (PathBuf, PathBuf) {
    std::fs::create_dir_all(dir).expect("create dog20 fixture dir");
    let script_path = dir.join("dog20.sil");
    let test_file_path = dir.join("dog20.test.json");
    let (source, test_file) = build_dog20_handoff_fixture_content();
    std::fs::write(&script_path, source).expect("write dog20 fixture script");
    std::fs::write(&test_file_path, test_file).expect("write dog20 test file");
    (script_path, test_file_path)
}

fn write_test_fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    write_named_test_fixture("simple.sil", "simple.test.json")
}

fn write_fixture_files(
    script_name: &str,
    test_file_name: &str,
    script_source: &str,
    test_file_source: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
    let dir = std::env::temp_dir().join(format!("cli_debugger_test_fixture_{}_{}", std::process::id(), nonce));
    std::fs::create_dir_all(&dir).expect("create temp fixture dir");

    let script_path = dir.join(script_name);
    let test_file_path = dir.join(test_file_name);

    std::fs::write(&script_path, script_source).expect("write fixture contract");
    std::fs::write(&test_file_path, test_file_source).expect("write fixture test file");

    (script_path, test_file_path)
}

fn write_logging_test_fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    write_fixture_files(
        "logging.sil",
        "logging.test.json",
        r#"pragma silverscript ^0.1.0;

contract Logging(int seed) {
    entrypoint function check(int a) {
        console.log("seed", seed);
        console.log("sum", seed + a);
        require(seed + a > 0);
    }
}
"#,
        r#"{
  "tests": [
    {
      "name": "log_case",
      "function": "check",
      "constructor_args": [5],
      "args": [4],
      "expect": "pass"
    }
  ]
}
"#,
    )
}

fn shared_example_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name)
}

fn write_named_test_fixture(script_name: &str, test_file_name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    write_fixture_files(
        script_name,
        test_file_name,
        r#"pragma silverscript ^0.1.0;

contract Simple(int x) {
    entrypoint function check(int a) {
        require(a == x);
    }
}
"#,
        r#"{
  "tests": [
    {
      "name": "pass_case",
      "function": "check",
      "constructor_args": [5],
      "args": [5],
      "expect": "pass"
    },
    {
      "name": "fail_case",
      "function": "check",
      "constructor_args": [5],
      "args": [4],
      "expect": "fail"
    }
  ]
}
"#,
    )
}

fn write_structured_args_fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    write_fixture_files(
        "structured_args.sil",
        "structured_args.test.json",
        r#"pragma silverscript ^0.1.0;

contract StructuredArgs() {
    int amount = 1;
    byte[32] owner = 0x1111111111111111111111111111111111111111111111111111111111111111;

    entrypoint function inspect(State next) {
        int bumped = next.amount + 1;
        require(bumped > amount);
    }

    entrypoint function inspect_many(State[] next_states) {
        require(next_states.length == 2);
    }
}
"#,
        r#"{
  "tests": [
    {
      "name": "object_arg_pass",
      "function": "inspect",
      "args": [
        {
          "amount": 7,
          "owner": "0x2222222222222222222222222222222222222222222222222222222222222222"
        }
      ],
      "expect": "pass"
    },
    {
      "name": "object_array_arg_pass",
      "function": "inspect_many",
      "args": [
        [
          {
            "amount": 7,
            "owner": "0x2222222222222222222222222222222222222222222222222222222222222222"
          },
          {
            "amount": 9,
            "owner": "0x3333333333333333333333333333333333333333333333333333333333333333"
          }
        ]
      ],
      "expect": "pass"
    }
  ]
}
"#,
    )
}

fn write_structured_ctor_fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    write_fixture_files(
        "structured_ctor.sil",
        "structured_ctor.test.json",
        r#"pragma silverscript ^0.1.0;

contract StructuredCtor(Pair seed) {
    struct Pair {
        int amount;
        byte[2] code;
    }

    entrypoint function inspect() {
        require(true);
    }
}
"#,
        r#"{
  "tests": [
    {
      "name": "struct_ctor_pass",
      "function": "inspect",
      "constructor_args": [
        {
          "amount": 7,
          "code": "0x1234"
        }
      ],
      "expect": "pass"
    }
  ]
}
"#,
    )
}

#[test]
fn cli_debugger_repl_all_commands_smoke() {
    let tmp = std::env::temp_dir().join("cli_test_if_statement.sil");
    std::fs::write(
        &tmp,
        r#"pragma silverscript ^0.1.0;

contract IfStatement(int x, int y) {
    entrypoint function hello(int a, int b) {
        int d = a + b;
        d = d - a;
        if (d == x - 2) {
            int c = d + b;
            d = a + c;
            require(c > d);
        } else {
            require(d == a);
        }
        d = d + a;
        require(d == y);
    }
}
"#,
    )
    .expect("write temp contract");
    let contract_path = &tmp;

    let mut child = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(contract_path)
        .arg("--function")
        .arg("hello")
        .arg("--ctor-arg")
        .arg("3")
        .arg("--ctor-arg")
        .arg("10")
        .arg("--arg")
        .arg("5")
        .arg("--arg")
        .arg("5")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cli-debugger");

    let input = b"help\nl\nstack\nb 1\nb 7\nb\nn\nsi\nq\n";
    child.stdin.as_mut().expect("stdin available").write_all(input).expect("write stdin");

    let output = child.wait_with_output().expect("wait for cli-debugger");
    assert!(output.status.success(), "cli-debugger exited with status {:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("Stepping through"), "missing startup output");
    assert!(stdout.contains("(sdb)"), "missing prompt output");
    assert!(stdout.contains("Commands:"), "missing help output");
    assert!(stdout.contains("Stack:"), "missing stack output");
    let saw_line1_feedback = stdout.contains("no statement at line 1") || stdout.contains("Breakpoint set at line 1");
    assert!(saw_line1_feedback, "missing breakpoint feedback for line 1");
    assert!(stdout.contains("Breakpoint set at line 7"), "missing line-7 breakpoint success");
    let listing_contains_7 = stdout.lines().any(|line| line.contains("Breakpoints:") && line.contains('7'));
    assert!(listing_contains_7, "missing breakpoint listing containing line 7");
}

#[test]
fn cli_debugger_eval_command_reports_results_and_errors() {
    let (script_path, _test_file_path) = write_test_fixture();

    let mut child = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--function")
        .arg("check")
        .arg("--ctor-arg")
        .arg("5")
        .arg("--arg")
        .arg("5")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cli-debugger");

    let input = b"eval 1 + 2\ne a + 1\ne missing + 1\nq\n";
    child.stdin.as_mut().expect("stdin available").write_all(input).expect("write stdin");

    let output = child.wait_with_output().expect("wait for cli-debugger");
    assert!(output.status.success(), "cli-debugger exited with status {:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("1 + 2 = (int) 3"), "missing literal eval output: {stdout}");
    assert!(stdout.contains("a + 1 = (int) 6"), "missing scoped eval output: {stdout}");
    assert!(
        stdout.contains("ERROR: failed to compile debug expression: undefined identifier: missing"),
        "missing eval error output: {stdout}"
    );
}

#[test]
fn cli_debugger_interactive_prints_console_logs_automatically() {
    let (script_path, _test_file_path) = write_logging_test_fixture();

    let mut child = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--function")
        .arg("check")
        .arg("--ctor-arg")
        .arg("5")
        .arg("--arg")
        .arg("4")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cli-debugger");

    child.stdin.as_mut().expect("stdin available").write_all(b"q\n").expect("write stdin");

    let output = child.wait_with_output().expect("wait for cli-debugger");
    assert!(output.status.success(), "cli-debugger exited with status {:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("seed 5"), "missing first console log: {stdout}");
}

#[test]
fn cli_debugger_accepts_state_object_arg_and_renders_source_level_value() {
    let (script_path, _test_file_path) = write_structured_args_fixture();

    let mut child = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--function")
        .arg("inspect")
        .arg("--arg")
        .arg(r#"{"amount":7,"owner":"0x2222222222222222222222222222222222222222222222222222222222222222"}"#)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cli-debugger");

    let input = b"vars\np next\nq\n";
    child.stdin.as_mut().expect("stdin available").write_all(input).expect("write stdin");

    let output = child.wait_with_output().expect("wait for cli-debugger");
    assert!(output.status.success(), "cli-debugger exited with status {:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let rendered = "{amount: 7, owner: 0x2222222222222222222222222222222222222222222222222222222222222222}";

    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("Contract State:"), "missing Contract State section: {stdout}");
    assert!(stdout.contains("Call Arguments:"), "missing Call Arguments section: {stdout}");
    assert!(stdout.contains(&format!("next (State) = {rendered}")), "missing rendered State value: {stdout}");
}

#[test]
fn cli_debugger_accepts_state_object_array_arg_and_renders_source_level_value() {
    let (script_path, _test_file_path) = write_structured_args_fixture();

    let mut child = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--function")
        .arg("inspect_many")
        .arg("--arg")
        .arg(
            r#"[{"amount":7,"owner":"0x2222222222222222222222222222222222222222222222222222222222222222"},{"amount":9,"owner":"0x3333333333333333333333333333333333333333333333333333333333333333"}]"#,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cli-debugger");

    let input = b"vars\np next_states\nq\n";
    child.stdin.as_mut().expect("stdin available").write_all(input).expect("write stdin");

    let output = child.wait_with_output().expect("wait for cli-debugger");
    assert!(output.status.success(), "cli-debugger exited with status {:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let rendered = "[{amount: 7, owner: 0x2222222222222222222222222222222222222222222222222222222222222222}, {amount: 9, owner: 0x3333333333333333333333333333333333333333333333333333333333333333}]";

    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains(&format!("next_states (State[]) = {rendered}")), "missing rendered State[] value: {stdout}");
}

#[test]
fn cli_debugger_accepts_struct_constructor_arg_and_renders_source_level_value() {
    let (script_path, _test_file_path) = write_structured_ctor_fixture();

    let mut child = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--function")
        .arg("inspect")
        .arg("--ctor-arg")
        .arg(r#"{"amount":7,"code":"0x1234"}"#)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cli-debugger");

    let input = b"vars\np seed\nq\n";
    child.stdin.as_mut().expect("stdin available").write_all(input).expect("write stdin");

    let output = child.wait_with_output().expect("wait for cli-debugger");
    assert!(output.status.success(), "cli-debugger exited with status {:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("seed (Pair) = {amount: 7, code: 0x1234}"), "missing rendered constructor struct value: {stdout}");
}

#[test]
fn cli_debugger_accepts_state_arg_with_byte_one_field_and_renders_source_level_value() {
    let script_path = shared_example_path("debug_state.sil");

    let mut child = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--function")
        .arg("inspect_state")
        .arg("--ctor-arg")
        .arg("4")
        .arg("--arg")
        .arg(r#"{"amount":5,"active":true,"tag":"0xaa"}"#)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cli-debugger");

    let input = b"vars\np next_state\nq\n";
    child.stdin.as_mut().expect("stdin available").write_all(input).expect("write stdin");

    let output = child.wait_with_output().expect("wait for cli-debugger");
    assert!(output.status.success(), "cli-debugger exited with status {:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("next_state (State) = {amount: 5, active: true, tag: 0xaa}"), "missing rendered State value: {stdout}");
}

#[test]
fn cli_debugger_accepts_state_array_arg_with_byte_one_field_and_renders_source_level_value() {
    let script_path = shared_example_path("debug_state.sil");

    let mut child = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--function")
        .arg("inspect_state_array")
        .arg("--ctor-arg")
        .arg("4")
        .arg("--arg")
        .arg(r#"[{"amount":5,"active":true,"tag":"0xaa"},{"amount":7,"active":true,"tag":"0xaa"}]"#)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cli-debugger");

    let input = b"vars\np next_states\nq\n";
    child.stdin.as_mut().expect("stdin available").write_all(input).expect("write stdin");

    let output = child.wait_with_output().expect("wait for cli-debugger");
    assert!(output.status.success(), "cli-debugger exited with status {:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(
        stdout.contains("next_states (State[]) = [{amount: 5, active: true, tag: 0xaa}, {amount: 7, active: true, tag: 0xaa}]"),
        "missing rendered State[] value: {stdout}"
    );
}

#[test]
fn cli_debugger_run_test_file_pass_case() {
    let (_script_path, test_file_path) = write_test_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run")
        .arg("--test-file")
        .arg(&test_file_path)
        .arg("--test-name")
        .arg("pass_case")
        .output()
        .expect("run cli-debugger pass test");

    assert!(
        output.status.success(),
        "expected success, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS"), "expected PASS in stdout, got: {stdout}");
}

#[test]
fn cli_debugger_run_test_file_expected_fail_case() {
    let (_script_path, test_file_path) = write_test_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run")
        .arg("--test-file")
        .arg(&test_file_path)
        .arg("--test-name")
        .arg("fail_case")
        .output()
        .expect("run cli-debugger expected-fail test");

    assert!(
        output.status.success(),
        "expected success for expected-fail test, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS (expected failure)"), "expected expected-failure PASS marker in stdout, got: {stdout}");
}

#[test]
fn cli_debugger_run_all_uses_test_file_suite() {
    let (_script_path, test_file_path) = write_test_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run-all")
        .arg("--test-file")
        .arg(&test_file_path)
        .output()
        .expect("run cli-debugger --run-all");

    assert!(
        output.status.success(),
        "expected success for run-all, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RUN   pass_case"), "missing pass_case header: {stdout}");
    assert!(stdout.contains("RUN   fail_case"), "missing fail_case header: {stdout}");
    assert!(stdout.contains("PASS  pass_case"), "missing pass_case status: {stdout}");
    assert!(stdout.contains("PASS  fail_case"), "missing fail_case status: {stdout}");
    assert!(stdout.contains("2 tests: 2 passed, 0 failed"), "missing summary line: {stdout}");
}

#[test]
fn cli_debugger_run_all_supports_structured_args_from_test_file() {
    let (_script_path, test_file_path) = write_structured_args_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run-all")
        .arg("--test-file")
        .arg(&test_file_path)
        .output()
        .expect("run cli-debugger --run-all for structured args");

    assert!(
        output.status.success(),
        "expected success for structured run-all, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS  object_arg_pass"), "missing object_arg_pass line: {stdout}");
    assert!(stdout.contains("PASS  object_array_arg_pass"), "missing object_array_arg_pass line: {stdout}");
    assert!(stdout.contains("2 tests: 2 passed, 0 failed"), "missing summary line: {stdout}");
}

#[test]
fn cli_debugger_run_all_supports_structured_constructor_args_from_test_file() {
    let (_script_path, test_file_path) = write_structured_ctor_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run-all")
        .arg("--test-file")
        .arg(&test_file_path)
        .output()
        .expect("run cli-debugger --run-all for structured ctor args");

    assert!(
        output.status.success(),
        "expected success for structured ctor run-all, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS  struct_ctor_pass"), "missing struct_ctor_pass line: {stdout}");
    assert!(stdout.contains("1 tests: 1 passed, 0 failed"), "missing summary line: {stdout}");
}

#[test]
fn cli_debugger_run_all_infers_test_file_from_script_path() {
    let (script_path, _test_file_path) = write_test_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--run-all")
        .output()
        .expect("run cli-debugger --run-all with inferred sidecar");

    assert!(
        output.status.success(),
        "expected success for inferred run-all, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RUN   pass_case"), "missing pass_case header: {stdout}");
    assert!(stdout.contains("RUN   fail_case"), "missing fail_case header: {stdout}");
    assert!(stdout.contains("PASS  pass_case"), "missing pass_case status: {stdout}");
    assert!(stdout.contains("PASS  fail_case"), "missing fail_case status: {stdout}");
    assert!(stdout.contains("2 tests: 2 passed, 0 failed"), "missing summary line: {stdout}");
}

#[test]
fn cli_debugger_run_all_uses_script_override_for_mismatched_sidecar_name() {
    let (script_path, test_file_path) = write_named_test_fixture("actual_contract.sil", "suite.test.json");

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--run-all")
        .arg("--test-file")
        .arg(&test_file_path)
        .output()
        .expect("run cli-debugger --run-all with script override");

    assert!(
        output.status.success(),
        "expected success for run-all with script override, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RUN   pass_case"), "missing pass_case header: {stdout}");
    assert!(stdout.contains("RUN   fail_case"), "missing fail_case header: {stdout}");
    assert!(stdout.contains("PASS  pass_case"), "missing pass_case status: {stdout}");
    assert!(stdout.contains("PASS  fail_case"), "missing fail_case status: {stdout}");
    assert!(stdout.contains("2 tests: 2 passed, 0 failed"), "missing summary line: {stdout}");
}

#[test]
fn cli_debugger_run_test_name_infers_test_file_from_script_path() {
    let (script_path, _test_file_path) = write_test_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--run")
        .arg("--test-name")
        .arg("pass_case")
        .output()
        .expect("run cli-debugger with inferred sidecar");

    assert!(
        output.status.success(),
        "expected success for inferred run test, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS"), "expected PASS in stdout, got: {stdout}");
}

#[test]
fn cli_debugger_run_prints_console_logs_before_pass() {
    let (_script_path, test_file_path) = write_logging_test_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run")
        .arg("--test-file")
        .arg(&test_file_path)
        .arg("--test-name")
        .arg("log_case")
        .output()
        .expect("run cli-debugger logging test");

    assert!(
        output.status.success(),
        "expected success, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let seed_index = stdout.find("seed 5").expect("missing seed log");
    let sum_index = stdout.find("sum 9").expect("missing sum log");
    let pass_index = stdout.find("PASS").expect("missing PASS output");
    assert!(seed_index < sum_index && sum_index < pass_index, "unexpected stdout order: {stdout}");
}

#[test]
fn cli_debugger_run_test_name_requires_matching_sidecar_or_explicit_test_file() {
    let (script_path, _test_file_path) = write_named_test_fixture("actual_contract.sil", "suite.test.json");

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg(&script_path)
        .arg("--run")
        .arg("--test-name")
        .arg("pass_case")
        .output()
        .expect("run cli-debugger without matching inferred script");

    assert!(!output.status.success(), "expected failure when inferred sidecar script is missing");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to canonicalize test file") && stderr.contains("actual_contract.test.json"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn cli_debugger_run_all_requires_test_file() {
    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run-all")
        .output()
        .expect("run cli-debugger --run-all without test file");

    assert!(!output.status.success(), "expected failure when both script path and --test-file are missing");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--run-all requires SCRIPT_PATH or --test-file"), "unexpected stderr: {stderr}");
}

#[test]
fn cli_debugger_test_file_requires_test_name_in_run_mode() {
    let (_script_path, test_file_path) = write_test_fixture();

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run")
        .arg("--test-file")
        .arg(&test_file_path)
        .output()
        .expect("run cli-debugger --run --test-file without test-name");

    assert!(!output.status.success(), "expected failure when --test-name is missing");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--test-file requires --test-name"), "unexpected stderr: {stderr}");
}

#[test]
fn cli_debugger_test_name_requires_script_path_or_test_file() {
    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run")
        .arg("--test-name")
        .arg("pass_case")
        .output()
        .expect("run cli-debugger --run --test-name without script path or test file");

    assert!(!output.status.success(), "expected failure when neither script path nor test file is provided");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--test-name requires --test-file or SCRIPT_PATH"), "unexpected stderr: {stderr}");
}

#[test]
fn writes_dog20_debug_test_fixtures_directory() {
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dog20-debug-test-fixtures");
    let (script_path, test_file_path) = write_dog20_handoff_fixture_to(&fixtures_dir);
    assert!(script_path.exists(), "missing generated script fixture at {}", script_path.display());
    assert!(test_file_path.exists(), "missing generated test fixture at {}", test_file_path.display());
}

#[test]
fn cli_debugger_allows_double_underscore_variables_for_dog20_fixture() {
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dog20-debug-test-fixtures");
    let (_script_path, test_file_path) = write_dog20_handoff_fixture_to(&fixtures_dir);

    let output = Command::new(env!("CARGO_BIN_EXE_cli-debugger"))
        .arg("--run")
        .arg("--allow-double-underscore-variables")
        .arg("--test-file")
        .arg(&test_file_path)
        .arg("--test-name")
        .arg("dog20_handoff_until_line_803")
        .output()
        .expect("run cli-debugger dog20 fixture test");

    assert!(
        output.status.success(),
        "expected success, status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS (expected failure)"), "expected expected-failure PASS marker in stdout, got: {stdout}");
}
