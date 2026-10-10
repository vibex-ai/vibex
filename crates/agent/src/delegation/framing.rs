//! Bounded framing shared by the sidecar and its loopback connection.

use std::io::{self, Write};

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::MAX_MCP_MESSAGE_BYTES;

const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_HEADER_LINES: usize = 32;

/// Inspects the buffered bytes before extending the destination. Neither a
/// missing newline nor an oversized line can allocate beyond the limit.
pub(super) async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let length = newline.map_or(available.len(), |index| index + 1);
        if length > limit.saturating_sub(line.len()) {
            return Err(invalid_frame("message exceeds the transport byte limit"));
        }
        line.try_reserve_exact(length).map_err(io::Error::other)?;
        line.extend_from_slice(&available[..length]);
        reader.consume(length);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

/// Reads one newline-delimited or Content-Length framed payload. Header count
/// and aggregate header bytes are bounded independently of the payload.
pub(super) async fn read_stdio_payload(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> io::Result<Option<Vec<u8>>> {
    let first = loop {
        let Some(line) = read_bounded_line(reader, MAX_MCP_MESSAGE_BYTES).await? else {
            return Ok(None);
        };
        if !line.iter().all(u8::is_ascii_whitespace) {
            break line;
        }
    };
    if !first
        .get(..15)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"content-length:"))
    {
        return Ok(Some(first));
    }
    let header =
        std::str::from_utf8(&first).map_err(|_| invalid_frame("invalid Content-Length header"))?;
    let length = header[15..]
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|length| *length <= MAX_MCP_MESSAGE_BYTES)
        .ok_or_else(|| invalid_frame("invalid or oversized Content-Length"))?;
    let mut header_bytes = first.len();
    for _ in 0..MAX_HEADER_LINES {
        let remaining = MAX_HEADER_BYTES
            .checked_sub(header_bytes)
            .ok_or_else(|| invalid_frame("MCP headers are too large"))?;
        let line = read_bounded_line(reader, remaining).await?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete MCP headers")
        })?;
        header_bytes += line.len();
        if line.iter().all(u8::is_ascii_whitespace) {
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).await?;
            return Ok(Some(bytes));
        }
        let header = std::str::from_utf8(&line).map_err(|_| invalid_frame("invalid MCP header"))?;
        let (name, _) = header
            .split_once(':')
            .ok_or_else(|| invalid_frame("invalid MCP header"))?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            return Err(invalid_frame("duplicate Content-Length header"));
        }
    }
    Err(invalid_frame("too many MCP headers"))
}

/// Serialization is bounded while writing, before a large JSON response can
/// grow an intermediate allocation or be partially written to the transport.
pub(super) fn encode_json(message: &Value, limit: usize) -> io::Result<Vec<u8>> {
    struct BoundedBuffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for BoundedBuffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(invalid_frame("response exceeds the transport byte limit"));
            }
            self.bytes
                .try_reserve_exact(bytes.len())
                .map_err(io::Error::other)?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedBuffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, message).map_err(io::Error::other)?;
    Ok(output.bytes)
}

pub(super) async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
) -> io::Result<()> {
    writer.write_all(bytes).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

fn invalid_frame(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{BufReader, duplex};

    #[tokio::test]
    async fn an_oversized_unterminated_line_is_rejected_without_waiting_for_eof() {
        let (mut producer, consumer) = duplex(64);
        producer.write_all(&[b'x'; 33]).await.unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            read_bounded_line(&mut BufReader::with_capacity(8, consumer), 32),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn both_framings_preserve_the_following_message() {
        let first = br#"{"id":1}"#;
        let framed = format!(
            "Content-Length: {}\r\nContent-Type: application/json\r\n\r\n{}{{\"id\":2}}\n",
            first.len(),
            std::str::from_utf8(first).unwrap(),
        );
        let mut reader = BufReader::with_capacity(3, framed.as_bytes());
        assert_eq!(
            read_stdio_payload(&mut reader).await.unwrap().unwrap(),
            first
        );
        let second: Value =
            serde_json::from_slice(&read_stdio_payload(&mut reader).await.unwrap().unwrap())
                .unwrap();
        assert_eq!(second["id"], 2);
        assert!(read_stdio_payload(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn truncated_and_unbounded_headers_are_rejected() {
        let mut truncated = BufReader::new(&b"Content-Length: 8\r\n"[..]);
        assert_eq!(
            read_stdio_payload(&mut truncated).await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let headers = format!(
            "Content-Length: 2\n{}",
            "x: a\n".repeat(MAX_HEADER_LINES + 1)
        );
        assert!(
            read_stdio_payload(&mut BufReader::new(headers.as_bytes()))
                .await
                .is_err()
        );
        let headers = format!("Content-Length: 2\n{}", "x".repeat(MAX_HEADER_BYTES));
        assert!(
            read_stdio_payload(&mut BufReader::new(headers.as_bytes()))
                .await
                .is_err()
        );
        let mut duplicate = BufReader::new(&b"Content-Length: 2\nContent-Length: 2\n\n{}"[..]);
        assert!(read_stdio_payload(&mut duplicate).await.is_err());
    }

    #[test]
    fn response_encoding_counts_json_escapes_before_writing() {
        let value = serde_json::json!({ "text": "\n".repeat(20) });
        assert!(encode_json(&value, 32).is_err());
        let bytes = encode_json(&value, 128).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), value);
    }
}
