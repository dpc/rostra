//! Restricted retained JPEG validation, not a general-purpose decoder.
//!
//! See the packaged README and provenance for the fixed subset and risk model.
#![forbid(unsafe_code)]
// Keep the private upstream implementation recognizable, including unreachable
// unsupported-format machinery. None of its generic APIs are exported.
#![allow(dead_code)]

mod preflight;
mod validate;
pub use validate::{Dimensions, Rejection, validate};
#[cfg(test)]
mod tests;

extern crate alloc;
extern crate core;

#[cfg(any())]
extern crate rayon;

use std::io;

use completion::Completion;

#[cfg(not(all()))]
mod arch;
#[allow(clippy::all)]
mod completion;
#[allow(clippy::all)]
mod decoder;
#[allow(clippy::all)]
mod error;
#[allow(clippy::all)]
mod huffman;
#[allow(clippy::all)]
mod idct;
#[allow(clippy::all)]
mod marker;
#[allow(clippy::all)]
mod parser;
#[allow(clippy::all)]
mod upsampler;
#[allow(clippy::all)]
mod worker;

fn read_u8<R: io::Read>(reader: &mut R) -> io::Result<u8> {
    let mut buf = [0];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u16_from_be<R: io::Read>(reader: &mut R) -> io::Result<u16> {
    let mut buf = [0, 0];
    reader.read_exact(&mut buf)?;
    Ok(u16::from_be_bytes(buf))
}
