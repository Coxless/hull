//! 入力欄の Tab 補完。カーソルの直前の単語を、ファイル名として補う。
//!
//! bash に問い合わせず、デーモンが今のディレクトリを見て候補を返す。補うのはファイル名だけで、
//! コマンド名や引数の補完はしない。単語の区切りと引用符は、bash の読み方に合わせる。

use std::path::{Path, PathBuf};

/// 返す候補の数の上限。
const MAX_CANDIDATES: usize = 1000;

/// 補完の結果。`text[start..cursor]` を `replacement` に置き換える。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Completion {
    /// 置き換える範囲の始まり (バイト位置)。終わりはカーソルの位置。
    pub start: usize,
    /// 置き換えるテキスト。候補がなければ空で、`start` はカーソルの位置。
    pub replacement: String,
    /// 候補が複数あるときの一覧 (ディレクトリは末尾に `/`)。1つに決まったときは空。
    pub candidates: Vec<String>,
}

/// 引用符の中か外か。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// カーソルの直前の単語のうち、補う部分 (最後の `/` より後ろ) と、その手前。
#[derive(Debug, PartialEq, Eq)]
struct Target {
    /// 最後の `/` までの、引用符や `\` を外したパス (`/` を含む)。`/` がなければ空。
    dir: String,
    /// 最後の `/` より後ろの、引用符や `\` を外した名前の書きかけ。
    prefix: String,
    /// 名前の書きかけが始まる位置 (バイト位置)。ここからカーソルまでを置き換える。
    start: usize,
    /// `start` の位置での引用符の状態。置き換えるテキストは、これに合わせて書く。
    quote: Quote,
    /// 単語が、引用符の外の `~` で始まっている。
    tilde: bool,
}

/// `text` (カーソルより前のテキスト) の最後の単語を読む。
fn target(text: &str) -> Target {
    let mut quote = Quote::None;
    let mut value = String::new();
    // 最後の `/` の時点での状態。
    let mut dir_len = 0;
    let mut start = 0;
    let mut start_quote = Quote::None;
    let mut tilde = false;
    let mut word_start = 0;

    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let after = at + c.len_utf8();
        let literal = match (quote, c) {
            (Quote::None, '\\') => chars.next().map(|(_, escaped)| escaped),
            (Quote::None, '\'') => {
                quote = Quote::Single;
                None
            }
            (Quote::None, '"') => {
                quote = Quote::Double;
                None
            }
            // 単語の区切り。次の文字から新しい単語になる。
            (Quote::None, c) if c.is_whitespace() || "|&;<>(=".contains(c) => {
                value.clear();
                (dir_len, start, start_quote, tilde, word_start) =
                    (0, after, Quote::None, false, after);
                None
            }
            (Quote::Single, '\'') | (Quote::Double, '"') => {
                quote = Quote::None;
                None
            }
            // 二重引用符の中の `\` は、特別な文字の前でだけ効く。
            (Quote::Double, '\\') => match chars.peek() {
                Some(&(_, escaped)) if "\"\\$`".contains(escaped) => {
                    chars.next();
                    Some(escaped)
                }
                _ => Some('\\'),
            },
            (_, c) => Some(c),
        };
        let Some(literal) = literal else { continue };
        if value.is_empty() && at == word_start && quote == Quote::None && c == '~' {
            tilde = true;
        }
        value.push(literal);
        if literal == '/' {
            let end = chars.peek().map_or(text.len(), |&(next, _)| next);
            (dir_len, start, start_quote) = (value.len(), end, quote);
        }
    }

    let prefix = value.split_off(dir_len);
    Target { dir: value, prefix, start, quote: start_quote, tilde }
}

/// 候補を探すディレクトリ。`~/` はホームに、相対パスは今のディレクトリからのパスにする。
fn resolve(target: &Target, cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if target.tilde {
        // `~名前/` (ほかのユーザーのホーム) は扱わない。
        let rest = target.dir.strip_prefix("~/")?;
        return Some(home?.join(rest));
    }
    Some(cwd.join(&target.dir))
}

/// `text` の `cursor` (バイト位置) の直前の単語を、ファイル名として補う。
pub(crate) fn complete(text: &str, cursor: usize, cwd: &Path, home: Option<&Path>) -> Completion {
    let nothing = Completion { start: cursor, ..Completion::default() };
    let Some(before) = text.get(..cursor) else { return nothing };
    let target = target(before);

    // `~` だけなら、ホームのディレクトリとして `/` を補う。
    if target.tilde && target.dir.is_empty() {
        return match (target.prefix.as_str(), home) {
            ("~", Some(_)) => Completion { start: cursor, replacement: "/".into(), ..nothing },
            _ => nothing,
        };
    }

    let Some(dir) = resolve(&target, cwd, home) else { return nothing };
    let mut matches = entries(&dir, &target.prefix);
    matches.sort();
    let Some(first) = matches.first() else { return nothing };

    if matches.len() == 1 {
        let Entry { name, is_dir } = first;
        // ディレクトリなら、続けて中をたどれるように `/` で終える。ファイルなら、引用符を閉じて
        // 空白を足す。
        let suffix = match (is_dir, target.quote) {
            (true, _) => "/",
            (false, Quote::None) => " ",
            (false, Quote::Single) => "' ",
            (false, Quote::Double) => "\" ",
        };
        let replacement = format!("{}{suffix}", encode(name, target.quote));
        return Completion { start: target.start, replacement, candidates: Vec::new() };
    }

    // 複数あれば、共通する部分まで補って、候補の一覧を返す。
    let common = matches.iter().skip(1).fold(first.name.as_str(), |common, entry| {
        let len = common
            .char_indices()
            .zip(entry.name.chars())
            .find(|((_, a), b)| a != b)
            .map_or(common.len().min(entry.name.len()), |((at, _), _)| at);
        &common[..len]
    });
    let candidates = matches
        .iter()
        .take(MAX_CANDIDATES)
        .map(|entry| format!("{}{}", entry.name, if entry.is_dir { "/" } else { "" }))
        .collect();
    Completion { start: target.start, replacement: encode(common, target.quote), candidates }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    name: String,
    is_dir: bool,
}

/// `dir` の中の、名前が `prefix` で始まるもの。`.` で始まる名前は、`prefix` も `.` で
/// 始まるときだけ含める。ディレクトリがなければ空。
fn entries(dir: &Path, prefix: &str) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    read.flatten()
        .filter_map(|entry| {
            // UTF-8 でない名前は、入力欄のテキストにできないので除く。
            let name = entry.file_name().into_string().ok()?;
            let hidden = name.starts_with('.') && !prefix.starts_with('.');
            if hidden || !name.starts_with(prefix) {
                return None;
            }
            // シンボリックリンクは、指している先がディレクトリかどうかで決める。
            let is_dir = std::fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir());
            Some(Entry { name, is_dir })
        })
        .collect()
}

/// 名前を、bash がそのまま読める形にする。
fn encode(name: &str, quote: Quote) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match quote {
            Quote::None if c.is_whitespace() || "\\'\"$&|;<>()*?[]{}!#~`=".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            Quote::Double if "\"\\$`".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            // 一重引用符の中には `'` を書けないので、いったん閉じて書く。
            Quote::Single if c == '\'' => out.push_str("'\\''"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用のディレクトリ。`cwd` と `home` を持つ。
    struct Tree {
        root: tempfile::TempDir,
    }

    impl Tree {
        /// `/` で終わる名前はディレクトリ、それ以外は空のファイルとして作る。
        fn new(paths: &[&str]) -> Self {
            let root = tempfile::tempdir().unwrap();
            for dir in ["cwd", "home"] {
                std::fs::create_dir(root.path().join(dir)).unwrap();
            }
            for path in paths {
                let full = root.path().join(path);
                if path.ends_with('/') {
                    std::fs::create_dir_all(&full).unwrap();
                } else {
                    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                    std::fs::write(&full, "").unwrap();
                }
            }
            Self { root }
        }

        /// `text` の末尾で補完する。
        fn complete(&self, text: &str) -> Completion {
            self.complete_at(text, text.len())
        }

        fn complete_at(&self, text: &str, cursor: usize) -> Completion {
            let home = self.root.path().join("home");
            complete(text, cursor, &self.root.path().join("cwd"), Some(&home))
        }

        /// 補完したあとのテキスト。
        fn apply(&self, text: &str) -> String {
            let completion = self.complete(text);
            format!("{}{}", &text[..completion.start], completion.replacement)
        }
    }

    #[test]
    fn a_single_match_is_completed_with_a_space() {
        let tree = Tree::new(&["cwd/readme.md", "cwd/src/"]);
        assert_eq!(tree.apply("cat rea"), "cat readme.md ");
        assert_eq!(tree.complete("cat rea").candidates, Vec::<String>::new());
    }

    #[test]
    fn a_directory_is_completed_with_a_slash() {
        let tree = Tree::new(&["cwd/src/main.rs", "cwd/src/lib.rs"]);
        assert_eq!(tree.apply("ls sr"), "ls src/");
        assert_eq!(tree.apply("ls src/m"), "ls src/main.rs ");
    }

    #[test]
    fn several_matches_are_completed_to_the_common_part_and_listed() {
        let tree = Tree::new(&["cwd/report-2025.txt", "cwd/report-2026.txt", "cwd/reports/"]);
        let completion = tree.complete("cat re");
        assert_eq!(completion.replacement, "report");
        assert_eq!(completion.candidates, ["report-2025.txt", "report-2026.txt", "reports/"]);
        assert_eq!(tree.apply("cat report-"), "cat report-202");
    }

    #[test]
    fn an_empty_word_lists_everything_but_hidden_files() {
        let tree = Tree::new(&["cwd/a.txt", "cwd/b/", "cwd/.hidden", "cwd/.config/"]);
        let completion = tree.complete("ls ");
        assert_eq!(completion.candidates, ["a.txt", "b/"]);
        assert_eq!((completion.start, completion.replacement.as_str()), (3, ""));
        assert_eq!(tree.complete("ls .").candidates, [".config/", ".hidden"]);
    }

    #[test]
    fn nothing_changes_without_a_match() {
        let tree = Tree::new(&["cwd/a.txt"]);
        let text = "cat zzz";
        assert_eq!(tree.complete(text), Completion { start: text.len(), ..Completion::default() });
        // 存在しないディレクトリの中。
        assert_eq!(tree.complete("cat nowhere/a").replacement, "");
        assert_eq!(tree.complete("cat nowhere/a").candidates, Vec::<String>::new());
    }

    #[test]
    fn names_with_spaces_are_escaped() {
        let tree = Tree::new(&["cwd/my file.txt", "cwd/my dir/inner file"]);
        assert_eq!(tree.apply("cat my"), "cat my\\ ");
        assert_eq!(tree.apply("cat my\\ f"), "cat my\\ file.txt ");
        assert_eq!(tree.apply("cat my\\ dir/in"), "cat my\\ dir/inner\\ file ");
    }

    #[test]
    fn a_word_in_quotes_stays_in_quotes() {
        let tree = Tree::new(&["cwd/my dir/inner file", "cwd/my dir/it's", "cwd/say \"hi\""]);
        // 引用符が最後の `/` より前で開いていれば、閉じて空白を足す。
        assert_eq!(tree.apply("cat \"my dir/in"), "cat \"my dir/inner file\" ");
        assert_eq!(tree.apply("cat 'my dir/in"), "cat 'my dir/inner file' ");
        assert_eq!(tree.apply("cat 'my dir/it"), "cat 'my dir/it'\\''s' ");
        // 補う名前の中で開いた引用符は、`\` での書き方に置き換える。
        assert_eq!(tree.apply("cat \"my d"), "cat my\\ dir/");
        assert_eq!(tree.apply("cat 'say"), "cat say\\ \\\"hi\\\" ");
    }

    #[test]
    fn special_characters_are_escaped() {
        let tree = Tree::new(&["cwd/a&b (1).txt", "cwd/$price"]);
        assert_eq!(tree.apply("cat a"), "cat a\\&b\\ \\(1\\).txt ");
        assert_eq!(tree.apply("cat \\$"), "cat \\$price ");
    }

    #[test]
    fn japanese_names_are_completed() {
        let tree = Tree::new(&["cwd/日本語のメモ.txt", "cwd/日本/東京.md"]);
        let completion = tree.complete("cat 日");
        assert_eq!(completion.replacement, "日本");
        assert_eq!(completion.candidates, ["日本/", "日本語のメモ.txt"]);
        assert_eq!(tree.apply("cat 日本語"), "cat 日本語のメモ.txt ");
        assert_eq!(tree.apply("cat 日本/"), "cat 日本/東京.md ");
    }

    #[test]
    fn tilde_is_the_home_directory() {
        let tree = Tree::new(&["home/Documents/notes.txt", "home/Downloads/"]);
        assert_eq!(tree.apply("ls ~"), "ls ~/");
        assert_eq!(tree.complete("ls ~/Do").candidates, ["Documents/", "Downloads/"]);
        assert_eq!(tree.apply("ls ~/Doc"), "ls ~/Documents/");
        assert_eq!(tree.apply("ls ~/Documents/n"), "ls ~/Documents/notes.txt ");
        // 引用符の中の `~` は、ただの文字。ほかのユーザーのホームは扱わない。
        assert_eq!(tree.complete("ls '~/Doc").replacement, "");
        assert_eq!(tree.complete("ls ~someone/").candidates, Vec::<String>::new());
    }

    #[test]
    fn absolute_and_parent_paths_are_followed() {
        let tree = Tree::new(&["cwd/sub/", "other/file.txt"]);
        assert_eq!(tree.apply("cat ../oth"), "cat ../other/");
        let absolute = format!("cat {}/other/f", tree.root.path().display());
        assert_eq!(
            tree.apply(&absolute),
            format!("cat {}/other/file.txt ", tree.root.path().display())
        );
    }

    #[test]
    fn the_word_starts_after_operators() {
        let tree = Tree::new(&["cwd/input.txt", "cwd/out/"]);
        assert_eq!(tree.apply("sort <inp"), "sort <input.txt ");
        assert_eq!(tree.apply("true;cat inp"), "true;cat input.txt ");
        assert_eq!(tree.apply("echo a|tee ou"), "echo a|tee out/");
        assert_eq!(tree.apply("FILE=inp"), "FILE=input.txt ");
        assert_eq!(tree.apply("echo $(cat inp"), "echo $(cat input.txt ");
        // 引用符の中の空白や記号では、単語は切れない。
        assert_eq!(tree.complete("echo 'a; inp").replacement, "");
    }

    #[test]
    fn only_the_text_before_the_cursor_is_read() {
        let tree = Tree::new(&["cwd/input.txt"]);
        let text = "cat inp | wc -l";
        let completion = tree.complete_at(text, 7);
        assert_eq!((completion.start, completion.replacement.as_str()), (4, "input.txt "));
        // 文字の途中を指すカーソルは、何もしない。
        assert_eq!(tree.complete_at("cat 日本", 5).replacement, "");
    }

    #[test]
    fn a_symlink_to_a_directory_is_a_directory() {
        let tree = Tree::new(&["cwd/real/"]);
        let cwd = tree.root.path().join("cwd");
        std::os::unix::fs::symlink(cwd.join("real"), cwd.join("link")).unwrap();
        std::os::unix::fs::symlink(cwd.join("missing"), cwd.join("broken")).unwrap();
        assert_eq!(tree.apply("ls li"), "ls link/");
        assert_eq!(tree.apply("ls bro"), "ls broken ");
    }

    #[test]
    fn the_target_is_read_like_bash_reads_words() {
        let read = |text: &str| {
            let target = target(text);
            (target.dir, target.prefix, target.start, target.quote)
        };
        assert_eq!(read("ls a/b"), ("a/".into(), "b".into(), 5, Quote::None));
        assert_eq!(read("ls \"a b/c d"), ("a b/".into(), "c d".into(), 8, Quote::Double));
        assert_eq!(read("ls a\\ b"), (String::new(), "a b".into(), 3, Quote::None));
        assert_eq!(read("ls \"a\\$b\\x"), (String::new(), "a$b\\x".into(), 3, Quote::None));
        assert_eq!(read("ls 'a'\"b\"c"), (String::new(), "abc".into(), 3, Quote::None));
        assert_eq!(read(""), (String::new(), String::new(), 0, Quote::None));
    }
}
