//! Shared djot utilities for Rostra.
//!
//! This crate provides common djot parsing utilities used across multiple
//! Rostra crates, including link extraction and mention detection.

pub mod extract;
pub mod links;
pub mod mention;
pub mod native;
mod parse_guard;

#[cfg(test)]
mod tests;
