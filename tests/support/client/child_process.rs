use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::rc::Rc;

use futures::Poll;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_process::{Child, CommandExt};

pub struct ChildProcess {
    stdin: tokio_process::ChildStdin,
    stdout: tokio_process::ChildStdout,
    child: Rc<tokio_process::Child>,
}

impl Read for ChildProcess {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        Read::read(&mut self.stdout, buf)
    }
}

impl Write for ChildProcess {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Write::write(&mut self.stdin, buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Write::flush(&mut self.stdin)
    }
}

impl AsyncRead for ChildProcess {}
impl AsyncWrite for ChildProcess {
    fn shutdown(&mut self) -> Poll<(), std::io::Error> {
        AsyncWrite::shutdown(&mut self.stdin)
    }
}

impl ChildProcess {
    pub fn spawn_from_command(mut cmd: Command) -> Result<ChildProcess, std::io::Error> {
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        let mut child = cmd.spawn_async()?;

        Ok(ChildProcess {
            stdout: child.stdout().take().unwrap(),
            stdin: child.stdin().take().unwrap(),
            child: Rc::new(child),
        })
    }

    /// Returns a handle to the underlying `Child` process.
    /// Useful when waiting until child process exits.
    pub fn child(&self) -> Rc<Child> {
        Rc::clone(&self.child)
    }
}
