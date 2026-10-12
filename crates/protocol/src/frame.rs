use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 1フレームの本体の上限。壊れた長さを読んで巨大な確保をしないための歯止め。
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

const HEADER_LEN: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("入出力エラー: {0}")]
    Io(#[from] std::io::Error),
    #[error("フレームが大きすぎます ({0} バイト)")]
    TooLarge(usize),
    #[error("フレームの途中で接続が切れました")]
    Truncated,
    #[error("メッセージを書き出せません: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("メッセージを解釈できません: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
}

/// メッセージを「長さ + 本体」の1フレームにする。
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let mut frame = vec![0u8; HEADER_LEN];
    rmp_serde::encode::write_named(&mut frame, message)?;
    let len = frame.len() - HEADER_LEN;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    frame[..HEADER_LEN].copy_from_slice(&(len as u32).to_be_bytes());
    Ok(frame)
}

/// フレームの本体をメッセージに戻す。
///
/// 失敗したフレームは読み飛ばしてよい (新しい相手が送った未知のメッセージかもしれない)。
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    Ok(rmp_serde::from_slice(body)?)
}

/// 届いたバイト列をためて、そろったフレームの本体を順に取り出す。
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// フレームがまだそろっていなければ `Ok(None)`。
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let Some(header) = self.buf.first_chunk::<HEADER_LEN>() else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(*header) as usize;
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        if self.buf.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let body = self.buf[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.buf.drain(..HEADER_LEN + len);
        Ok(Some(body))
    }

    /// フレームの途中のバイトが残っているか。
    pub fn has_partial(&self) -> bool {
        !self.buf.is_empty()
    }
}

/// フレームを1つ読む。フレームの境目で接続が閉じられたら `Ok(None)`。
///
/// キャンセル安全ではないので、`select!` の中で直接使わない。
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        match reader.read(&mut header[filled..]).await? {
            0 if filled == 0 => return Ok(None),
            0 => return Err(FrameError::Truncated),
            n => filled += n,
        }
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => FrameError::Truncated,
        _ => FrameError::Io(e),
    })?;
    Ok(Some(body))
}

/// フレームを1つ書く。flush はしない。
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), FrameError> {
    writer.write_all(&encode(message)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlockInfo, BlockResult, ClientMessage, FinishedBlock, PromptState, RunRejection,
        ServerMessage, PROTOCOL_VERSION,
    };

    fn client_messages() -> Vec<ClientMessage> {
        vec![
            ClientMessage::Hello { version: PROTOCOL_VERSION },
            ClientMessage::Attach { cols: 120, rows: 40, raw: false },
            ClientMessage::Attach { cols: 80, rows: 24, raw: true },
            ClientMessage::Run { text: "echo 日本語\necho b".into() },
            ClientMessage::FetchOutput { id: 12 },
            ClientMessage::Input { data: b"ls -la\r".to_vec() },
            ClientMessage::Input { data: vec![0x00, 0x1b, 0xff, 0x80] },
            ClientMessage::Resize { cols: 80, rows: 24 },
            ClientMessage::Detach,
        ]
    }

    fn server_messages() -> Vec<ServerMessage> {
        vec![
            ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                daemon_version: "0.1.0".into(),
                pid: Some(4242),
                integrated: true,
            },
            ServerMessage::Welcome {
                version: 1,
                daemon_version: String::new(),
                pid: None,
                integrated: false,
            },
            ServerMessage::Replay { data: vec![b'x'; 70_000] },
            ServerMessage::Output { data: "日本語 \x1b[31mred\x1b[0m".as_bytes().to_vec() },
            ServerMessage::Output { data: Vec::new() },
            ServerMessage::Exited { code: 130 },
            ServerMessage::Error { message: "バージョンが違います".into() },
            ServerMessage::History {
                blocks: vec![FinishedBlock { info: block(1, Some("ls")), result: result(Some(0)) }],
                running: Some(block(2, None)),
                prompt: PromptState { cwd: Some("/tmp".into()), branch: Some("main".into()) },
                ready: false,
            },
            ServerMessage::Prompt { prompt: PromptState::default() },
            ServerMessage::BlockStarted { block: block(3, Some("")) },
            ServerMessage::BlockFinished { id: 3, result: result(None) },
            ServerMessage::BlockOutput { id: 3, data: vec![0x1b, 0xff], truncated: true },
            ServerMessage::RunRejected {
                reason: RunRejection::Incomplete,
                text: "echo \"a".into(),
            },
            ServerMessage::RunRejected { reason: RunRejection::Busy, text: String::new() },
        ]
    }

    fn block(id: u64, command: Option<&str>) -> BlockInfo {
        BlockInfo {
            id,
            command: command.map(Into::into),
            cwd: Some("/home/me".into()),
            started_at: 1_760_000_000_000,
            cols: 120,
            rows: 40,
        }
    }

    fn result(exit_code: Option<i32>) -> BlockResult {
        BlockResult { exit_code, finished_at: 1_760_000_000_123, fullscreen: exit_code.is_none() }
    }

    #[test]
    fn client_messages_round_trip() {
        for message in client_messages() {
            let frame = encode(&message).unwrap();
            let decoded: ClientMessage = decode(&frame[HEADER_LEN..]).unwrap();
            assert_eq!(decoded, message);
        }
    }

    #[test]
    fn server_messages_round_trip() {
        for message in server_messages() {
            let frame = encode(&message).unwrap();
            let decoded: ServerMessage = decode(&frame[HEADER_LEN..]).unwrap();
            assert_eq!(decoded, message);
        }
    }

    #[test]
    fn bytes_are_encoded_as_msgpack_bin() {
        let data = vec![7u8; 300];
        let frame = encode(&ServerMessage::Output { data }).unwrap();
        // 配列 (1要素1〜2バイト) ではなく bin16 でそのまま入っていれば、本体は 300 バイト + わずか。
        assert!(frame.len() < 300 + 32, "frame is {} bytes", frame.len());
    }

    #[test]
    fn decoder_handles_frames_split_at_every_byte() {
        let messages = server_messages();
        let stream: Vec<u8> = messages.iter().flat_map(|m| encode(m).unwrap()).collect();

        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::new();
        for byte in &stream {
            decoder.push(std::slice::from_ref(byte));
            while let Some(body) = decoder.next_frame().unwrap() {
                decoded.push(decode::<ServerMessage>(&body).unwrap());
            }
        }
        assert_eq!(decoded, messages);
        assert!(!decoder.has_partial());
    }

    #[test]
    fn decoder_handles_several_frames_in_one_push_and_uneven_chunks() {
        let messages = client_messages();
        let stream: Vec<u8> = messages.iter().flat_map(|m| encode(m).unwrap()).collect();

        for chunk_size in [1, 2, 3, 5, 7, 64, stream.len()] {
            let mut decoder = FrameDecoder::new();
            let mut decoded = Vec::new();
            for chunk in stream.chunks(chunk_size) {
                decoder.push(chunk);
                while let Some(body) = decoder.next_frame().unwrap() {
                    decoded.push(decode::<ClientMessage>(&body).unwrap());
                }
            }
            assert_eq!(decoded, messages, "chunk_size = {chunk_size}");
        }
    }

    #[test]
    fn decoder_rejects_oversized_frame() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&(MAX_FRAME_LEN as u32 + 1).to_be_bytes());
        assert!(matches!(decoder.next_frame(), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn unknown_message_fails_to_decode_but_keeps_the_stream_in_sync() {
        // 将来のデーモンが知らないメッセージを送ってきた想定。
        #[derive(serde::Serialize)]
        enum Future {
            Bell { volume: u8 },
        }
        let mut stream = encode(&Future::Bell { volume: 3 }).unwrap();
        stream.extend(encode(&ServerMessage::Exited { code: 0 }).unwrap());

        let mut decoder = FrameDecoder::new();
        decoder.push(&stream);
        let unknown = decoder.next_frame().unwrap().unwrap();
        assert!(decode::<ServerMessage>(&unknown).is_err());
        let known = decoder.next_frame().unwrap().unwrap();
        assert_eq!(decode::<ServerMessage>(&known).unwrap(), ServerMessage::Exited { code: 0 });
    }

    #[test]
    fn extra_fields_from_a_newer_peer_are_ignored() {
        #[derive(serde::Serialize)]
        enum Newer {
            Attach { cols: u16, rows: u16, pixel_width: u16 },
        }
        let frame = encode(&Newer::Attach { cols: 80, rows: 24, pixel_width: 640 }).unwrap();
        let decoded: ClientMessage = decode(&frame[HEADER_LEN..]).unwrap();
        assert_eq!(decoded, ClientMessage::Attach { cols: 80, rows: 24, raw: false });
    }

    #[tokio::test]
    async fn async_read_and_write_round_trip_over_a_tiny_pipe() {
        // バッファを 3 バイトにして、読み書きが必ず途中で分かれるようにする。
        let (mut a, mut b) = tokio::io::duplex(3);
        let messages = server_messages();
        let to_send = messages.clone();
        let writer = tokio::spawn(async move {
            for message in &to_send {
                write_frame(&mut a, message).await.unwrap();
            }
        });

        let mut received = Vec::new();
        while let Some(body) = read_frame(&mut b).await.unwrap() {
            received.push(decode::<ServerMessage>(&body).unwrap());
        }
        writer.await.unwrap();
        assert_eq!(received, messages);
    }

    #[tokio::test]
    async fn async_read_reports_truncated_frame() {
        let frame = encode(&ServerMessage::Exited { code: 1 }).unwrap();
        let mut partial = &frame[..frame.len() - 1];
        assert!(matches!(read_frame(&mut partial).await, Err(FrameError::Truncated)));
    }
}
