//! Argument parsing and startup wiring for Navarch.

#![forbid(unsafe_code)]

pub mod home;

pub mod worker;

/// Hub CLI wiring.
pub mod hub;

mod terminal;

/// Remote pairing and relay configuration.
pub mod remote;

/// Per-user service generation and startup wiring.
pub mod service;
