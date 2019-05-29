// mio and std::io return std::io::Result, bincode returns bincode::Error, other parts of rls-vfs returns Result<_,
// rls_vfs::Error>, libc use errno (which means we need each a errno class for each (libc_function,
// errno) pair)

use quick_error::quick_error;

use std::convert::From;
use std::error::Error;

use super::super::super::Error as RlsVfsError;
use bincode::Error as BinCodeError;
use std::io::Error as StdIoError;


// a simplified Error class for libc
pub struct LibcError {
    func: &'static str,
    errno: libc::c_int,
}

impl LibcError {
    pub fn new(func: &'static str, errno: libc::c_int) -> Self {
        LibcError {
            func,
            errno,
        }
    }

    pub fn is_would_block(&self) -> bool {
        // I'm not sure whether EWOULDBLOCK and EAGAIN are guaranteed to be the same, so let's stay
        // on the safe side and hope that compiler doesn't warn about dead code
        if self.errno == libc::EWOULDBLOCK {
            return true;
        }
        if self.errno == libc::EAGAIN {
            return true;
        }
        return false;
    }

    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "error from libc function {} with errno {}", self.func, self.errno)
    }
}

impl std::fmt::Display for LibcError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        LibcError::fmt(&self, f)
    }
}

impl std::fmt::Debug for LibcError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        LibcError::fmt(&self, f)
    }
}

impl Error for LibcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
	// TODO: hierarchical error handling
	None
    }
}

quick_error! {
    #[derive(Debug)]
    pub enum RlsVfsIpcError {
        LibcError(err: LibcError) {
            from()
        }
        StdIoError(err: StdIoError) {
            from()
        }
        RlsVfsError(err: RlsVfsError) {
            from()
        }
        SerializeError(err: BinCodeError) {
        }
        DeserializeError(err: BinCodeError) {
        }
        CloseNonOpenedFile {
        }
        TokenNotFound {
        }
        PipeCloseMiddle {
        }
        RemoveUnknownClient {
        }
        GetFileFromClosedHandle {
        }
        InternalError {
        }
        Other {
        }
    }
}
/*
impl RlsVfsIpcError {
    fn fmt(&self, f:&mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::LibcError(err) => {
                write!(f, "LibcError({})", err)
            },
            Self::StdIoError(err) => {
                write!(f, "StdioError({})", err)
            },
            Self::BinCodeError(err) => {
                write!(f, "BinCodeError({})", err)
            },
            Self::RlsVfsError(err) => {
                write!(f, "RlsVfsError({})", err)
            },
            Self::TokenNotFound => {
                write!(f, "TokenNotFound")
            },
            Self::PipeCloseMiddle(err) => {
                write!(f, "PipeCloseMiddle")
            },
            Self::SerializeError(err) => {
                write!(f, "Serialize")
            },
            Self::DeserializeError(err) => {
                write!(f, "Deserialize")
            },
            Self::Other => {
                write!(f, "Other");
            },
        }
    }
}

impl Error for RlsVfsIpcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
	// TODO: hierarchical error handling
	None
    }
}

impl std::fmt::Display for RlsVfsIpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        RlsVfsIpcError::fmt(&self, f)
    }
}

impl std::fmt::Debug for RlsVfsIpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        RlsVfsIpcError::fmt(&self, f)
    }
}

// TODO: write some macros to simplify code
impl From<LibcError> for RlsVfsIpcError {
    fn from(e: LibcError) -> Self {
        Self::LibcError(e)
    }
}

impl From<std::io::Error> for RlsVfsIpcError {
    fn from(e: std::io::Error) -> Self {
        Self::StdIoError(e)
    }
}

impl From<RlsVfsError> for RlsVfsIpcError {
    fn from(e: RlsVfsError) -> Self {
        Self::RlsVfsError(e)
    }
}

impl std::default::Default for RlsVfsError {
    fn default() -> Self {
        RlsVfsError::Other
    }
}
*/
macro_rules! handle_libc_error {
    ($name:expr) => {
        let err = std::io::Error::last_os_error();
        let err_code = err.raw_os_error().unwrap();
        return std::result::Result::Err(std::convert::From::from(LibcError::new($name, err_code)));
    }
}

macro_rules! would_block_or_error {
    ($name:expr) => {
        {
            let err = std::io::Error::last_os_error();
            let err_code = err.raw_os_error().unwrap();
            if err_code == libc::EWOULDBLOCK {
                true
            } else if err_code == libc::EAGAIN {
                true
            } else {
                return std::result::Result::Err(std::convert::From::from(LibcError::new($name, err_code)));
            }
        }
    }
}

macro_rules! fake_libc_error {
    ($name:expr, $errno:expr) => {
            return std::result::Result::Err(std::convert::From::from(LibcError::new($name, $errno)));
    }
}

