//! 行协议 IO 基元：在任意 `AsyncRead` 上做「读一行（带长度上限）」与
//! 「裸字节续读」。
//!
//! 传输协议里清单/头/决定都是 `\n` 结尾的 UTF-8 行（JSON 内换行已转义），
//! 清单一行可达数百 KB（1000 项上限），因此 read_line 必须带上限防对端
//! 灌内存。文件裸流紧跟在头行之后，而读前缓冲可能已经把裸流开头吞了
//! 进来——[`LineReader::read_raw`] 先清缓冲再落到流上读，保证字节序不断。
//! 用 trait 抽象读端使回环单测可以喂 tokio duplex 而不必起 QUIC。

use std::io;

const CHUNK: usize = 8 * 1024;

/// 带读前缓冲的行读取器。`R` 按值持有（QUIC RecvStream / DuplexStream 皆可）。
pub struct LineReader<R> {
    inner: R,
    buf: Vec<u8>,
    consumed: usize,
}

impl<R: tokio::io::AsyncRead + Unpin> LineReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(CHUNK),
            consumed: 0,
        }
    }

    /// 读一行（不含 `\n`）。EOF 且无任何字节 = None；EOF 截断或超上限 =
    /// Err。行内容必须合法 UTF-8。
    pub async fn read_line(&mut self, limit: usize) -> io::Result<Option<String>> {
        loop {
            if let Some(pos) = self.buf[self.consumed..].iter().position(|&b| b == b'\n') {
                let end = self.consumed + pos;
                let line = self.buf[self.consumed..end].to_vec();
                self.consumed = end + 1;
                self.compact();
                if line.len() > limit {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "行长度超过协议上限",
                    ));
                }
                let text = String::from_utf8(line).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "协议行不是合法 UTF-8")
                })?;
                return Ok(Some(text));
            }
            if self.buf.len() - self.consumed > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "行长度超过协议上限",
                ));
            }
            self.compact();
            let mut chunk = [0u8; CHUNK];
            let n = tokio::io::AsyncReadExt::read(&mut self.inner, &mut chunk).await?;
            if n == 0 {
                if self.buf.len() > self.consumed {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "EOF 截断在行中间",
                    ));
                }
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// 裸字节读（文件段）：先取走行缓冲里的剩余字节，再从流读。
    /// 返回 0 = EOF。
    pub async fn read_raw(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        let buffered = self.buf.len() - self.consumed;
        if buffered > 0 {
            let n = buffered.min(dst.len());
            dst[..n].copy_from_slice(&self.buf[self.consumed..self.consumed + n]);
            self.consumed += n;
            self.compact();
            return Ok(n);
        }
        self.compact();
        tokio::io::AsyncReadExt::read(&mut self.inner, dst).await
    }

    fn compact(&mut self) {
        if self.consumed == self.buf.len() {
            self.buf.clear();
            self.consumed = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;
    use tokio::io::AsyncWriteExt;

    async fn collect_lines(
        mut reader: LineReader<tokio::io::DuplexStream>,
        limit: usize,
    ) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(line) = reader.read_line(limit).await.unwrap() {
            out.push(line);
        }
        out
    }

    #[tokio::test]
    async fn splits_lines_and_returns_none_at_eof() {
        let (mut tx, rx) = duplex(64);
        tx.write_all(b"one\ntwo\n").await.unwrap();
        drop(tx);
        let lines = collect_lines(LineReader::new(rx), 1024).await;
        assert_eq!(lines, vec!["one", "two"]);
    }

    #[tokio::test]
    async fn reads_line_written_across_chunk_boundary() {
        let (mut tx, rx) = duplex(64);
        let payload = "x".repeat(200);
        // duplex 缓冲 64B：写入端必须并发跑，否则单线程 runtime 下
        // write_all 等腾空、reader 等写入，互等死锁。
        let writer = tokio::spawn(async move {
            tx.write_all(payload.as_bytes()).await.unwrap();
            tx.write_all(b"\n").await.unwrap();
            drop(tx);
        });
        let lines = collect_lines(LineReader::new(rx), 4096).await;
        writer.await.unwrap();
        assert_eq!(lines, vec!["x".repeat(200)]);
    }

    #[tokio::test]
    async fn oversized_line_is_rejected() {
        let (mut tx, rx) = duplex(64);
        let writer = tokio::spawn(async move {
            tx.write_all(vec![b'y'; 300].as_slice()).await.unwrap();
            tx.write_all(b"\n").await.unwrap();
            drop(tx);
        });
        let mut reader = LineReader::new(rx);
        let err = reader.read_line(256).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        writer.abort();
    }

    #[tokio::test]
    async fn truncated_tail_is_rejected() {
        let (mut tx, rx) = duplex(64);
        tx.write_all(b"ok\npartial").await.unwrap();
        drop(tx);
        let mut reader = LineReader::new(rx);
        assert_eq!(reader.read_line(1024).await.unwrap(), Some("ok".into()));
        let err = reader.read_line(1024).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn invalid_utf8_is_rejected() {
        let (mut tx, rx) = duplex(64);
        tx.write_all(&[0xff, 0xfe, b'\n']).await.unwrap();
        drop(tx);
        let mut reader = LineReader::new(rx);
        let err = reader.read_line(1024).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_raw_drains_line_buffer_before_stream() {
        // 头行 + 裸流一次性写入：duplex 缓冲 64B，一次 read 会把行和裸流
        // 一起吞进行缓冲——read_raw 必须先吐出缓冲里的裸流开头。
        let (mut tx, rx) = duplex(64);
        tx.write_all(b"ITEM\t1\t5\nABCDE").await.unwrap();
        drop(tx);
        let mut reader = LineReader::new(rx);
        assert_eq!(
            reader.read_line(64).await.unwrap(),
            Some("ITEM\t1\t5".into())
        );
        let mut dst = [0u8; 5];
        let mut got = Vec::new();
        while got.len() < 5 {
            let n = reader.read_raw(&mut dst[got.len()..]).await.unwrap();
            assert_ne!(n, 0, "EOF 早于预期");
            got.extend_from_slice(&dst[..n]);
        }
        assert_eq!(&got, b"ABCDE");
        assert_eq!(reader.read_raw(&mut dst).await.unwrap(), 0);
    }
}
