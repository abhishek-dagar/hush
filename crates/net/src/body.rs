//! Reading a body: from the network or a file, decompressed as it streams,
//! capped on decoded bytes, and sent to the page in coalesced chunks (spec
//! 0004, child 0001, Compression and Chunks; AC-4, AC-5).

use std::future::poll_fn;
use std::io::{self, Write};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use brotli_decompressor::DecompressorWriter;
use flate2::write::{DeflateDecoder, GzDecoder, ZlibDecoder};
use hyper::body::{Body as _, Incoming};
use ipc::FetchError;
use tokio::io::{AsyncRead, ReadBuf};
use tracing::warn;

use crate::fetch::Load;
use crate::limits::{CHUNK_COALESCE, IDLE_TIMEOUT};

/// How a body is compressed, from its `Content-Encoding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Identity,
    Gzip,
    Deflate,
    Brotli,
}

impl Encoding {
    /// The encoding a `Content-Encoding` value names. Only what the request
    /// offered is decoded; anything else is passed through as it is.
    pub(crate) fn from_header(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("" | "identity") => Self::Identity,
            Some("gzip" | "x-gzip") => Self::Gzip,
            Some("deflate") => Self::Deflate,
            Some("br") => Self::Brotli,
            Some(other) => {
                warn!(encoding = other, "unknown content encoding passed through");
                Self::Identity
            }
        }
    }
}

/// Where the raw bytes come from.
enum Source {
    Http(Incoming),
    File(tokio::fs::File, Box<[u8]>),
}

/// A body being read: raw bytes in, decoded and size checked bytes out.
pub(crate) struct Body {
    source: Source,
    decoder: Decoder,
    ended: bool,
}

impl Body {
    /// A response body, decoded per `encoding`, at most `limit` decoded bytes.
    pub(crate) fn http(body: Incoming, encoding: Encoding, limit: usize) -> Self {
        Self {
            source: Source::Http(body),
            decoder: Decoder::new(encoding, limit),
            ended: false,
        }
    }

    /// A file's bytes, at most `limit` of them.
    pub(crate) fn file(file: tokio::fs::File, limit: usize) -> Self {
        Self {
            source: Source::File(file, vec![0; CHUNK_COALESCE].into_boxed_slice()),
            decoder: Decoder::new(Encoding::Identity, limit),
            ended: false,
        }
    }

    /// The next decoded bytes, or `None` at the end. Fails with `TimedOut`
    /// when nothing arrives for [`IDLE_TIMEOUT`].
    pub(crate) async fn next(&mut self) -> Result<Option<Vec<u8>>, FetchError> {
        loop {
            if self.ended {
                return Ok(None);
            }
            let raw = tokio::time::timeout(IDLE_TIMEOUT, poll_fn(|cx| self.poll_raw(cx)))
                .await
                .map_err(|_| FetchError::TimedOut)??;
            if let Some(decoded) = self.decode(raw)? {
                return Ok(Some(decoded));
            }
        }
    }

    /// The next decoded bytes if some are ready without waiting. `None`
    /// means nothing is ready now; `Some(Ok(None))` means the body ended.
    pub(crate) fn ready(&mut self) -> Option<Result<Option<Vec<u8>>, FetchError>> {
        if self.ended {
            return Some(Ok(None));
        }
        // Polling with a waker that does nothing is fine: when this says
        // nothing is ready, the caller awaits `next`, which polls again
        // with the task's real waker.
        let mut cx = Context::from_waker(Waker::noop());
        match self.poll_raw(&mut cx) {
            Poll::Pending => None,
            Poll::Ready(Err(err)) => Some(Err(err)),
            Poll::Ready(Ok(raw)) => Some(self.decode(raw).map(|d| Some(d.unwrap_or_default()))),
        }
    }

    /// Reads until at least `wanted` decoded bytes are in, or the body
    /// ended, and gives them all.
    pub(crate) async fn read_at_least(&mut self, wanted: usize) -> Result<Vec<u8>, FetchError> {
        let mut out = Vec::new();
        while out.len() < wanted {
            match self.next().await? {
                Some(bytes) => out.extend_from_slice(&bytes),
                None => break,
            }
        }
        Ok(out)
    }

    /// Feeds raw bytes (`None` at the end of the source) to the decoder.
    /// `None` means they decoded to nothing yet.
    fn decode(&mut self, raw: Option<Vec<u8>>) -> Result<Option<Vec<u8>>, FetchError> {
        let decoded = match raw {
            Some(raw) => self.decoder.push(&raw)?,
            None => {
                self.ended = true;
                self.decoder.finish()?
            }
        };
        Ok((!decoded.is_empty() || self.ended).then_some(decoded))
    }

    /// The next raw bytes from the source, `None` at its end.
    fn poll_raw(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Vec<u8>>, FetchError>> {
        match &mut self.source {
            Source::Http(body) => loop {
                match Pin::new(&mut *body).poll_frame(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(Ok(None)),
                    Poll::Ready(Some(Err(err))) => {
                        warn!(%err, "body cut short");
                        return Poll::Ready(Err(FetchError::ConnectFailed));
                    }
                    Poll::Ready(Some(Ok(frame))) => {
                        // Trailers carry no body bytes.
                        if let Ok(data) = frame.into_data() {
                            return Poll::Ready(Ok(Some(data.to_vec())));
                        }
                    }
                }
            },
            Source::File(file, buffer) => {
                let mut read = ReadBuf::new(buffer);
                match Pin::new(file).poll_read(cx, &mut read) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(Err(err)) => {
                        warn!(%err, "file read failed");
                        Poll::Ready(Err(FetchError::FileNotFound))
                    }
                    Poll::Ready(Ok(())) if read.filled().is_empty() => Poll::Ready(Ok(None)),
                    Poll::Ready(Ok(())) => Poll::Ready(Ok(Some(read.filled().to_vec()))),
                }
            }
        }
    }
}

/// Streams `body` to the page after its commit or response: `first` (bytes
/// already read, for sniffing) and then the rest, coalesced up to
/// [`CHUNK_COALESCE`] while more is ready at once. Ends with `FetchDone`,
/// or with `FetchFailed` and the reason, which is also returned.
pub(crate) async fn stream(load: &Load, mut body: Body, first: Vec<u8>) -> Result<(), FetchError> {
    let mut pending = first;
    loop {
        if pending.len() >= CHUNK_COALESCE {
            let rest = pending.split_off(CHUNK_COALESCE);
            load.chunk(&pending);
            pending = rest;
            continue;
        }
        let next = if pending.is_empty() {
            body.next().await
        } else {
            match body.ready() {
                Some(next) => next,
                // Nothing more right now: send what there is, then wait.
                None => {
                    load.chunk(&std::mem::take(&mut pending));
                    continue;
                }
            }
        };
        match next {
            Ok(Some(bytes)) => pending.extend_from_slice(&bytes),
            Ok(None) => {
                load.chunk(&pending);
                load.done();
                return Ok(());
            }
            Err(reason) => {
                load.chunk(&pending);
                load.failed(reason.clone());
                return Err(reason);
            }
        }
    }
}

/// Decodes one body, holding the size limit on what comes out.
enum Decoder {
    Identity(Capped),
    Gzip(Box<GzDecoder<Capped>>),
    /// `deflate` before its first two bytes say whether it has the zlib
    /// wrapper the standard asks for or is raw, as some servers send it.
    DeflateUnknown(Capped, Vec<u8>),
    Zlib(Box<ZlibDecoder<Capped>>),
    RawDeflate(Box<DeflateDecoder<Capped>>),
    Brotli(Box<DecompressorWriter<Capped>>),
}

/// The brotli decoder's output buffer.
const BROTLI_BUFFER: usize = 32 * 1024;

impl Decoder {
    fn new(encoding: Encoding, limit: usize) -> Self {
        let out = Capped {
            out: Vec::new(),
            room: limit,
        };
        match encoding {
            Encoding::Identity => Self::Identity(out),
            Encoding::Gzip => Self::Gzip(Box::new(GzDecoder::new(out))),
            Encoding::Deflate => Self::DeflateUnknown(out, Vec::new()),
            Encoding::Brotli => Self::Brotli(Box::new(DecompressorWriter::new(out, BROTLI_BUFFER))),
        }
    }

    /// Decodes `input` and gives what came out.
    fn push(&mut self, input: &[u8]) -> Result<Vec<u8>, FetchError> {
        let result = match self {
            Self::Identity(out) => out.write_all(input),
            Self::Gzip(decoder) => decoder.write_all(input),
            Self::Zlib(decoder) => decoder.write_all(input),
            Self::RawDeflate(decoder) => decoder.write_all(input),
            Self::Brotli(decoder) => decoder.write_all(input),
            Self::DeflateUnknown(_, held) => {
                held.extend_from_slice(input);
                if held.len() < 2 {
                    return Ok(Vec::new());
                }
                return self.settle_deflate();
            }
        };
        result.map_err(decode_error)?;
        Ok(self.take())
    }

    /// The end of the input: flushes what the decoder still holds.
    fn finish(&mut self) -> Result<Vec<u8>, FetchError> {
        let result = match self {
            Self::Identity(_) => Ok(()),
            Self::Gzip(decoder) => decoder.try_finish(),
            Self::Zlib(decoder) => decoder.try_finish(),
            Self::RawDeflate(decoder) => decoder.try_finish(),
            Self::Brotli(decoder) => decoder.close(),
            Self::DeflateUnknown(..) => {
                let mut out = self.settle_deflate()?;
                out.extend(self.finish()?);
                return Ok(out);
            }
        };
        result.map_err(decode_error)?;
        Ok(self.take())
    }

    /// Picks zlib or raw deflate from the held bytes and decodes them.
    fn settle_deflate(&mut self) -> Result<Vec<u8>, FetchError> {
        let Self::DeflateUnknown(out, held) = self else {
            return Ok(Vec::new());
        };
        let out = std::mem::replace(
            out,
            Capped {
                out: Vec::new(),
                room: 0,
            },
        );
        let held = std::mem::take(held);
        // A zlib header: compression method 8, and the two bytes as a big
        // endian number divisible by 31 (RFC 1950).
        let zlib = match held.as_slice() {
            [cmf, flg, ..] => cmf & 0x0F == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0,
            _ => true,
        };
        *self = if zlib {
            Self::Zlib(Box::new(ZlibDecoder::new(out)))
        } else {
            Self::RawDeflate(Box::new(DeflateDecoder::new(out)))
        };
        if held.is_empty() {
            Ok(Vec::new())
        } else {
            self.push(&held)
        }
    }

    /// Takes the decoded bytes so far.
    fn take(&mut self) -> Vec<u8> {
        let out = match self {
            Self::Identity(out) | Self::DeflateUnknown(out, _) => out,
            Self::Gzip(decoder) => decoder.get_mut(),
            Self::Zlib(decoder) => decoder.get_mut(),
            Self::RawDeflate(decoder) => decoder.get_mut(),
            Self::Brotli(decoder) => decoder.get_mut(),
        };
        std::mem::take(&mut out.out)
    }
}

/// A decoder failed: over the limit, or corrupt data, which ends the body
/// as if the connection had dropped.
fn decode_error(err: io::Error) -> FetchError {
    if err.kind() == io::ErrorKind::FileTooLarge {
        FetchError::TooLarge
    } else {
        warn!(%err, "body failed to decode");
        FetchError::ConnectFailed
    }
}

/// Where decoders write: refuses to grow past the size limit, so a
/// compression bomb stops at the cap instead of filling memory (AC-4).
struct Capped {
    out: Vec<u8>,
    /// How many more decoded bytes are allowed.
    room: usize,
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.room {
            return Err(io::Error::from(io::ErrorKind::FileTooLarge));
        }
        self.room -= buf.len();
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};

    use super::*;

    fn decode_all(
        encoding: Encoding,
        limit: usize,
        input: &[u8],
        step: usize,
    ) -> Result<Vec<u8>, FetchError> {
        let mut decoder = Decoder::new(encoding, limit);
        let mut out = Vec::new();
        for piece in input.chunks(step.max(1)) {
            out.extend(decoder.push(piece)?);
        }
        out.extend(decoder.finish()?);
        Ok(out)
    }

    #[test]
    fn every_offered_encoding_decodes_in_any_split() {
        let text = "<p>compressed body</p>".repeat(500);
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(text.as_bytes()).unwrap();
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
        zlib.write_all(text.as_bytes()).unwrap();
        let mut raw = DeflateEncoder::new(Vec::new(), Compression::default());
        raw.write_all(text.as_bytes()).unwrap();
        let cases = [
            (Encoding::Identity, text.as_bytes().to_vec()),
            (Encoding::Gzip, gzip.finish().unwrap()),
            (Encoding::Deflate, zlib.finish().unwrap()),
            (Encoding::Deflate, raw.finish().unwrap()),
        ];
        for step in [1, 7, 4096] {
            for (encoding, bytes) in &cases {
                let out = decode_all(*encoding, usize::MAX, bytes, step).unwrap();
                assert_eq!(out, text.as_bytes(), "{encoding:?} in steps of {step}");
            }
        }
    }

    #[test]
    fn the_limit_counts_decoded_bytes() {
        // AC-4: a small gzip body that inflates past the cap stops at it.
        let mut bomb = GzEncoder::new(Vec::new(), Compression::best());
        bomb.write_all(&vec![0; 8 * 1024 * 1024]).unwrap();
        let bomb = bomb.finish().unwrap();
        assert!(bomb.len() < 64 * 1024);
        assert_eq!(
            decode_all(Encoding::Gzip, 1024 * 1024, &bomb, 4096),
            Err(FetchError::TooLarge)
        );
        assert_eq!(
            decode_all(Encoding::Identity, 10, b"eleven byte", 100),
            Err(FetchError::TooLarge)
        );
        assert_eq!(
            decode_all(Encoding::Identity, 11, b"eleven byte", 100),
            Ok(b"eleven byte".to_vec())
        );
    }

    #[test]
    fn corrupt_data_ends_the_body() {
        assert_eq!(
            decode_all(Encoding::Gzip, usize::MAX, b"not gzip at all", 100),
            Err(FetchError::ConnectFailed)
        );
    }

    #[test]
    fn content_encoding_names() {
        assert_eq!(Encoding::from_header(None), Encoding::Identity);
        assert_eq!(Encoding::from_header(Some(" GZIP ")), Encoding::Gzip);
        assert_eq!(Encoding::from_header(Some("br")), Encoding::Brotli);
        assert_eq!(Encoding::from_header(Some("deflate")), Encoding::Deflate);
        assert_eq!(Encoding::from_header(Some("zstd")), Encoding::Identity);
    }
}
