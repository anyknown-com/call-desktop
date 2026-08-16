//! Port of `pcm.ts`: streaming s16le bytes → f32 chunks.

use bytes::Bytes;
use futures::{stream, Stream, StreamExt};

use crate::{Error, PcmStream};

/// Signed 16-bit little-endian → f32 in `[-1, 1)`. `bytes.len()` must be even.
pub fn s16le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect()
}

/// Turn a stream of s16le byte chunks into float chunks. A sample split across two
/// chunks is carried over to the next one.
pub(crate) fn s16le_stream_to_pcm<S, E>(
    body: S,
    sample_rate: u32,
    provider: &'static str,
) -> PcmStream
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
    E: Into<reqwest::Error>,
{
    let chunks = stream::unfold(
        (Box::pin(body), Vec::new()),
        move |(mut body, mut carry)| async move {
            loop {
                match body.next().await {
                    None => return None,
                    Some(Err(e)) => {
                        return Some((Err(Error::transport(provider)(e.into())), (body, carry)))
                    }
                    Some(Ok(bytes)) => {
                        carry.extend_from_slice(&bytes);
                        let usable = carry.len() & !1;
                        if usable == 0 {
                            continue;
                        }
                        let out = s16le_to_f32(&carry[..usable]);
                        carry.drain(..usable);
                        return Some((Ok(out), (body, carry)));
                    }
                }
            }
        },
    );
    PcmStream {
        sample_rate,
        chunks: Box::pin(chunks),
    }
}

/// A `PcmStream` that yields all samples in one chunk.
pub fn single_chunk_pcm(samples: Vec<f32>, sample_rate: u32) -> PcmStream {
    PcmStream {
        sample_rate,
        chunks: Box::pin(stream::once(async move { Ok(samples) })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;

    #[test]
    fn converts_s16le() {
        let v = s16le_to_f32(&[0x00, 0x00, 0xff, 0x7f, 0x00, 0x80, 0x00, 0xc0]);
        assert_eq!(v, vec![0.0, 32767.0 / 32768.0, -1.0, -0.5]);
    }

    #[tokio::test]
    async fn carries_odd_trailing_byte_across_chunks() {
        // Sample 1 = 0x7fff, sample 2 = 0x8000 (-1), split so chunk 1 ends mid-sample.
        let chunks: Vec<std::result::Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::from_static(&[0xff, 0x7f, 0x00])),
            Ok(Bytes::from_static(&[])),
            Ok(Bytes::from_static(&[0x80, 0x00])),
            Ok(Bytes::from_static(&[0x40])),
        ];
        let pcm = s16le_stream_to_pcm(stream::iter(chunks), 24_000, "test");
        assert_eq!(pcm.sample_rate, 24_000);
        let out: Vec<Vec<f32>> = pcm.chunks.try_collect().await.unwrap();
        assert_eq!(out, vec![vec![32767.0 / 32768.0], vec![-1.0], vec![0.5]]);
    }
}
