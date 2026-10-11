//! デーモンを実際に起動し、ソケット越しに bash を操作する結合テスト。
//!
//! 入力したコマンドの表示 (エコーバック) と結果を取り違えないよう、
//! `$((...))` の計算結果のように、入力には現れない文字列を待つ。

mod common;

use common::{find, number_between, start_daemon, start_daemon_with, Client, TIMEOUT};
use hull_terminal_daemon::{run, Config};
use hull_terminal_protocol::{ClientMessage, ServerMessage, PROTOCOL_VERSION};
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread")]
async fn echo_returns_output_and_exit_ends_the_daemon() {
    let daemon = start_daemon();
    let mut client = Client::connect(&daemon.endpoint).await;
    let pid = client.hello().await;
    assert!(pid.is_some_and(|pid| pid > 0), "bash の PID が取れていること");
    client.attach(80, 24).await;

    client.type_line("echo hello-$((40+2))").await;
    client.read_until("hello-42").await;

    client.type_line("exit 7").await;
    assert_eq!(client.read_until_exit().await, 7);

    // bash が終わったらデーモンも終わる。
    let code = timeout(TIMEOUT, daemon.handle).await.expect("デーモンが終了すること");
    assert_eq!(code.unwrap().unwrap(), 7);
    #[cfg(unix)]
    assert!(!std::path::Path::new(&daemon.endpoint).exists(), "ソケットファイルが消えていること");
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_reaches_the_same_bash_and_replays_the_screen() {
    let daemon = start_daemon();

    let mut first = Client::connect_and_attach(&daemon.endpoint).await;
    first.type_line("echo \"pid=<$$>\" marker-$((1000+234))").await;
    let seen = first.read_until("marker-1234").await;
    let pid = number_between(&seen, "pid=<", ">").expect("PID が表示されること");

    // ウィンドウを閉じる。
    first.send(ClientMessage::Detach).await;
    drop(first);

    // 開き直すと、直前の画面が戻る。
    let mut second = Client::connect(&daemon.endpoint).await;
    second.hello().await;
    let replay = String::from_utf8_lossy(&second.attach(80, 24).await).into_owned();
    assert!(replay.contains("marker-1234"), "Replay に直前の出力が入っていること: {replay:?}");

    // 同じ bash につながっている。
    second.clear_output();
    second.type_line("echo \"again=<$$>\" marker-$((2000+345))").await;
    let seen = second.read_until("marker-2345").await;
    assert_eq!(number_between(&seen, "again=<", ">"), Some(pid));

    second.type_line("exit").await;
    assert_eq!(second.read_until_exit().await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn output_produced_while_detached_is_replayed() {
    let daemon = start_daemon();

    let mut first = Client::connect_and_attach(&daemon.endpoint).await;
    first.type_line("echo ready-$((1+1))").await;
    first.read_until("ready-2").await;
    // 切断したあとに出力するコマンドを仕掛けておく。
    first.type_line("sleep 1; echo late-$((3+4))").await;
    drop(first);

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let mut second = Client::connect_and_attach(&daemon.endpoint).await;
    second.read_until("late-7").await;

    second.type_line("exit").await;
    second.read_until_exit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn killing_the_connection_without_detach_keeps_bash_alive() {
    let daemon = start_daemon();

    let mut first = Client::connect_and_attach(&daemon.endpoint).await;
    first.type_line("KEEP=kept-$((5*5))").await;
    drop(first); // Detach を送らずに切る (UI が落ちた想定)

    let mut second = Client::connect_and_attach(&daemon.endpoint).await;
    second.type_line("echo \"<$KEEP>\"").await;
    second.read_until("<kept-25>").await;

    second.type_line("exit").await;
    second.read_until_exit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_with_another_protocol_version_gets_an_error() {
    let daemon = start_daemon();

    let mut old = Client::connect(&daemon.endpoint).await;
    old.send(ClientMessage::Hello { version: PROTOCOL_VERSION + 1 }).await;
    match old.next_message().await {
        Some(ServerMessage::Error { message }) => assert!(message.contains("バージョン")),
        other => panic!("Error が来るはずが {other:?}"),
    }
    assert_eq!(old.next_message().await, None, "そのあと切断されること");

    // デーモンと bash は巻き込まれない。
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;
    client.type_line("exit").await;
    client.read_until_exit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_daemon_on_the_same_endpoint_refuses_to_start() {
    let daemon = start_daemon();
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;

    let config = Config {
        endpoint: daemon.endpoint.clone(),
        shell: Some(common::test_shell()),
        ..Config::default()
    };
    let second = timeout(TIMEOUT, run(config)).await.expect("すぐに失敗すること");
    assert!(second.is_err(), "2つ目のデーモンは起動しないこと");

    // 1つ目はそのまま使える。
    client.type_line("echo still-$((6*7))").await;
    client.read_until("still-42").await;
    client.type_line("exit").await;
    client.read_until_exit().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_stale_socket_file_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = common::test_endpoint(&dir);
    // 前のデーモンが異常終了して、ソケットファイルだけが残った想定。
    drop(std::os::unix::net::UnixListener::bind(&endpoint).unwrap());
    assert!(std::path::Path::new(&endpoint).exists());

    let config = Config {
        endpoint: endpoint.clone(),
        shell: Some(common::test_shell()),
        ..Config::default()
    };
    let handle = tokio::spawn(run(config));
    let mut client = Client::connect_and_attach(&endpoint).await;
    client.type_line("exit").await;
    client.read_until_exit().await;
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_and_resize_set_the_terminal_size() {
    let daemon = start_daemon();
    let mut client = Client::connect(&daemon.endpoint).await;
    client.hello().await;
    client.attach(100, 30).await;

    client.type_line("echo \"size=<$(stty size)>\"").await;
    client.read_until("size=<30 100>").await;

    client.send(ClientMessage::Resize { cols: 120, rows: 40 }).await;
    // リサイズが bash に届くまでの間を置かずに済むよう、通るまで繰り返す。
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        client.clear_output();
        client.type_line("echo \"size=<$(stty size)>\" end-$((1+2))").await;
        let seen = client.read_until("end-3").await;
        if seen.contains("size=<40 120>") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "リサイズが反映されません: {seen:?}");
    }

    client.type_line("exit").await;
    client.read_until_exit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn large_output_arrives_complete_and_in_order() {
    let daemon = start_daemon();
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;

    client
        .type_line("for ((i=0;i<20000;i++)); do echo \"n$((i*3))e\"; done; echo done-$((8*8))")
        .await;
    let seen = client.read_until("done-64").await;

    // ConPTY は画面を描き直すので、行がそのまま全部届くとは限らない。Ubuntu だけで確かめる。
    if cfg!(unix) {
        let mut pos = 0;
        for i in 0..20000 {
            let line = format!("n{}e\r\n", i * 3);
            match seen[pos..].find(&line) {
                Some(found) => pos += found + line.len(),
                None => panic!("{i} 行目が抜けているか、順番が違います"),
            }
        }
    }

    client.type_line("exit").await;
    client.read_until_exit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_is_limited_to_the_ring_capacity() {
    let daemon = start_daemon_with(4096);
    let mut first = Client::connect_and_attach(&daemon.endpoint).await;
    first
        .type_line("for ((i=0;i<2000;i++)); do echo \"row$((i+1))\"; done; echo done-$((9*9))")
        .await;
    first.read_until("done-81").await;
    drop(first);

    let mut second = Client::connect(&daemon.endpoint).await;
    second.hello().await;
    let replay = second.attach(80, 24).await;
    assert!(replay.len() <= 4096 + 256, "Replay は {} バイト", replay.len());
    assert!(find(&replay, b"done-81").is_some());

    second.type_line("exit").await;
    second.read_until_exit().await;
}

/// Phase 2 のブロック区切りに使う OSC 133 が、PTY を通ってそのまま届くかを確かめる。
///
/// Windows の ConPTY は制御シーケンスを書き換えることがあるので、Windows では結果を
/// 記録するだけにして、失敗にはしない (`cargo test -- --nocapture` で表示される)。
#[tokio::test(flavor = "multi_thread")]
async fn osc_133_sequences_pass_through_the_pty() {
    let daemon = start_daemon();
    let mut client = Client::connect_and_attach(&daemon.endpoint).await;

    client
        .type_line(
            r"printf '\033]133;A\007\033]133;D;0\007\033]7;file:///tmp\007'; echo osc-$((7*7))",
        )
        .await;
    client.read_until("osc-49").await;

    let a = find(&client.output, b"\x1b]133;A\x07").is_some();
    let d = find(&client.output, b"\x1b]133;D;0\x07").is_some();
    let osc7 = find(&client.output, b"\x1b]7;file:///tmp\x07").is_some();
    println!("OSC 133 passthrough on {}: A={a} D={d} OSC7={osc7}", std::env::consts::OS);
    if cfg!(unix) {
        assert!(a && d && osc7, "Ubuntu の PTY は OSC をそのまま通すこと");
    }

    client.type_line("exit").await;
    client.read_until_exit().await;
}
