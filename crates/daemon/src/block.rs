//! ブロック (コマンドと出力の1往復) の記録。
//!
//! Phase 3 で SQLite に置き換えるまで、メモリに持つ。実行中のブロックの出力はリングにため、
//! 終わった時点の中身を残す。終了したブロックの出力の合計が上限を超えたら、古いものから捨てる
//! (コマンドや終了コードは残す)。

use std::collections::VecDeque;

use hull_terminal_protocol::{BlockInfo, BlockResult, FinishedBlock};

use crate::marker::has_visible_text;
use crate::Ring;

/// 終了したブロックの出力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredOutput {
    pub data: Vec<u8>,
    /// 先頭を省いた (長すぎたか、古いブロックで捨てた)。
    pub truncated: bool,
}

/// ブロックを始めるときにわかっていること。
#[derive(Debug, Clone, Default)]
pub(crate) struct Context {
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// 今の時刻 (Unix エポックからのミリ秒)。
    pub now: u64,
}

struct Running {
    info: BlockInfo,
    ring: Ring,
}

struct Finished {
    block: FinishedBlock,
    /// 捨てたあとは `None`。
    output: Option<Vec<u8>>,
    truncated: bool,
}

pub(crate) struct Blocks {
    /// ブロック1つの出力の上限 (バイト)。
    capacity: usize,
    /// 終了したブロックの出力の、合計の上限 (バイト)。
    budget: usize,
    next_id: u64,
    running: Option<Running>,
    finished: Vec<Finished>,
    /// 出力をまだ持っているブロックの位置 (古い順)。
    stored: VecDeque<usize>,
    stored_bytes: usize,
    /// コマンドの外で出て、まだブロックにしていない出力。
    outside: Vec<u8>,
}

impl Blocks {
    pub fn new(capacity: usize, budget: usize) -> Self {
        Self {
            capacity,
            budget,
            next_id: 1,
            running: None,
            finished: Vec::new(),
            stored: VecDeque::new(),
            stored_bytes: 0,
            outside: Vec::new(),
        }
    }

    /// ブロックを始める。前のブロックが終わっていなければ、先に終わらせる。
    pub fn start(&mut self, command: Option<String>, context: Context) -> BlockInfo {
        self.finish(BlockResult { exit_code: None, finished_at: context.now, fullscreen: false });
        let info = BlockInfo {
            id: self.next_id,
            command,
            cwd: context.cwd,
            started_at: context.now,
            cols: context.cols,
            rows: context.rows,
        };
        self.next_id += 1;
        self.running = Some(Running { info: info.clone(), ring: Ring::new(self.capacity) });
        info
    }

    /// 実行中のブロックの出力を足す。
    pub fn push_output(&mut self, data: &[u8]) {
        if let Some(running) = &mut self.running {
            running.ring.push(data);
        }
    }

    /// 実行中のブロックを終わらせ、その番号を返す。
    pub fn finish(&mut self, result: BlockResult) -> Option<u64> {
        let Running { info, ring } = self.running.take()?;
        let id = info.id;
        let output = ring.snapshot();
        self.stored_bytes += output.len();
        self.stored.push_back(self.finished.len());
        self.finished.push(Finished {
            block: FinishedBlock { info, result },
            output: Some(output),
            truncated: ring.is_truncated(),
        });
        self.evict();
        Some(id)
    }

    /// コマンドの外で出た出力をためる。
    pub fn push_outside(&mut self, data: &[u8]) {
        let room = self.capacity.saturating_sub(self.outside.len());
        self.outside.extend_from_slice(&data[..data.len().min(room)]);
    }

    /// ためてあるコマンドの外の出力を、見出しのないブロックにする。
    /// 画面に見える文字がなければ (改行やモードの切り替えだけなら) 何も作らない。
    pub fn flush_outside(&mut self, context: Context) -> Option<(FinishedBlock, Vec<u8>)> {
        let output = std::mem::take(&mut self.outside);
        if !has_visible_text(&output) {
            return None;
        }
        let result = BlockResult { exit_code: None, finished_at: context.now, fullscreen: false };
        self.start(None, context);
        self.push_output(&output);
        self.finish(result);
        self.finished.last().map(|finished| (finished.block.clone(), output))
    }

    pub fn running(&self) -> Option<&BlockInfo> {
        self.running.as_ref().map(|running| &running.info)
    }

    /// 実行中のブロックの、ここまでの出力。
    pub fn running_output(&self) -> Option<Vec<u8>> {
        self.running.as_ref().map(|running| running.ring.snapshot())
    }

    pub fn finished(&self) -> Vec<FinishedBlock> {
        self.finished.iter().map(|finished| finished.block.clone()).collect()
    }

    /// 終了したブロックの出力。そのブロックがなければ `None`。
    pub fn output(&self, id: u64) -> Option<StoredOutput> {
        // 番号は 1 から順に付けていて、終了した順に並んでいる。
        let index =
            self.finished.binary_search_by_key(&id, |finished| finished.block.info.id).ok()?;
        let finished = &self.finished[index];
        Some(match &finished.output {
            Some(data) => StoredOutput { data: data.clone(), truncated: finished.truncated },
            None => StoredOutput { data: Vec::new(), truncated: true },
        })
    }

    /// 合計が上限に収まるまで、古いブロックの出力を捨てる。最後のブロックは残す。
    fn evict(&mut self) {
        while self.stored_bytes > self.budget && self.stored.len() > 1 {
            let Some(index) = self.stored.pop_front() else { break };
            if let Some(output) = self.finished[index].output.take() {
                self.stored_bytes -= output.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(now: u64) -> Context {
        Context { cwd: Some("/home/me".into()), cols: 80, rows: 24, now }
    }

    fn result(exit_code: i32, finished_at: u64) -> BlockResult {
        BlockResult { exit_code: Some(exit_code), finished_at, fullscreen: false }
    }

    fn run(blocks: &mut Blocks, command: &str, output: &[u8]) -> u64 {
        let id = blocks.start(Some(command.into()), context(0)).id;
        blocks.push_output(output);
        blocks.finish(result(0, 0));
        id
    }

    #[test]
    fn a_block_is_recorded_with_its_output() {
        let mut blocks = Blocks::new(1024, 1024);
        let info = blocks.start(Some("echo hi".into()), context(1000));
        assert_eq!((info.id, info.cols, info.rows, info.started_at), (1, 80, 24, 1000));
        assert_eq!(blocks.running(), Some(&info));

        blocks.push_output(b"hi\r\n");
        assert_eq!(blocks.running_output().as_deref(), Some(b"hi\r\n".as_slice()));
        assert_eq!(blocks.output(1), None, "実行中のブロックの出力は Replay で渡す");

        assert_eq!(blocks.finish(result(3, 1500)), Some(1));
        assert_eq!(blocks.running(), None);
        assert_eq!(blocks.finished(), [FinishedBlock { info, result: result(3, 1500) }]);
        assert_eq!(
            blocks.output(1),
            Some(StoredOutput { data: b"hi\r\n".to_vec(), truncated: false })
        );
        assert_eq!(blocks.finish(result(0, 1600)), None, "二重には終わらない");
    }

    #[test]
    fn numbers_go_up_from_one() {
        let mut blocks = Blocks::new(1024, 1024);
        assert_eq!(run(&mut blocks, "a", b""), 1);
        assert_eq!(run(&mut blocks, "b", b""), 2);
        assert_eq!(blocks.output(3), None);
        assert_eq!(blocks.output(0), None);
    }

    #[test]
    fn a_long_output_keeps_its_tail() {
        let mut blocks = Blocks::new(4, 1024);
        let id = run(&mut blocks, "seq", b"0123456789");
        assert_eq!(
            blocks.output(id),
            Some(StoredOutput { data: b"6789".to_vec(), truncated: true })
        );
    }

    #[test]
    fn old_outputs_are_dropped_when_the_total_is_over_the_budget() {
        let mut blocks = Blocks::new(1024, 10);
        let first = run(&mut blocks, "a", b"aaaa");
        let second = run(&mut blocks, "b", b"bbbb");
        assert_eq!(blocks.output(first).unwrap().data, b"aaaa");

        let third = run(&mut blocks, "c", b"cccc");
        assert_eq!(blocks.output(first), Some(StoredOutput { data: Vec::new(), truncated: true }));
        assert_eq!(blocks.output(second).unwrap().data, b"bbbb");
        assert_eq!(blocks.output(third).unwrap().data, b"cccc");
        assert_eq!(blocks.finished().len(), 3, "コマンドや終了コードは残る");
    }

    #[test]
    fn the_last_output_is_kept_even_if_it_is_over_the_budget() {
        let mut blocks = Blocks::new(1024, 2);
        let first = run(&mut blocks, "a", b"aaaa");
        assert_eq!(blocks.output(first).unwrap().data, b"aaaa");
        let second = run(&mut blocks, "b", b"bbbb");
        assert!(blocks.output(first).unwrap().data.is_empty());
        assert_eq!(blocks.output(second).unwrap().data, b"bbbb");
    }

    #[test]
    fn outside_output_becomes_a_block_without_a_command() {
        let mut blocks = Blocks::new(1024, 1024);
        blocks.push_outside(b"wel");
        blocks.push_outside(b"come\r\n");
        let (block, output) = blocks.flush_outside(context(7)).unwrap();
        assert_eq!(block.info.command, None);
        assert_eq!((block.info.id, block.info.started_at, block.result.finished_at), (1, 7, 7));
        assert_eq!(output, b"welcome\r\n");
        assert_eq!(blocks.output(1).unwrap().data, b"welcome\r\n");
        assert!(blocks.flush_outside(context(8)).is_none(), "同じ出力を二度ブロックにしない");
    }

    #[test]
    fn outside_output_without_visible_text_makes_no_block() {
        let mut blocks = Blocks::new(1024, 1024);
        blocks.push_outside(b"\r\n\x1b[?2004h");
        assert!(blocks.flush_outside(context(0)).is_none());
        assert!(blocks.finished().is_empty());

        blocks.push_outside(b"x");
        assert_eq!(blocks.flush_outside(context(0)).unwrap().1, b"x", "前のぶんは残っていない");
    }

    #[test]
    fn starting_a_block_finishes_the_one_left_running() {
        let mut blocks = Blocks::new(1024, 1024);
        blocks.start(Some("a".into()), context(1));
        let second = blocks.start(Some("b".into()), context(2));
        assert_eq!(second.id, 2);
        let finished = blocks.finished();
        assert_eq!(finished.len(), 1);
        assert_eq!(
            finished[0].result,
            BlockResult { exit_code: None, finished_at: 2, fullscreen: false }
        );
    }
}
