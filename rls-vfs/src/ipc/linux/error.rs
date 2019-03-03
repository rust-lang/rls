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
        write!(f, "error from libc funciton {} with errno {}", self.func, self.errno)
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

/*
pub struct TokenNotFound;
impl TokenNotFound {
    pub fn new() -> Self {
        return RlsVfsIpcError::TokenNotFound;
    }

    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "token not found in LinuxVfsIpcServer's connection_infos when polling from mio")
    }
}

impl std::fmt::Display for TokenNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        TokenNotFound::fmt(&self, f)
    }
}

impl std::fmt::Debug for TokenNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        TokenNotFound::fmt(&self, f)
    }
}

impl std::error::Error for TokenNotFound {
    fn source(&self) -> Option<&'static dyn std::error::Error> {
        None
    }
}

impl From<TokenNotFound> for RlsVfsIpcError {
    fn from(err: TokenNotFound) -> RlsVfsIpcError {
        RlsVfsIpcError::TokenNotFound(err)
    }
}

pub struct PipeCloseMiddle;

impl PipeCloseMiddle {
    pub fn new() -> Self {
        PipeCloseMiddle
    }

    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "pipe closed while still reading/writing")
    }
}

impl std::fmt::Display for PipeCloseMiddle {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        PipeCloseMiddle::fmt(&self, f)
    }
}

impl std::fmt::Debug for PipeCloseMiddle {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        PipeCloseMiddle::fmt(&self, f)
    }
}

impl std::error::Error for PipeCloseMiddle {
    fn source(&self) -> Option<&'static dyn std::error::Error> {
        None
    }
}

impl From<PipeCloseMiddle> for RlsVfsIpcError {
    fn from(err: PipeCloseMiddle) -> RlsVfsIpcError {
        RlsVfsIpcError::PipeCloseMiddle(err)
    }
}

pub struct SerializeError;

impl SerializeError {
    pub fn new() -> Self {
        SerializeError
    }

    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "error when serializing something")
    }
}

impl std::fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        SerializeError::fmt(&self, f)
    }
}

impl std::fmt::Debug for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        SerializeError::fmt(&self, f)
    }
}

impl std::error::Error for SerializeError {
    fn source(&self) -> Option<&'static dyn std::error::Error> {
        None
    }
}

impl From<SerializeError> for RlsVfsIpcError {
    fn from(err: SerializeError) -> RlsVfsIpcError {
        RlsVfsIpcError::SerializeError(err)
    }
}

pub struct DeserializeError;

impl DeserializeError {
    pub fn new() -> Self {
        DeserializeError
    }

    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "error when deserializing something")
    }
}

impl std::fmt::Display for DeserializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        DeserializeError::fmt(&self, f)
    }
}

impl std::fmt::Debug for DeserializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        DeserializeError::fmt(&self, f)
    }
}

impl std::error::Error for DeserializeError {
    fn source(&self) -> Option<&'static dyn std::error::Error> {
        None
    }
}

impl From<DeserializeError> for RlsVfsIpcError {
    fn from(err: DeserializeError) -> RlsVfsIpcError {
        RlsVfsIpcError::DeserializeError(err)
    }
}
*/
