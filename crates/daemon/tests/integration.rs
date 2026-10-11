//! 統合スクリプトを読み込んだ bash が、コマンドの区切りの合図を出すことを確かめる結合テスト。
//!
//! 生の出力をそのまま受け取り、合図の並びを見る。

mod common;

use common::{find, start_integrated_daemon, Client};

const PLAIN_PROMPT: &str = "PS1='PROMPT> '\n";

/// 出力から合言葉を取り出す。
fn nonce(output: &[u8]) -> String {
    let prefix = b"\x1b]133;A;hull=";
    let start = find(output, prefix).expect("A の合図があること") + prefix.len();
    let len = find(&output[start..], b"\x07").expect("合図が終わっていること");
    String::from_utf8(output[start..start + len].to_vec()).unwrap()
}

/// `needles` が、この順に重ならずに現れる位置を返す。
fn positions_in_order(output: &[u8], needles: &[&str]) -> Vec<usize> {
    let mut from = 0;
    needles
        .iter()
        .map(|needle| {
            let pos = find(&output[from..], needle.as_bytes()).unwrap_or_else(|| {
                panic!(
                    "{needle:?} が順番どおりに現れません。出力:\n{:?}",
                    String::from_utf8_lossy(output)
                )
            });
            from += pos + needle.len();
            from - needle.len()
        })
        .collect()
}

async fn exit(mut client: Client) {
    client.type_line("exit").await;
    client.read_until_exit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn markers_arrive_in_order_with_the_exit_code() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.read_until("PROMPT> \x1b]133;B").await;
    let nonce = nonce(&client.output);
    client.clear_output();

    client.type_line("echo out-$((6*7))").await;
    client.read_until(&format!("\x1b]133;D;0;hull={nonce}\x07")).await;
    client.read_until("PROMPT> \x1b]133;B").await;
    positions_in_order(
        &client.output,
        &[
            &format!("\x1b]133;C;hull={nonce}\x07"),
            "out-42\r\n",
            &format!("\x1b]133;D;0;hull={nonce}\x07"),
            &format!("\x1b]133;A;hull={nonce}\x07"),
            "PROMPT> ",
            &format!("\x1b]133;B;hull={nonce}\x07"),
        ],
    );
    let c = format!("\x1b]133;C;hull={nonce}\x07");
    let start = find(&client.output, c.as_bytes()).unwrap();
    assert!(
        find(&client.output[start..], b"echo out").is_none(),
        "C のあとに、入力したコマンドの表示が出ないこと"
    );
    client.clear_output();

    client.type_line("false").await;
    client.read_until(&format!("\x1b]133;D;1;hull={nonce}\x07")).await;

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_line_runs_nothing() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.read_until("PROMPT> \x1b]133;B").await;
    client.clear_output();

    client.type_line("").await;
    client.read_until("PROMPT> \x1b]133;B").await;
    assert!(find(&client.output, b"\x1b]133;C").is_none(), "空の Enter では C が出ないこと");

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pasted_lines_end_with_a_single_d() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.read_until("PROMPT> \x1b]133;B").await;
    let nonce = nonce(&client.output);
    client.clear_output();

    client.type_line("\x1b[200~echo one-$((1+0))\necho two-$((1+1))\x1b[201~").await;
    client.read_until("two-2\r\n").await;
    client.read_until("PROMPT> \x1b]133;B").await;
    let c = format!("\x1b]133;C;hull={nonce}\x07");
    let first_c = find(&client.output, c.as_bytes()).expect("C があること");
    let executed = &client.output[first_c..];
    let d = format!("\x1b]133;D;0;hull={nonce}\x07");
    assert_eq!(executed.windows(d.len()).filter(|w| *w == d.as_bytes()).count(), 1);
    positions_in_order(executed, &["one-1\r\n", "two-2\r\n", &d]);

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unfinished_command_shows_the_continuation_marker() {
    let daemon = start_integrated_daemon("PS1='PROMPT> '\nPS2='MORE> '\n");
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.read_until("PROMPT> \x1b]133;B").await;
    let nonce = nonce(&client.output);
    client.clear_output();

    client.type_line("echo \"abc").await;
    client.read_until(&format!("\x1b]133;A;k=c;hull={nonce}\x07MORE> ")).await;
    assert!(find(&client.output, b"\x1b]133;C").is_none(), "途中の入力では C が出ないこと");

    client.send_bytes(b"\x03").await;
    client.read_until(&format!("\x1b]133;D;130;hull={nonce}\x07")).await;
    client.read_until("PROMPT> \x1b]133;B").await;

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_users_prompt_command_and_status_are_kept() {
    for prompt_command in
        [r#"PROMPT_COMMAND='echo "user-pc:$?";'"#, r#"PROMPT_COMMAND=(true 'echo "user-pc:$?"')"#]
    {
        let daemon = start_integrated_daemon(&format!("{prompt_command}\nPS1='[$?]> '\n"));
        let mut client = Client::connect_and_attach(&daemon.endpoint).await;
        client.read_until("[0]> \x1b]133;B").await;
        client.clear_output();

        client.type_line("(exit $((2+3)))").await;
        client.read_until("[5]> \x1b]133;B").await;
        positions_in_order(&client.output, &["\x1b]133;D;5;", "user-pc:5\r\n", "[5]> "]);

        exit(client).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_reset_by_prompt_command_is_wrapped_again() {
    let daemon = start_integrated_daemon("PROMPT_COMMAND='PS1=\"reset> \"'\n");
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.read_until("reset> \x1b]133;B").await;
    let nonce = nonce(&client.output);
    client.clear_output();

    client.type_line("echo again-$((1+1))").await;
    client.read_until(&format!("\x1b]133;A;hull={nonce}\x07reset> \x1b]133;B")).await;

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_nonce_is_not_passed_to_commands() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.read_until("PROMPT> \x1b]133;B").await;
    let nonce = nonce(&client.output);
    assert_eq!(nonce.len(), 32);
    client.clear_output();

    client.type_line("echo \"env=$(env | grep -c HULL_TERMINAL_NONCE)\"").await;
    client.read_until("env=0\r\n").await;

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_directory_and_git_branch_are_reported() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let home = daemon.home();
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.read_until("PROMPT> \x1b]133;B").await;
    let nonce = nonce(&client.output);
    assert!(
        find(&client.output, format!("\x1b]7133;hull={nonce};branch=\x07").as_bytes()).is_some(),
        "リポジトリの外ではブランチが空であること"
    );
    client.clear_output();

    client.type_line("mkdir 'a b' && cd 'a b' && mkdir 日本 && cd 日本").await;
    client.read_until("PROMPT> \x1b]133;B").await;
    let cwd = format!("{}/a%20b/%E6%97%A5%E6%9C%AC\x07", home.display());
    let output = String::from_utf8_lossy(&client.output).into_owned();
    let osc7 = output.find("\x1b]7;file://").expect("OSC 7 があること");
    assert!(output[osc7..].contains(&cwd), "{cwd:?} が OSC 7 にあること: {output:?}");
    client.clear_output();

    client.type_line("git init -q -b topic/one . && git commit -q --allow-empty -m init").await;
    client.read_until(&format!("\x1b]7133;hull={nonce};branch=topic/one\x07")).await;

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_integration_script_is_written_next_to_the_socket() {
    use std::os::unix::fs::PermissionsExt;

    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.read_until("PROMPT> \x1b]133;B").await;

    let script = std::path::Path::new(&daemon.endpoint).with_file_name("integration.bash");
    let mode = std::fs::metadata(&script).expect("スクリプトがあること").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    exit(client).await;
}
