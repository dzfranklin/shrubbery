use crate::frame::Frame;
use tokio_util::codec::{Decoder, Encoder, LinesCodec, LinesCodecError};

const MAX_LENGTH: usize = 1024 * 1024 * 16;

pub struct ShrubCodec(LinesCodec);

impl ShrubCodec {
    pub fn new() -> Self {
        Self(LinesCodec::new_with_max_length(MAX_LENGTH))
    }
}

impl Decoder for ShrubCodec {
    type Item = Frame;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let Some(line) = self.0.decode(src).map_err(map_lines_err)? else {
            return Ok(None);
        };
        serde_json::from_str(&line)
            .map(Some)
            .map_err(map_serde_json_err)
    }
}

impl Encoder<Frame> for ShrubCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Frame, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        let line = serde_json::to_string(&item).map_err(map_serde_json_err)?;
        self.0.encode(line, dst).map_err(map_lines_err)
    }
}

fn map_lines_err(err: LinesCodecError) -> std::io::Error {
    match err {
        LinesCodecError::MaxLineLengthExceeded => {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "max line length exceeded")
        }
        LinesCodecError::Io(err) => err,
    }
}

fn map_serde_json_err(err: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, err)
}
