use bytes::{Bytes, BytesMut, Buf};

pub struct SseCodec {
    buffer: BytesMut,
}

impl SseCodec {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
        }
    }

    pub fn decode(&mut self, chunk: Bytes) -> Vec<String> {
        self.buffer.extend_from_slice(&chunk);
        let mut lines = Vec::new();

        while let Some(i) = self.buffer.iter().position(|&b| b == b'\n') {
            let line_bytes = self.buffer.split_to(i);
            self.buffer.advance(1); // skip newline
            
            // Handle \r if present (CRLF)
            let line_slice = if line_bytes.ends_with(b"\r") {
                &line_bytes[..line_bytes.len() - 1]
            } else {
                &line_bytes[..]
            };

            if let Ok(line) = std::str::from_utf8(line_slice) {
                if !line.is_empty() {
                    lines.push(line.to_string());
                }
            }
        }
        
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_codec_fragmentation() {
        let mut codec = SseCodec::new();
        
        let chunk1 = Bytes::from("data: {\"foo\":");
        let lines = codec.decode(chunk1);
        assert!(lines.is_empty());

        let chunk2 = Bytes::from(" \"bar\"}\n\ndata: [DO");
        let lines = codec.decode(chunk2);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "data: {\"foo\": \"bar\"}");

        let chunk3 = Bytes::from("NE]\n");
        let lines = codec.decode(chunk3);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "data: [DONE]");
    }
    
    #[test]
    fn test_sse_codec_crlf() {
        let mut codec = SseCodec::new();
        let chunk = Bytes::from("data: foo\r\ndata: bar\r\n");
        let lines = codec.decode(chunk);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "data: foo");
        assert_eq!(lines[1], "data: bar");
    }
}
