//! bash の出力に入っている合図を読み、コマンドごとに区切る。
//!
//! 統合スクリプトが出す OSC 133 (A・B・C・D) の位置でバイト列を切り、
//! 「コマンドの出力」「ブロックの外の出力」に分ける。プロンプトと、bash が表示し直す
//! コマンドは捨てる。入出力を持たない部品で、読み取りの境目がどこにあっても結果は変わらない。

use vte::{Params, Parser, Perform};

const ESC: u8 = 0x1b;

/// 合図かどうかがわかるまで、出力に回さずに持っておく長さの上限。
///
/// 統合スクリプトの合図はこれより短い。これより長い制御シーケンスは合図ではないので、
/// 途中でも出力に回す (終わらない OSC で出力が止まらないように)。
const MAX_HELD: usize = 256;

/// 区切った結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// 実行中のコマンドの出力。
    Output(Vec<u8>),
    /// コマンドの外で出た出力 (`.bashrc` や `PROMPT_COMMAND` の出力)。
    Outside(Vec<u8>),
    Event(Event),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// プロンプトが出て、入力を受け付けられる。
    Prompt(PromptInfo),
    /// コマンドが途中で、bash が続きの入力を待っている (`PS2`)。
    Continuation,
    /// コマンドの実行が始まった。
    Started,
    /// コマンドが終わった。
    Finished {
        /// 終了コード。合図から読み取れなかったときは `None`。
        exit_code: Option<i32>,
        /// 代替画面を使った (vim などの全画面アプリ)。
        fullscreen: bool,
    },
}

/// プロンプトと一緒に届く、bash の今の状態。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PromptInfo {
    /// 今のディレクトリ。
    pub cwd: Option<String>,
    /// git ブランチ。リポジトリの外では `None`。
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// 最初のプロンプトが出る前。
    Starting,
    /// プロンプトを表示して、入力を待っている (A → C)。
    Prompt,
    /// コマンドを実行している (C → D)。
    Running,
    /// コマンドが終わり、次のプロンプトを準備している (D → A)。
    Between,
}

/// 制御シーケンスを1つ読み終えたときにわかったこと。
#[derive(Debug, Default)]
struct Seen {
    /// シーケンスが終わった (または、シーケンスではなかった)。
    done: bool,
    mark: Option<Mark>,
}

#[derive(Debug, PartialEq, Eq)]
enum Mark {
    PromptStart,
    Continuation,
    PromptEnd,
    CommandStart,
    CommandEnd(Option<i32>),
    Cwd(String),
    Branch(Option<String>),
    AlternateScreen,
}

struct Reader<'a> {
    nonce: &'a str,
    seen: Seen,
}

impl Perform for Reader<'_> {
    fn print(&mut self, _c: char) {
        self.seen.done = true;
    }

    fn execute(&mut self, _byte: u8) {
        self.seen.done = true;
    }

    fn unhook(&mut self) {
        self.seen.done = true;
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.seen.done = true;
        self.seen.mark = parse_osc(params, self.nonce);
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        self.seen.done = true;
        let alternate = intermediates == b"?"
            && action == 'h'
            && params.iter().flatten().any(|&mode| matches!(mode, 47 | 1047 | 1049));
        if alternate {
            self.seen.mark = Some(Mark::AlternateScreen);
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {
        self.seen.done = true;
    }
}

fn parse_osc(params: &[&[u8]], nonce: &str) -> Option<Mark> {
    let (&kind, rest) = params.split_first()?;
    // OSC 7 は合言葉を付けられない。受け取る側で、コマンドの実行中のものを捨てる。
    if kind == b"7" {
        return parse_file_url(rest.first()?).map(Mark::Cwd);
    }
    let signed = rest
        .iter()
        .any(|param| param.strip_prefix(b"hull=").is_some_and(|value| value == nonce.as_bytes()));
    if !signed {
        return None;
    }
    match kind {
        b"133" => match *rest.first()? {
            b"A" if rest.contains(&b"k=c".as_slice()) => Some(Mark::Continuation),
            b"A" => Some(Mark::PromptStart),
            b"B" => Some(Mark::PromptEnd),
            b"C" => Some(Mark::CommandStart),
            b"D" => {
                let code = rest.get(1).and_then(|param| std::str::from_utf8(param).ok());
                Some(Mark::CommandEnd(code.and_then(|code| code.parse().ok())))
            }
            _ => None,
        },
        b"7133" => rest
            .iter()
            .find_map(|param| param.strip_prefix(b"branch="))
            .map(|branch| Mark::Branch(Some(percent_decode(branch)).filter(|b| !b.is_empty()))),
        _ => None,
    }
}

/// `file://<ホスト>/<パス>` からパスを取り出す。
fn parse_file_url(url: &[u8]) -> Option<String> {
    let rest = url.strip_prefix(b"file://")?;
    let path = &rest[rest.iter().position(|&byte| byte == b'/')?..];
    Some(percent_decode(path))
}

fn percent_decode(text: &[u8]) -> String {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let mut rest = bytes.clone();
            let hex = rest.next().zip(rest.next()).and_then(|(high, low)| {
                Some((char::from(high).to_digit(16)? * 16 + char::from(low).to_digit(16)?) as u8)
            });
            if let Some(decoded) = hex {
                out.push(decoded);
                bytes = rest;
                continue;
            }
        }
        out.push(byte);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 出力を合図の位置で区切る。
pub(crate) struct Splitter {
    parser: Parser,
    nonce: String,
    state: State,
    /// 制御シーケンスの途中。`ESC` から、読み終えるまで。
    in_sequence: bool,
    /// 合図かどうかがわかるまで持っているバイト列 (`ESC` から)。
    held: Vec<u8>,
    /// 長すぎて、持たずに出力へ回すことにした。
    overflowed: bool,
    prompt: PromptInfo,
    fullscreen: bool,
}

impl Splitter {
    pub fn new(nonce: impl Into<String>) -> Self {
        Self {
            parser: Parser::new(),
            nonce: nonce.into(),
            state: State::Starting,
            in_sequence: false,
            held: Vec::new(),
            overflowed: false,
            prompt: PromptInfo::default(),
            fullscreen: false,
        }
    }

    /// コマンドを実行している最中か。
    pub fn is_running(&self) -> bool {
        self.state == State::Running
    }

    /// プロンプトを表示して、入力を待っているか。
    pub fn is_at_prompt(&self) -> bool {
        self.state == State::Prompt
    }

    /// 最後に届いたディレクトリと git ブランチ。
    pub fn prompt(&self) -> &PromptInfo {
        &self.prompt
    }

    /// 実行中のコマンドが代替画面を使ったか。
    pub fn fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// PTY から読んだバイト列を渡す。区切った結果を `out` の後ろに足す。
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<Item>) {
        let mut rest = data;
        while !rest.is_empty() {
            if !self.in_sequence {
                // 制御シーケンスの外は、次の ESC までまとめて流す。
                let plain = rest.iter().position(|&byte| byte == ESC).unwrap_or(rest.len());
                self.emit(&rest[..plain], out);
                rest = &rest[plain..];
                if rest.is_empty() {
                    break;
                }
                self.in_sequence = true;
            }

            let byte = rest[0];
            rest = &rest[1..];
            let mut reader = Reader { nonce: &self.nonce, seen: Seen::default() };
            self.parser.advance(&mut reader, &[byte]);
            let seen = reader.seen;

            if !seen.done {
                self.hold(byte, out);
                continue;
            }
            // ESC で終わったなら、その ESC は次のシーケンスの始まりでもある。
            let starts_next = byte == ESC;
            if !starts_next {
                self.hold(byte, out);
            }
            self.finish_sequence(seen.mark, out);
            if starts_next {
                self.in_sequence = true;
                self.hold(byte, out);
            }
        }
    }

    /// 制御シーケンスの途中のバイトを持っておく。長くなりすぎたら出力に回す。
    fn hold(&mut self, byte: u8, out: &mut Vec<Item>) {
        if self.overflowed {
            self.emit(&[byte], out);
            return;
        }
        self.held.push(byte);
        if self.held.len() > MAX_HELD {
            let held = std::mem::take(&mut self.held);
            self.emit(&held, out);
            self.overflowed = true;
        }
    }

    fn finish_sequence(&mut self, mark: Option<Mark>, out: &mut Vec<Item>) {
        let held = std::mem::take(&mut self.held);
        let overflowed = std::mem::take(&mut self.overflowed);
        self.in_sequence = false;

        // 統合スクリプトの合図は出力に残さない。ほかの制御シーケンスはそのまま流す。
        let is_marker = !overflowed
            && !matches!(mark, None | Some(Mark::AlternateScreen))
            && !(self.state == State::Running && matches!(mark, Some(Mark::Cwd(_))));
        if !is_marker {
            self.emit(&held, out);
        }
        if let Some(mark) = mark.filter(|_| !overflowed) {
            self.apply(mark, out);
        }
    }

    fn apply(&mut self, mark: Mark, out: &mut Vec<Item>) {
        match (mark, self.state) {
            (Mark::PromptStart, State::Prompt) => {}
            (Mark::PromptStart, state) => {
                // D が来ないままプロンプトに戻った (PROMPT_COMMAND を書き換えられたなど)。
                if state == State::Running {
                    self.finish_command(None, out);
                }
                self.state = State::Prompt;
                out.push(Item::Event(Event::Prompt(self.prompt.clone())));
            }
            (Mark::Continuation, State::Prompt) => out.push(Item::Event(Event::Continuation)),
            (Mark::CommandStart, State::Prompt | State::Between) => {
                self.state = State::Running;
                self.fullscreen = false;
                out.push(Item::Event(Event::Started));
            }
            (Mark::CommandEnd(code), State::Running) => {
                self.finish_command(code, out);
                self.state = State::Between;
            }
            // 何も実行しなかった (空の Enter や Ctrl+C)。次のプロンプトの準備に入る。
            (Mark::CommandEnd(_), State::Prompt) => self.state = State::Between,
            (Mark::AlternateScreen, State::Running) => self.fullscreen = true,
            (Mark::Cwd(cwd), state) if state != State::Running => self.prompt.cwd = Some(cwd),
            (Mark::Branch(branch), _) => self.prompt.branch = branch,
            _ => {}
        }
    }

    fn finish_command(&mut self, exit_code: Option<i32>, out: &mut Vec<Item>) {
        let fullscreen = std::mem::take(&mut self.fullscreen);
        out.push(Item::Event(Event::Finished { exit_code, fullscreen }));
    }

    fn emit(&mut self, bytes: &[u8], out: &mut Vec<Item>) {
        if bytes.is_empty() {
            return;
        }
        match (self.state, out.last_mut()) {
            (State::Prompt, _) => {}
            (State::Running, Some(Item::Output(last))) => last.extend_from_slice(bytes),
            (State::Running, _) => out.push(Item::Output(bytes.to_vec())),
            (_, Some(Item::Outside(last))) => last.extend_from_slice(bytes),
            (_, _) => out.push(Item::Outside(bytes.to_vec())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "0123abcd";

    fn mark(body: &str) -> String {
        format!("\x1b]133;{body};hull={NONCE}\x07")
    }

    fn branch(name: &str) -> String {
        format!("\x1b]7133;hull={NONCE};branch={name}\x07")
    }

    fn output(text: &str) -> Item {
        Item::Output(text.as_bytes().to_vec())
    }

    fn outside(text: &str) -> Item {
        Item::Outside(text.as_bytes().to_vec())
    }

    fn prompt(cwd: Option<&str>, branch: Option<&str>) -> Item {
        Item::Event(Event::Prompt(PromptInfo {
            cwd: cwd.map(Into::into),
            branch: branch.map(Into::into),
        }))
    }

    fn started() -> Item {
        Item::Event(Event::Started)
    }

    fn finished(exit_code: i32) -> Item {
        Item::Event(Event::Finished { exit_code: Some(exit_code), fullscreen: false })
    }

    /// となり合う同じ種類の出力をつなげる (読み取りの境目で分かれたぶんを戻す)。
    fn merge(items: Vec<Item>) -> Vec<Item> {
        let mut merged: Vec<Item> = Vec::new();
        for item in items {
            match (merged.last_mut(), item) {
                (Some(Item::Output(last)), Item::Output(more)) => last.extend(more),
                (Some(Item::Outside(last)), Item::Outside(more)) => last.extend(more),
                (_, item) => merged.push(item),
            }
        }
        merged
    }

    fn split_in(stream: &[u8], chunk: usize) -> Vec<Item> {
        let mut splitter = Splitter::new(NONCE);
        let mut items = Vec::new();
        for piece in stream.chunks(chunk) {
            splitter.feed(piece, &mut items);
        }
        merge(items)
    }

    /// まとめて渡しても、1バイトずつ渡しても、同じ結果になることを確かめて返す。
    fn split(stream: impl AsRef<[u8]>) -> Vec<Item> {
        let stream = stream.as_ref();
        let whole = split_in(stream, stream.len().max(1));
        for chunk in [1, 2, 3, 7, 64] {
            assert_eq!(split_in(stream, chunk), whole, "{chunk} バイトずつ渡したとき");
        }
        whole
    }

    /// 最初のプロンプトが出るまで。
    fn startup() -> String {
        [&mark("D;0"), "\x1b]7;file://host/home/me\x07", &branch(""), &mark("A"), "$ ", &mark("B")]
            .concat()
    }

    /// コマンドが終わってから、次のプロンプトが出るまで。
    fn next_prompt(exit_code: i32) -> String {
        [
            &mark(&format!("D;{exit_code}")),
            &branch(""),
            "\r\n\x1b[?2004h",
            &mark("A"),
            "$ ",
            &mark("B"),
        ]
        .concat()
    }

    #[test]
    fn a_command_becomes_output_between_started_and_finished() {
        let stream =
            [&startup(), "echo hi\r\n\x1b[?2004l\r", &mark("C"), "hi\r\n", &next_prompt(0)]
                .concat();
        assert_eq!(
            split(stream),
            [
                prompt(Some("/home/me"), None),
                started(),
                output("hi\r\n"),
                finished(0),
                outside("\r\n\x1b[?2004h"),
                prompt(Some("/home/me"), None),
            ]
        );
    }

    #[test]
    fn the_exit_code_is_reported() {
        let stream = [&startup(), "false\r\n", &mark("C"), &next_prompt(1)].concat();
        assert_eq!(split(stream)[1..3], [started(), finished(1)]);
    }

    #[test]
    fn output_before_the_first_prompt_is_outside() {
        let stream = ["welcome\r\n", &startup()].concat();
        assert_eq!(split(stream), [outside("welcome\r\n"), prompt(Some("/home/me"), None)]);
    }

    #[test]
    fn an_empty_line_starts_nothing() {
        let stream = [&startup(), "\r\n", &next_prompt(0)].concat();
        assert_eq!(
            split(stream),
            [
                prompt(Some("/home/me"), None),
                outside("\r\n\x1b[?2004h"),
                prompt(Some("/home/me"), None)
            ]
        );
    }

    #[test]
    fn several_pasted_lines_are_one_command() {
        let stream = [
            &startup(),
            "echo a\r\n",
            &mark("C"),
            "a\r\n",
            &mark("C"),
            "b\r\n",
            &mark("C"),
            &next_prompt(1),
        ]
        .concat();
        assert_eq!(split(stream)[1..4], [started(), output("a\r\nb\r\n"), finished(1)]);
    }

    #[test]
    fn a_redrawn_prompt_is_not_announced_again() {
        let stream =
            [&startup(), "echo a", "\x1b[A\r", &mark("A"), "$ ", &mark("B"), "echo a"].concat();
        assert_eq!(split(stream), [prompt(Some("/home/me"), None)]);
    }

    #[test]
    fn an_unfinished_command_is_announced_and_cancelled() {
        let stream =
            [&startup(), "echo \"a\r\n", &mark("A;k=c"), "> ", "^C\r\n", &next_prompt(130)]
                .concat();
        assert_eq!(
            split(stream),
            [
                prompt(Some("/home/me"), None),
                Item::Event(Event::Continuation),
                outside("\r\n\x1b[?2004h"),
                prompt(Some("/home/me"), None),
            ]
        );
    }

    #[test]
    fn marks_with_another_nonce_stay_in_the_output() {
        let forged = "\x1b]133;D;0;hull=ffff\x07\x1b]133;D;0\x07\x1b]133;A\x07";
        let stream = [&startup(), &mark("C"), "a", forged, "b", &next_prompt(0)].concat();
        let items = split(stream);
        assert_eq!(items[1..4], [started(), output(&format!("a{forged}b")), finished(0)]);
    }

    #[test]
    fn other_control_sequences_pass_through_unchanged() {
        let text = "\x1b[1;31mred\x1b[0m\x1b]8;;http://example.com\x1b\\link\x1b]8;;\x1b\\\x1b(B\x1bPq#0\x1b\\\x1b_app\x1b\\日本語";
        let stream = [&startup(), &mark("C"), text, &next_prompt(0)].concat();
        assert_eq!(split(stream)[2], output(text));
    }

    #[test]
    fn a_control_sequence_that_never_ends_does_not_hold_the_output_back() {
        let long = format!("\x1b]52;c;{}", "A".repeat(10_000));
        let mut splitter = Splitter::new(NONCE);
        let mut items = Vec::new();
        splitter.feed([&startup(), &mark("C"), long.as_str()].concat().as_bytes(), &mut items);
        assert_eq!(merge(items)[2], output(&long));

        // そのあとに来た合図は、変わらず読める。
        let mut items = Vec::new();
        splitter.feed(next_prompt(0).as_bytes(), &mut items);
        assert_eq!(items[0], finished(0));
    }

    #[test]
    fn the_directory_and_branch_follow_the_prompt() {
        let stream = [
            &startup(),
            &mark("C"),
            &mark("D;0"),
            "\x1b]7;file://host/tmp/a%20b/%E6%97%A5%E6%9C%AC\x07",
            &branch("topic/one"),
            &mark("A"),
        ]
        .concat();
        assert_eq!(split(stream).last(), Some(&prompt(Some("/tmp/a b/日本"), Some("topic/one"))));
    }

    #[test]
    fn a_directory_reported_by_a_running_command_is_ignored() {
        let osc7 = "\x1b]7;file://remote/srv\x07";
        let stream = [&startup(), &mark("C"), osc7, &mark("D;0"), &mark("A")].concat();
        assert_eq!(
            split(stream)[1..],
            [started(), output(osc7), finished(0), prompt(Some("/home/me"), None)]
        );
    }

    #[test]
    fn a_command_that_uses_the_alternate_screen_is_fullscreen() {
        let stream = [
            &startup(),
            &mark("C"),
            "\x1b[?1049h\x1b[2Jvim\x1b[?1049l",
            &mark("D;0"),
            &mark("A"),
            &mark("C"),
            "plain",
            &mark("D;0"),
        ]
        .concat();
        let items = split(stream);
        assert_eq!(items[3], Item::Event(Event::Finished { exit_code: Some(0), fullscreen: true }));
        assert_eq!(items.last(), Some(&finished(0)));
    }

    #[test]
    fn a_prompt_without_the_end_mark_still_finishes_the_command() {
        let stream = [&startup(), &mark("C"), "out", &mark("A"), "$ "].concat();
        assert_eq!(
            split(stream)[1..],
            [
                started(),
                output("out"),
                Item::Event(Event::Finished { exit_code: None, fullscreen: false }),
                prompt(Some("/home/me"), None),
            ]
        );
    }

    #[test]
    fn the_state_is_visible_between_chunks() {
        let mut splitter = Splitter::new(NONCE);
        let mut items = Vec::new();
        splitter.feed(startup().as_bytes(), &mut items);
        assert!(splitter.is_at_prompt() && !splitter.is_running());
        assert_eq!(splitter.prompt().cwd.as_deref(), Some("/home/me"));

        splitter.feed([&mark("C"), "\x1b[?1049h"].concat().as_bytes(), &mut items);
        assert!(splitter.is_running() && splitter.fullscreen());
    }
}
