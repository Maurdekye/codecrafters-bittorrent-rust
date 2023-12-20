use std::io::{self, Read};

use crate::download::corkboard::PieceLocation;

pub struct DataProxy {
    pub pieces: Vec<PieceLocation>,
    pub piece_length: usize,
    pub length: usize,
    pub cursor: usize,
}

impl DataProxy {
    pub fn new(pieces: Vec<PieceLocation>, piece_length: usize, length: usize) -> Self {
        Self {
            pieces,
            piece_length,
            length,
            cursor: 0,
        }
    }
}

impl Read for DataProxy {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0;
        while self.cursor < self.length && read < buf.len() {
            let current_piece = self.cursor / self.piece_length;
            let piece_offset = current_piece * self.piece_length;
            let offset_in_piece = self.cursor % self.piece_length;
            let piece_length = (self.length - piece_offset).min(self.piece_length);
            let to_read = (piece_length - offset_in_piece).min(buf.len() - read);
            let piece = self.pieces.get(current_piece).ok_or(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("Piece index out of range: {current_piece}"),
            ))?;
            let piece_location = piece.clone();
            let piece_data = piece_location.load().map_err(|err| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Piece not found: {current_piece}: {err}"),
                )
            })?;
            buf[read..read + to_read]
                .copy_from_slice(&piece_data[offset_in_piece..offset_in_piece + to_read]);
            read += to_read;
            self.cursor += to_read;
        }

        Ok(read)
    }
}
