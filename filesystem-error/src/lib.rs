use std::{io, result};
use thiserror::Error;

pub type Result<T> = result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] io::Error),

    #[error(transparent)]
    Errno(#[from] nix::errno::Errno),

    #[error("internal error")]
    Internal,
}

pub mod prelude {
    pub use crate::Error;
    pub use crate::Result;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwrapping() {
        fn produce() -> Result<()> {
            Err(Error::IO(io::Error::from_raw_os_error(38)))
        }

        if let Err(err) = produce() {
            match err {
                Error::IO(ioerr) => {
                    assert!(ioerr.raw_os_error().is_some());
                }
                _ => {
                    unreachable!()
                }
            }
        }
    }
}
