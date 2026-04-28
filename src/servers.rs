//! TCP server implementations for protocol, stderr, and health endpoints.

#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) mod health;

#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) mod protocol;

#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) mod stderr;
