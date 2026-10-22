//! A WebSocket server endpoint (RFC 6455), std only: the opening handshake
//! on an accepted HTTP request, and text frames in and out. Enough for a
//! JSON-RPC peer; no extensions, no fragments longer than memory.

use std::io::{self, Read, Write};
use std::net::TcpStream;

const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Largest message accepted from a peer.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// SHA-1 of `data` (only for the handshake's accept key; not security).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The `Sec-WebSocket-Accept` value for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    let mut data = key.trim().as_bytes().to_vec();
    data.extend_from_slice(GUID);
    base64(&sha1(&data))
}

/// Whether a request asks to upgrade to a WebSocket.
pub fn wants_upgrade(headers: &[(String, String)]) -> bool {
    let h = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
    h("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        && h("connection").is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")))
        && h("sec-websocket-key").is_some()
}

/// Answer the opening handshake (the caller checked [`wants_upgrade`]).
pub fn upgrade(mut stream: &TcpStream, headers: &[(String, String)]) -> io::Result<()> {
    let key = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key"))
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| io::Error::other("no Sec-WebSocket-Key"))?;
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

/// A message from the peer.
#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    /// A close frame: status code and reason, when given.
    Close(Option<u16>, String),
}

/// Frames read from a stream, reassembling fragmented messages. `read`
/// returns `Ok(None)` when the stream's read timeout passes without a
/// complete frame, so a caller can poll a flag between messages.
pub struct Reader {
    buf: Vec<u8>,
    /// A message under reassembly: opcode and payload so far.
    partial: Option<(u8, Vec<u8>)>,
}

impl Default for Reader {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader {
    pub fn new() -> Self {
        Reader { buf: Vec::new(), partial: None }
    }

    /// The next message, `Ok(None)` on a read timeout, `Err` when the
    /// stream ended or a frame was malformed.
    pub fn read(&mut self, mut stream: &TcpStream) -> io::Result<Option<Message>> {
        loop {
            if let Some((frame, used)) = parse_frame(&self.buf)? {
                self.buf.drain(..used);
                match self.take(frame)? {
                    Some(m) => return Ok(Some(m)),
                    None => continue,
                }
            }
            let mut chunk = [0u8; 8192];
            match stream.read(&mut chunk) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed")),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if self.buf.len() > MAX_MESSAGE + 16 {
                        return Err(io::Error::other("message too large"));
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Fold a frame into the message under reassembly; the message once
    /// complete.
    fn take(&mut self, frame: Frame) -> io::Result<Option<Message>> {
        match frame.opcode {
            0x8 => {
                let code = (frame.payload.len() >= 2).then(|| u16::from_be_bytes([frame.payload[0], frame.payload[1]]));
                let reason = if frame.payload.len() > 2 {
                    String::from_utf8_lossy(&frame.payload[2..]).into_owned()
                } else {
                    String::new()
                };
                return Ok(Some(Message::Close(code, reason)));
            }
            0x9 => return Ok(Some(Message::Ping(frame.payload))),
            0xA => return Ok(Some(Message::Pong(frame.payload))),
            _ => {}
        }
        let (opcode, mut payload) = match (frame.opcode, self.partial.take()) {
            (0x0, Some((op, mut sofar))) => {
                sofar.extend_from_slice(&frame.payload);
                (op, sofar)
            }
            (0x0, None) => return Err(io::Error::other("continuation without a start")),
            (op @ (0x1 | 0x2), None) => (op, frame.payload),
            (_, Some(_)) => return Err(io::Error::other("new message inside a fragmented one")),
            (op, None) => return Err(io::Error::other(format!("unknown opcode {op}"))),
        };
        if payload.len() > MAX_MESSAGE {
            return Err(io::Error::other("message too large"));
        }
        if !frame.fin {
            self.partial = Some((opcode, payload));
            return Ok(None);
        }
        payload.shrink_to_fit();
        Ok(Some(if opcode == 0x1 {
            Message::Text(String::from_utf8(payload).map_err(|_| io::Error::other("text frame is not UTF-8"))?)
        } else {
            Message::Binary(payload)
        }))
    }
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// Parse one frame from the front of `buf`: the frame and the bytes it
/// took, or `None` if more bytes are needed.
fn parse_frame(buf: &[u8]) -> io::Result<Option<(Frame, usize)>> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let fin = buf[0] & 0x80 != 0;
    if buf[0] & 0x70 != 0 {
        return Err(io::Error::other("reserved bits set (no extensions negotiated)"));
    }
    let opcode = buf[0] & 0x0F;
    let masked = buf[1] & 0x80 != 0;
    if !masked {
        return Err(io::Error::other("client frames must be masked"));
    }
    let mut len = usize::from(buf[1] & 0x7F);
    let mut at = 2;
    if len == 126 {
        if buf.len() < 4 {
            return Ok(None);
        }
        len = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
        at = 4;
    } else if len == 127 {
        if buf.len() < 10 {
            return Ok(None);
        }
        let n = u64::from_be_bytes(buf[2..10].try_into().unwrap());
        if n > MAX_MESSAGE as u64 {
            return Err(io::Error::other("frame too large"));
        }
        len = n as usize;
        at = 10;
    }
    if opcode >= 0x8 && (len > 125 || !fin) {
        return Err(io::Error::other("control frame too long or fragmented"));
    }
    if buf.len() < at + 4 + len {
        return Ok(None);
    }
    let mask = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
    at += 4;
    let payload: Vec<u8> = buf[at..at + len].iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
    Ok(Some((Frame { fin, opcode, payload }, at + len)))
}

/// An unmasked server frame.
fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n < 65536 => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

pub fn write_text(mut stream: &TcpStream, text: &str) -> io::Result<()> {
    stream.write_all(&frame(0x1, text.as_bytes()))?;
    stream.flush()
}

pub fn write_pong(mut stream: &TcpStream, data: &[u8]) -> io::Result<()> {
    stream.write_all(&frame(0xA, data))?;
    stream.flush()
}

pub fn write_ping(mut stream: &TcpStream, data: &[u8]) -> io::Result<()> {
    stream.write_all(&frame(0x9, data))?;
    stream.flush()
}

pub fn write_close(mut stream: &TcpStream, code: u16, reason: &str) -> io::Result<()> {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason.as_bytes());
    payload.truncate(125);
    stream.write_all(&frame(0x8, &payload))?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_known_vectors() {
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn accept_key_is_the_rfc_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    /// A client (masked) frame.
    fn masked(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0x12, 0x34, 0x56, 0x78];
        let mut out = vec![(if fin { 0x80 } else { 0 }) | opcode];
        match payload.len() {
            n if n < 126 => out.push(0x80 | n as u8),
            n if n < 65536 => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        out
    }

    #[test]
    fn frames_round_trip() {
        let mut r = Reader::new();
        r.buf = masked(true, 0x1, b"hello");
        let (f, used) = parse_frame(&r.buf).unwrap().unwrap();
        assert_eq!(used, r.buf.len());
        assert_eq!(r.take(f).unwrap(), Some(Message::Text("hello".into())));

        let long = vec![b'x'; 70_000];
        let buf = masked(true, 0x2, &long);
        let (f, used) = parse_frame(&buf).unwrap().unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(f.payload.len(), 70_000);

        // Fragmented text, with a ping in between.
        let mut r = Reader::new();
        let (f, _) = parse_frame(&masked(false, 0x1, b"ab")).unwrap().unwrap();
        assert_eq!(r.take(f).unwrap(), None);
        let (f, _) = parse_frame(&masked(true, 0x9, b"p")).unwrap().unwrap();
        assert_eq!(r.take(f).unwrap(), Some(Message::Ping(b"p".to_vec())));
        let (f, _) = parse_frame(&masked(true, 0x0, b"cd")).unwrap().unwrap();
        assert_eq!(r.take(f).unwrap(), Some(Message::Text("abcd".into())));

        let (f, _) = parse_frame(&masked(true, 0x8, &[0x03, 0xE8, b'b', b'y', b'e'])).unwrap().unwrap();
        assert_eq!(r.take(f).unwrap(), Some(Message::Close(Some(1000), "bye".into())));

        assert!(parse_frame(&[0x81]).unwrap().is_none(), "need the length byte");
        assert!(parse_frame(&[0x81, 0x85, 1, 2]).unwrap().is_none(), "need the mask and payload");
        assert!(parse_frame(&[0x81, 0x05, 1, 2, 3, 4, 5, 6, 7, 8, 9]).is_err(), "unmasked");
        assert_eq!(frame(0x1, b"hi"), vec![0x81, 2, b'h', b'i']);
    }
}
