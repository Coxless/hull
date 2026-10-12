//! bash の出力がコマンドごとのブロックに区切られることを確かめる結合テスト。
//!
//! ブロックの表示でつなぎ、`Run` で実行して、届いた知らせと出力を見る。

mod common;

use common::{start_integrated_daemon, start_integrated_daemon_with, Client, Daemon};
use hull_terminal_daemon::Config;
use hull_terminal_protocol::{
    BlockInfo, BlockResult, ClientMessage, FinishedBlock, PromptState, RunRejection, ServerMessage,
};

const PLAIN_PROMPT: &str = "PS1='PROMPT> '\n";

/// `Attach` への応答。
struct History {
    blocks: Vec<FinishedBlock>,
    running: Option<BlockInfo>,
    prompt: PromptState,
    ready: bool,
}

/// 始まって終わったブロック1つ。
#[derive(Debug)]
struct Ran {
    block: BlockInfo,
    output: String,
    result: BlockResult,
}

/// ブロックの表示でつなぐ。
async fn attach(daemon: &Daemon) -> (Client, History) {
    let mut client = Client::connect(&daemon.endpoint).await;
    client.hello().await;
    client.send(ClientMessage::Attach { cols: 80, rows: 24, raw: false }).await;
    match client.next_message().await {
        Some(ServerMessage::History { blocks, running, prompt, ready }) => {
            (client, History { blocks, running, prompt, ready })
        }
        other => panic!("History が来るはずが {other:?}"),
    }
}

/// つないで、最初のプロンプトが出るまで待つ。
async fn attach_ready(daemon: &Daemon) -> Client {
    let (mut client, history) = attach(daemon).await;
    if !history.ready {
        next_prompt(&mut client).await;
    }
    client
}

/// 次のプロンプトまで読み、その間に終わったブロックを返す。
async fn blocks_until_prompt(client: &mut Client) -> (Vec<Ran>, PromptState) {
    let mut ran = Vec::new();
    let mut running: Option<(BlockInfo, Vec<u8>)> = None;
    loop {
        match client.next_message().await {
            Some(ServerMessage::BlockStarted { block }) => {
                assert!(running.is_none(), "前のブロックが終わる前に始まりました: {block:?}");
                running = Some((block, Vec::new()));
            }
            Some(ServerMessage::Output { data }) => {
                running.as_mut().expect("ブロックの外で出力が届きました").1.extend(data);
            }
            Some(ServerMessage::BlockFinished { id, result }) => {
                let (block, output) = running.take().expect("始まっていないブロックが終わりました");
                assert_eq!(id, block.id);
                ran.push(Ran {
                    block,
                    output: String::from_utf8_lossy(&output).into_owned(),
                    result,
                });
            }
            Some(ServerMessage::Prompt { prompt }) => {
                assert!(running.is_none(), "ブロックが終わる前にプロンプトが出ました");
                return (ran, prompt);
            }
            other => panic!("プロンプトを待っている間に {other:?} が届きました"),
        }
    }
}

async fn next_prompt(client: &mut Client) -> PromptState {
    blocks_until_prompt(client).await.1
}

/// `text` を実行し、できたブロック1つを返す。
async fn run(client: &mut Client, text: &str) -> Ran {
    client.send(ClientMessage::Run { text: text.into() }).await;
    let (mut ran, _) = blocks_until_prompt(client).await;
    assert_eq!(ran.len(), 1, "ブロックが1つできること: {ran:?}");
    ran.remove(0)
}

async fn fetch(client: &mut Client, id: u64) -> (String, bool) {
    client.send(ClientMessage::FetchOutput { id }).await;
    match client.next_message().await {
        Some(ServerMessage::BlockOutput { id: got, data, truncated }) => {
            assert_eq!(got, id);
            (String::from_utf8_lossy(&data).into_owned(), truncated)
        }
        other => panic!("BlockOutput が来るはずが {other:?}"),
    }
}

/// 実行中のブロックの出力に `needle` が現れるまで読む。
///
/// 出力は途中で分かれて届くことがある。続きが残らないよう、行の終わりまでを `needle` にする。
async fn read_output_until(client: &mut Client, needle: &str) -> String {
    let mut output = Vec::new();
    loop {
        match client.next_message().await {
            Some(ServerMessage::Output { data } | ServerMessage::Replay { data }) => {
                output.extend(data);
                if String::from_utf8_lossy(&output).contains(needle) {
                    return String::from_utf8_lossy(&output).into_owned();
                }
            }
            other => panic!("{needle:?} を待っている間に {other:?} が届きました"),
        }
    }
}

async fn started(client: &mut Client) -> BlockInfo {
    match client.next_message().await {
        Some(ServerMessage::BlockStarted { block }) => block,
        other => panic!("BlockStarted が来るはずが {other:?}"),
    }
}

async fn exit(mut client: Client) {
    client.send(ClientMessage::Run { text: "exit".into() }).await;
    loop {
        match client.next_message().await {
            Some(ServerMessage::Exited { .. }) | None => return,
            Some(_) => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_becomes_one_block_with_only_its_output() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    let before =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
            as u64;
    let ran = run(&mut client, "echo out-$((6*7))").await;
    let first = ran.block.id;
    assert_eq!(ran.block.command.as_deref(), Some("echo out-$((6*7))"));
    assert_eq!((ran.block.cols, ran.block.rows), (80, 24));
    assert_eq!(ran.block.cwd, Some(daemon.home().to_string_lossy().into_owned()));
    assert_eq!(ran.output, "out-42\r\n", "プロンプトやコマンドの表示が混ざらないこと");
    assert_eq!(ran.result.exit_code, Some(0));
    assert!(!ran.result.fullscreen);
    assert!(ran.block.started_at >= before && ran.result.finished_at >= ran.block.started_at);

    let ran = run(&mut client, "false").await;
    assert_eq!((ran.block.id, ran.result.exit_code), (first + 1, Some(1)));
    assert_eq!(ran.output, "");

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_colored_two_line_prompt_and_prompt_command_do_not_leak_into_blocks() {
    let bashrc = concat!(
        "PROMPT_COMMAND='printf \"\\033]0;title\\007\"'\n",
        "PS1='\\[\\e[1;32m\\]\\u@\\h\\[\\e[0m\\]:\\[\\e[34m\\]\\w\\[\\e[0m\\]\\n\\$ '\n",
    );
    let daemon = start_integrated_daemon(bashrc);
    let mut client = attach_ready(&daemon).await;

    for n in 0..3 {
        let ran = run(&mut client, &format!("printf 'a\\nb-%s\\n' $(({n}+1))")).await;
        assert_eq!(ran.output, format!("a\r\nb-{}\r\n", n + 1));
    }
    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn several_lines_and_a_here_document_are_one_block() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    let text = "echo one-$((1+0))\necho two-$((1+1))\n(exit 4)";
    let ran = run(&mut client, text).await;
    assert_eq!(ran.block.command.as_deref(), Some(text));
    assert_eq!(ran.output, "one-1\r\ntwo-2\r\n");
    assert_eq!(ran.result.exit_code, Some(4));

    let ran = run(&mut client, "cat <<EOF\nhere-$((2+3))\n  indented\nEOF").await;
    assert_eq!(ran.output, "here-5\r\n  indented\r\n");

    let ran = run(&mut client, "for i in 1 2; do\n  echo loop-$((i*2))\ndone").await;
    assert_eq!(ran.output, "loop-2\r\nloop-4\r\n");

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_line_and_a_comment_make_no_block() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    for text in ["", "   ", "# コメントだけ"] {
        client.send(ClientMessage::Run { text: text.into() }).await;
        let (ran, _) = blocks_until_prompt(&mut client).await;
        assert!(ran.is_empty(), "{text:?} ではブロックができないこと: {ran:?}");
    }
    let ran = run(&mut client, "echo after-$((1+1))").await;
    assert_eq!(ran.output, "after-2\r\n");

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unfinished_command_is_rejected_and_cancelled() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    client.send(ClientMessage::Run { text: "echo \"abc".into() }).await;
    assert_eq!(
        client.next_message().await,
        Some(ServerMessage::RunRejected {
            reason: RunRejection::Incomplete,
            text: "echo \"abc".into()
        })
    );
    let (ran, _) = blocks_until_prompt(&mut client).await;
    assert!(ran.is_empty(), "途中の入力ではブロックができないこと: {ran:?}");

    // 取り消したあとは、ふつうに実行できる。
    let ran = run(&mut client, "echo ok-$((1+1))").await;
    assert_eq!(ran.output, "ok-2\r\n");

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unfinished_last_line_ends_the_block_that_already_started() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    let ran = run(&mut client, "echo first-$((1+1))\necho \"abc").await;
    assert!(ran.output.starts_with("first-2\r\n"), "{:?}", ran.output);
    assert_eq!(ran.result.exit_code, Some(130));

    let ran = run(&mut client, "echo ok-$((1+1))").await;
    assert_eq!(ran.output, "ok-2\r\n");

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_is_rejected_while_a_command_is_running() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    client.send(ClientMessage::Run { text: "echo waiting-$((1+1)); read line".into() }).await;
    started(&mut client).await;
    read_output_until(&mut client, "waiting-2\r\n").await;

    client.send(ClientMessage::Run { text: "echo no".into() }).await;
    assert_eq!(
        client.next_message().await,
        Some(ServerMessage::RunRejected { reason: RunRejection::Busy, text: "echo no".into() })
    );

    // 実行中のキー入力は、そのままコマンドに届く。
    client.send_bytes(b"\r").await;
    let (ran, _) = blocks_until_prompt_from_running(&mut client).await;
    assert_eq!(ran.exit_code, Some(0));

    exit(client).await;
}

/// 実行中のブロックが終わり、次のプロンプトが出るまで読む。
async fn blocks_until_prompt_from_running(client: &mut Client) -> (BlockResult, PromptState) {
    let mut finished = None;
    loop {
        match client.next_message().await {
            Some(ServerMessage::Output { .. }) => {}
            Some(ServerMessage::BlockFinished { result, .. }) => finished = Some(result),
            Some(ServerMessage::Prompt { prompt }) => {
                return (finished.expect("ブロックが終わっていること"), prompt)
            }
            other => panic!("ブロックの終わりを待っている間に {other:?} が届きました"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_ends_the_block_with_130() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    // 合図を出すのは、Ctrl+C が届くようになったあと (子プロセスが端末の前面に出たあと) にする。
    let text = "bash -c 'echo go-$((1+1)); exec sleep 60'";
    client.send(ClientMessage::Run { text: text.into() }).await;
    started(&mut client).await;
    read_output_until(&mut client, "go-2\r\n").await;
    client.send_bytes(b"\x03").await;
    let (result, _) = blocks_until_prompt_from_running(&mut client).await;
    assert_eq!(result.exit_code, Some(130));

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_finishes_the_block_with_the_exit_code_of_bash() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    client.send(ClientMessage::Run { text: "exit 3".into() }).await;
    let block = started(&mut client).await;
    assert_eq!(block.command.as_deref(), Some("exit 3"));
    let mut result = None;
    loop {
        match client.next_message().await {
            Some(ServerMessage::Output { .. }) => {}
            Some(ServerMessage::BlockFinished { id, result: got }) => {
                assert_eq!(id, block.id);
                result = Some(got);
            }
            Some(ServerMessage::Exited { code }) => {
                assert_eq!(code, 3);
                break;
            }
            other => panic!("Exited が来るはずが {other:?}"),
        }
    }
    assert_eq!(result.expect("ブロックが確定していること").exit_code, Some(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn reopening_restores_the_blocks_and_their_output() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut first = attach_ready(&daemon).await;
    let one = run(&mut first, "echo one-$((1+0))").await;
    let two = run(&mut first, "(echo two-$((1+1)); exit 2)").await;
    first.send(ClientMessage::Detach).await;
    drop(first);

    let (mut second, history) = attach(&daemon).await;
    assert!(history.ready);
    assert!(history.running.is_none());
    assert_eq!(history.prompt.cwd, Some(daemon.home().to_string_lossy().into_owned()));
    // 最初のプロンプトより前の出力 (/etc/bash.bashrc の案内など) がブロックになっていることがある。
    let commands: Vec<_> =
        history.blocks.into_iter().filter(|block| block.info.command.is_some()).collect();
    assert_eq!(
        commands,
        [
            FinishedBlock { info: one.block.clone(), result: one.result },
            FinishedBlock { info: two.block.clone(), result: two.result },
        ]
    );
    assert_eq!(fetch(&mut second, two.block.id).await, ("two-2\r\n".into(), false));
    assert_eq!(fetch(&mut second, one.block.id).await, ("one-1\r\n".into(), false));

    // 続きの番号で実行できる。
    let three = run(&mut second, "echo three-$((1+2))").await;
    assert_eq!((three.block.id, three.output.as_str()), (two.block.id + 1, "three-3\r\n"));

    exit(second).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reopening_while_a_command_runs_replays_its_output() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut first = attach_ready(&daemon).await;
    first.send(ClientMessage::Run { text: "echo waiting-$((1+1)); read line".into() }).await;
    let block = started(&mut first).await;
    read_output_until(&mut first, "waiting-2\r\n").await;
    drop(first);

    let (mut second, history) = attach(&daemon).await;
    assert!(!history.ready);
    assert!(history.blocks.iter().all(|block| block.info.command.is_none()));
    assert_eq!(history.running, Some(block));
    match second.next_message().await {
        Some(ServerMessage::Replay { data }) => assert_eq!(data, b"waiting-2\r\n"),
        other => panic!("Replay が来るはずが {other:?}"),
    }

    second.send_bytes(b"\r").await;
    let (result, _) = blocks_until_prompt_from_running(&mut second).await;
    assert_eq!(result.exit_code, Some(0));

    exit(second).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn output_of_bashrc_becomes_a_block_without_a_command() {
    let daemon = start_integrated_daemon("echo welcome-$((1+1))\nPS1='PROMPT> '\n");
    let (mut client, history) = attach(&daemon).await;
    let outside = if history.ready {
        history.blocks.into_iter().next().expect("ブロック外の出力があること").info
    } else {
        let (mut ran, _) = blocks_until_prompt(&mut client).await;
        assert_eq!(ran.len(), 1);
        assert!(ran[0].output.contains("welcome-2\r\n"), "{:?}", ran[0].output);
        ran.remove(0).block
    };
    assert_eq!((outside.id, outside.command), (1, None));
    assert!(fetch(&mut client, 1).await.0.contains("welcome-2\r\n"));

    // PROMPT_COMMAND の出力も、コマンドのブロックには入らない。
    for text in ["PROMPT_COMMAND+=$'\\necho from-pc'", "echo cmd-$((1+1))"] {
        client.send(ClientMessage::Run { text: text.into() }).await;
        let (ran, _) = blocks_until_prompt(&mut client).await;
        assert_eq!(ran.len(), 2, "{ran:?}");
        assert_eq!(ran[0].block.command.as_deref(), Some(text));
        assert_eq!(ran[0].output, if text.starts_with("echo") { "cmd-2\r\n" } else { "" });
        assert_eq!(ran[1].block.command, None);
        assert!(ran[1].output.starts_with("from-pc\r\n"), "{:?}", ran[1].output);
    }

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn markers_in_the_output_do_not_split_the_block() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    let ran = run(
        &mut client,
        "printf 'a\\033]133;D;0\\007b\\033]133;A\\007c\\033]133;D;0;hull=0000\\007d-%s\\n' $((1+1))",
    )
    .await;
    assert_eq!(ran.output, "a\x1b]133;D;0\x07b\x1b]133;A\x07c\x1b]133;D;0;hull=0000\x07d-2\r\n");
    assert_eq!(ran.result.exit_code, Some(0));

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_uses_the_alternate_screen_is_fullscreen() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut client = attach_ready(&daemon).await;

    let ran = run(&mut client, "printf '\\033[?1049hscreen\\033[?1049l'").await;
    assert!(ran.result.fullscreen);
    let ran = run(&mut client, "echo plain").await;
    assert!(!ran.result.fullscreen);

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_prompt_reports_the_directory_and_git_branch() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let home = daemon.home().to_string_lossy().into_owned();
    let (mut client, history) = attach(&daemon).await;
    let prompt = if history.ready { history.prompt } else { next_prompt(&mut client).await };
    assert_eq!(prompt, PromptState { cwd: Some(home.clone()), branch: None });

    client.send(ClientMessage::Run { text: "mkdir 'a b' && cd 'a b'".into() }).await;
    let (_, prompt) = blocks_until_prompt(&mut client).await;
    assert_eq!(prompt.cwd, Some(format!("{home}/a b")));

    let text = "git init -q -b topic/one . && git commit -q --allow-empty -m init";
    client.send(ClientMessage::Run { text: text.into() }).await;
    let (_, prompt) = blocks_until_prompt(&mut client).await;
    assert_eq!(prompt.branch.as_deref(), Some("topic/one"));

    // 次のブロックは、移ったあとのディレクトリで記録される。
    let ran = run(&mut client, "true").await;
    assert_eq!(ran.block.cwd, Some(format!("{home}/a b")));

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_raw_client_sees_everything_while_a_block_client_sees_blocks() {
    let daemon = start_integrated_daemon(PLAIN_PROMPT);
    let mut blocks = attach_ready(&daemon).await;
    let mut raw = Client::connect_and_attach(&daemon.endpoint).await;
    raw.read_until("PROMPT> ").await;
    raw.clear_output();

    // 生の表示で打ったコマンドも、ブロックになる (テキストはわからないので空)。
    raw.type_line("echo typed-$((1+1))").await;
    let seen = raw.read_until("PROMPT> ").await;
    assert!(seen.contains("typed-2\r\n") && seen.contains("\x1b]133;C;hull="), "{seen:?}");

    let (ran, _) = blocks_until_prompt(&mut blocks).await;
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].block.command.as_deref(), Some(""));
    assert_eq!(ran[0].output, "typed-2\r\n");

    exit(blocks).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn old_outputs_are_dropped_over_the_budget_and_long_ones_keep_the_tail() {
    let daemon = start_integrated_daemon_with(PLAIN_PROMPT, |config| Config {
        ring_capacity: 1000,
        output_budget: 1500,
        ..config
    });
    let mut client = attach_ready(&daemon).await;

    // 1行 10 バイト × 150 行 = 1500 バイト。リングに入るのは最後の 1000 バイト。
    client.send(ClientMessage::Run { text: "seq -f '%08g' 1 150".into() }).await;
    let (ran, _) = blocks_until_prompt(&mut client).await;
    let long = ran[0].block.id;
    let (output, truncated) = fetch(&mut client, long).await;
    assert!(truncated);
    assert_eq!(output.len(), 1000);
    assert!(output.ends_with("00000150\r\n"));

    let short = run(&mut client, "seq -f '%08g' 1 60").await.block.id;
    assert!(!fetch(&mut client, short).await.1);
    assert_eq!(fetch(&mut client, long).await, (String::new(), true), "古い出力は捨てられる");

    // 知らない番号には答えず、次の要求にはふつうに答える。
    client.send(ClientMessage::FetchOutput { id: 99 }).await;
    assert_eq!(fetch(&mut client, short).await.0.len(), 600);

    exit(client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_integration_script_run_is_rejected() {
    let daemon = common::start_daemon();
    let mut client = Client::connect(&daemon.endpoint).await;
    client.send(ClientMessage::Hello { version: hull_terminal_protocol::PROTOCOL_VERSION }).await;
    match client.next_message().await {
        Some(ServerMessage::Welcome { integrated, .. }) => assert!(!integrated),
        other => panic!("Welcome が来るはずが {other:?}"),
    }
    client.send(ClientMessage::Attach { cols: 80, rows: 24, raw: false }).await;
    match client.next_message().await {
        Some(ServerMessage::History { blocks, ready, .. }) => {
            assert!(blocks.is_empty() && !ready)
        }
        other => panic!("History が来るはずが {other:?}"),
    }
    client.send(ClientMessage::Run { text: "echo no".into() }).await;
    assert_eq!(
        client.next_message().await,
        Some(ServerMessage::RunRejected { reason: RunRejection::Busy, text: "echo no".into() })
    );

    // キー入力はそのまま届くので、bash は終了できる。
    client.send_bytes(b"exit\r").await;
    assert_eq!(client.next_message().await, Some(ServerMessage::Exited { code: 0 }));
}
