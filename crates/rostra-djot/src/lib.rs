//! Shared djot utilities for Rostra.
//!
//! This crate provides common djot parsing utilities used across multiple
//! Rostra crates, including link extraction and mention detection.

pub mod links;
pub mod mention;

#[cfg(test)]
mod tests;
