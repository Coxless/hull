use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::Context;

/// 合言葉を bash に渡す環境変数。統合スクリプトが読んだあとで消す。
pub(crate) const NONCE_ENV: &str = "HULL_TERMINAL_NONCE";

const SCRIPT: &str = include_str!("integration.bash");
const SCRIPT_NAME: &str = "integration.bash";

/// bash に合図 (OSC 133 / OSC 7) を出させる統合スクリプトと、その合図に付ける合言葉。
#[derive(Debug, Clone)]
pub(crate) struct Integration {
    /// 書き出したスクリプトの場所。`bash --rcfile` に渡す。
    pub script: PathBuf,
    /// 起動ごとに変わる合言葉。コマンドの出力に混ざった合図と見分けるために使う。
    pub nonce: String,
}

impl Integration {
    /// スクリプトを `dir` に書き出し、合言葉を作る。
    pub fn install(dir: &Path) -> anyhow::Result<Self> {
        let script = dir.join(SCRIPT_NAME);
        std::fs::create_dir_all(dir).with_context(|| format!("{} を作れません", dir.display()))?;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&script)
            .and_then(|mut file| file.write_all(SCRIPT.as_bytes()))
            .with_context(|| format!("{} に書けません", script.display()))?;
        Ok(Self { script, nonce: nonce()? })
    }
}

fn nonce() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .context("合言葉を作れません")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
