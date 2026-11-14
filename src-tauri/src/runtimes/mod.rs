//! Getting external components onto the machine: HTTP, verified downloads.

pub mod download;
pub mod http;

#[cfg(test)]
pub(crate) mod test_server;
