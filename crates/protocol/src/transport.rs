//! UI とデーモンをつなぐ Unix ソケット。

use std::io;

use interprocess::local_socket::{
    tokio::{prelude::*, Listener, RecvHalf, SendHalf, Stream},
    GenericFilePath, ListenerOptions, Name,
};

pub use interprocess::local_socket::tokio::{
    Listener as LocalListener, RecvHalf as LocalRecvHalf, SendHalf as LocalSendHalf,
    Stream as LocalStream,
};

fn name(endpoint: &str) -> io::Result<Name<'_>> {
    endpoint.to_fs_name::<GenericFilePath>()
}

/// デーモンに接続する。
pub async fn connect(endpoint: &str) -> io::Result<(RecvHalf, SendHalf)> {
    Ok(Stream::connect(name(endpoint)?).await?.split())
}

/// 接続を待ち受ける。ソケットの親ディレクトリは自分だけが読める形で作る。
///
/// 同じ接続先で別のデーモンが生きていれば `AddrInUse` を返す。
/// 前のデーモンが残したソケットファイルだけがある場合は、消して作り直す。
pub async fn listen(endpoint: &str) -> io::Result<Listener> {
    prepare_socket_dir(endpoint)?;

    match ListenerOptions::new().name(name(endpoint)?).create_tokio() {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            if Stream::connect(name(endpoint)?).await.is_ok() {
                return Err(e);
            }
            std::fs::remove_file(endpoint)?;
            ListenerOptions::new().name(name(endpoint)?).create_tokio()
        }
        Err(e) => Err(e),
    }
}

/// 接続を1つ受け付ける。
pub async fn accept(listener: &Listener) -> io::Result<(RecvHalf, SendHalf)> {
    Ok(listener.accept().await?.split())
}

fn prepare_socket_dir(endpoint: &str) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let Some(dir) = std::path::Path::new(endpoint).parent().filter(|d| !d.as_os_str().is_empty())
    else {
        return Ok(());
    };
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
