use super::*;
use std::sync::Arc;
use std::rc::Rc;
use std::boxed::Box;
use std::pin::Pin;
use std::path::{Path, PathBuf};
use std::marker::PhantomData;
use std::collections::HashMap;
use mio::{Poll, Token};

pub struct LinuxVfsIpcChannel<U> {
    s2c_pipe: [libc::c_int;2],
    c2s_pipe: [libc::c_int;2],
    _u: PhantomData<U>
}

impl<U> VfsIpcChannel<U> for LinuxVfsIpcChannel<U> {
    type ServerEndPoint = LinuxVfsIpcServerEndPoint<U>;
    type ClientEndPoint = LinuxVfsIpcClientEndPoint<U>;

    fn new_prefork() -> Self {
        unsafe {
            let mut ret:Self = std::mem::uninitialized();
            libc::pipe2(&mut ret.s2c_pipe[0] as *mut libc::c_int, 0);
            libc::pipe2(&mut ret.c2s_pipe[0] as *mut libc::c_int, 0);
            ret
        }
    }

    fn into_server_end_point_postfork(self) -> Self::ServerEndPoint {
        unsafe {
            libc::close(self.c2s_pipe[1]);
            libc::close(self.s2c_pipe[0]);
        }
        Self::ServerEndPoint::new(self.c2s_pipe[0], self.s2c_pipe[1])
    }

    fn into_client_end_point_postfork(self) -> Self::ClientEndPoint {
        unsafe {
            libc::close(self.c2s_pipe[0]);
            libc::close(self.s2c_pipe[1]);
        }
        Self::ClientEndPoint::new(self.s2c_pipe[0], self.c2s_pipe[1])
    }
}

enum PipeReadState {
    None,
    Expecting(Vec<u8>),
}

enum PipeWriteState {
    None,
    Expecting(Vec<u8>)
}

struct ConnectionInfo<U> {
    server_end_point: LinuxVfsIpcServerEndPoint<U>,
    opened_files: HashMap<PathBuf, Rc<MapInfo>>,
    read_buf: PipeReadState,
    write_buf: PipeWriteState,
}

struct MapInfo {
    // NB: make sure mmap_path is null-terminated
    shm_name: String,
    length: libc::size_t,
}

fn generate_shm_name(file_path: &Path, version: &str) -> String {
    unimplemented!();
}

impl MapInfo {
    pub fn open(file_path: Rc<Path>, version: &str, cont: &str) -> Self {
        let shm_name = generate_shm_name(&file_path, version);
        let length = cont.len() as libc::size_t;
        unsafe {
            let shm_oflag = libc::O_CREAT | libc::O_EXCL | libc::O_RDWR;
            let shm_mode = libc::S_IRUSR | libc::S_IWUSR;
            let shm_fd = libc::shm_open(shm_name.as_ptr() as *const libc::c_char, shm_oflag, shm_mode);
            libc::ftruncate(shm_fd, length as libc::off_t);

            let mmap_prot = libc::PROT_READ | libc::PROT_WRITE;
            // shared map to save us a few memory pages
            // only the server write to the mapped area, the clients only read them, so no problem here
            let mmap_flags = libc::MAP_SHARED;
            let mmap_addr = libc::mmap(0 as *mut libc::c_void, length, mmap_prot, mmap_flags, shm_fd, 0);
            std::ptr::copy_nonoverlapping(cont.as_ptr() as *const u8, mmap_addr as *mut u8, length);
            libc::munmap(mmap_addr, length as libc::size_t);

            libc::close(shm_fd);
        }

        Self {
            file_path,
            shm_name,
            length,
        }
    }

    pub fn close(&self) {
        unsafe {
            libc::shm_unlink(self.shm_name.as_ptr() as *const libc::c_char);
        }
    }
}

pub struct LinuxVfsIpcServer<U> {
    connection_infos: HashMap<Token, ConnectionInfo<U>>,
    live_maps: HashMap<PathBuf, Rc<MapInfo>>,
    poll: Poll,
    vfs: Arc<Vfs>,
    _u: PhantomData<U>
}

#[derive(Debug, Display)]
struct TokenNotFound;

impl std::error::Error for TokenNotFound {
    fn source(&self) -> Option<&dyn std::error::Error> {
        None
    }
}

#[derive(Debug, Display)]
struct PipeBroken;

impl std::error::Error for PipeBroken {
    fn source(&self) -> Option<&dyn std::error::Error> {
        None
    }
}

#[derive(Debug, Display)]
struct PipeCloseMiddle;

impl std::error::Error for PipeCloseMiddle {
    fn source(&self) -> Option<&dyn std::error::Error> {
        None
    }
}

impl<U> VfsIpcServer<U> {
    fn handle_request(&mut self, tok: Token, req: VfsRequestMsg) {
        match self.connection_infos.get_mut(&tok) {
            Some(ref mut ci) => {
                match req {
                    VfsRequestMsg::OpenFile(path) => {
                    },
                    VfsRequestMsg::CloseFile(path) => {
                    },
                }
            },
            None => {
            },
        }
    }

    fn handle_read(&mut self, token: Token, ci: &mut ConnectionInfo) -> std::io::Result<()> {
        // TODO: more efficient buf read, less copy
        if PipeReadState::None == ci.read_buf {
            ci.read_buf = PipeReadState::Expecting(Vec::new());
        }
        if let PipeReadState::Expecting(ref mut buf) = ci.read_buf {
            let buf1 = [u8;4096];
            bool mut should_finish = false;
            loop {
                let res = unsafe {
                    libc::read(ci.server_endpoint.read_fd, &buf1[0] as *mut c_void, std::mem::size_of_val(&buf1))
                };
                if res > 0 {
                    buf.extend_from_slice(&buf1[..res]);
                } {
                    match res {
                        0 => {
                            should_finish = true;
                            break;
                        },
                        _ => {
                            match std::io::error::Error::last_os_error() {
                                libc::EWOULDBLOCK | libc::AGAIN => {
                                    break;
                                },
                                _ => {
                                    return Error(PipeBroken);
                                }
                            }
                        }
                    }
                }
            }
            let len = buf.len();
            let start_pos = 0;
            while start_pos + 4 <= len {
                let msg_len = bincode::deserialize(&buf[start_pos..(start_pos + 4)]);
                if msg_len + start_pos > len {
                    break;
                }
                let msg:VfsRequestMsg = bincode::deserialize(&buf[(start_pos+4)..(start_pos+msg_len)]);
                self.handle_request(&mut ci, msg);
                start_pos += msg_len;:w
            }
            buf = buf.split_off(start_pos);
            if should_finish {
                if !buf.empty() {
                    return Error(PipeCloseMiddle);
                } else {
                    self.finish_read(toke, &mut ci);
                }
            }
        } else {
            panic!("impossible condition");
        }
    }

    fn handle_write(&mut self, token: Token, ci: &mut ConnectionInfo) -> std::io::Result<()> {
        if let PipeWriteState::Expecting(&mut buf) == ci.write_buf {
            let len = buf.len();
            let start_pos:usize = 0;
            while len > start_pos {
                let res = unsafe {
                    libc::write(ci.server_end_point.write_fd, &buf[0] as *const libc::c_void, (len - start_pos) as libc::size_t);
                };
                if res > 0 {
                    start_pos += res;
                } else if res == 0 {
                    panic!("write zero byte on pipe, how could this happen?");
                } else {
                    match std::io::error::Error::last_os_error() {
                        libc::EWOULDBLOCK | libc::AGAIN => {
                            break;
                        },
                        _ => {
                            return Error(PipeBroken);
                        }
                    }
                }
            }
            buf.split_off(start_pos);
            if buf.is_empty() {
                EventedFd(&ci.server_end_point.write_fd).deregister(&self.poll);
            }
        } else {
            panic!("spurious write envent");
        }
    }
}

impl<U> VfsIpcServer<U> for LinuxVfsIpcServer<U> {
    type Channel = LinuxVfsIpcChannel<U>;
    type ServerEndPoint = LinuxVfsIpcServerEndPoint<U>;
    type ClientEndPoint = LinuxVfsIpcClientEndPoint<U>;

    fn new(vfs: Arc<Vfs>) -> std::io::Result<Self> {
        Ok(Self {
            connection_infos: HashMap::new(),
            live_maps: HashMap::new(),
            poll: Poll::new()?,
            vfs,
            _u: PhantomData,
        })
    }

    fn roll_the_loop(&mut self) -> std::io::Result<()> {
        // FIXME: a better capacity
        let mut events = mio::Events::with_capacity(64);
        loop {
            self.poll.poll(&mut events, None)?;
            for event in &events {
                let tok = event.token();
                let ref mut ci = match self.connection_infos.get_mut(tok) {
                    Some(ref mut ci) => ci,
                    None() => return Err(std::io::Error::new(std::io::ErrorKind::NotFound, Box::new(TokenNotFound)));
                }
                let ready = readiness();
                if ready & mio::Ready::readable() {
                    self.handle_read(tok, &mut ci)?;
                }
                if ready & mio::Ready::writable() {
                    self.handle_write(tok, &mut ci)?;
                }
            }
        }
    }

    fn add_server_end_point(&mut self, s_ep: Self::ServerEndPoint) -> Token {
        use mio::{event::Evented, unix::EventedFd};
        // fd's are unique
        let tok_usize = s_ep.read_fd as usize;
        let tok = Token(tok_usize);
        EventedFd(&s_ep.read_fd).register(&self.poll, tok, mio::Ready::readable(), mio::PollOpt::edge());
        tok
    }

    fn remove_server_end_point(&mut self, tok: Token) {
        use mio::{event::Evented, unix::EventedFd};
        match self.connection_infos.remove(&tok) {
            Some(ci) => {
                EventedFd(&ci.server_end_point.read_fd).deregister(&self.poll);
                EventedFd(&ci.server_end_point.write_fd).deregister(&self.poll);
                for mi in ci.opened_files {
                    if Rc::<MapInfo>::strong_count(&mi) == 2 {
                        mi.close();
                        self.live_maps.remove(&mi.file_path);
                    }
                }
            },
            None => {
            }
        }
    }
}

pub struct LinuxVfsIpcClientEndPoint<U> {
    read_fd: libc::c_int,
    write_fd: libc::c_int,
    _u: PhantomData<U>
}

impl<U> LinuxVfsIpcClientEndPoint<U> {
    fn new(read_fd: libc::c_int, write_fd: libc::c_int) -> Self {
        Self {
            read_fd,
            write_fd,
            _u: std::marker::PhantomData,
        }
    }
}

impl<U> VfsIpcClientEndPoint<U> for LinuxVfsIpcClientEndPoint<U> {
    fn request_file(path: &std::path::Path) -> (String, U) {
        unimplemented!();
    }
}

impl<U> Drop for LinuxVfsIpcClientEndPoint<U> {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

pub struct LinuxVfsIpcServerEndPoint<U> {
    read_fd: libc::c_int,
    write_fd: libc::c_int,
    _u: PhantomData<U>
}

impl<U> LinuxVfsIpcServerEndPoint<U> {
    fn new(read_fd: libc::c_int, write_fd: libc::c_int) -> Self {
        unsafe {
            libc::fcntl(read_fd, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(write_fd, libc::F_SETFL, libc::O_NONBLOCK);
        }
        Self {
            read_fd,
            write_fd,
            _u: std::marker::PhantomData,
        }
    }
}

impl<U> Drop for LinuxVfsIpcServerEndPoint<U> {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

impl<U> VfsIpcServerEndPoint<U> for LinuxVfsIpcServerEndPoint<U> {
}

struct LinuxVfsIpcFileHandle<U> {
    addr: *mut libc::c_void,
    length: libc::size_t,
    user_data: U
}

impl<U> LinuxVfsIpcFileHandle<U> {
    pub fn from_reply(reply: VfsReplyMsg<U>) -> Self {
        let addr;
        let length = reply.length as libc::size_t;
        unsafe {
            let shm_oflag = libc::O_RDONLY;
            let shm_mode: libc::mode_t = 0;
            let shm_fd = libc::shm_open(reply.path.as_ptr() as *const i8, shm_oflag, shm_mode);

            let mmap_prot = libc::PROT_READ;
            // shared map to save us a few memory pages
            // only the server write to the mapped area, the clients only read them, so no problem here
            let mmap_flags = libc::MAP_SHARED;
            addr = libc::mmap(0 as *mut libc::c_void, length, mmap_prot, mmap_flags, shm_fd, 0 as libc::off_t);
        }
        Self {
            addr,
            length,
            user_data: reply.user_data,
        }
    }
}

impl<U> VfsIpcFileHandle<U> for LinuxVfsIpcFileHandle<U> {
    fn get_file_ref(&self) -> &str {
        // NB: whether the file contents are valid utf8 are never checked
        unsafe {
            let slice = std::slice::from_raw_parts(self.addr as *const u8, self.length as usize);
            std::str::from_utf8_unchecked(&slice)
        }
    }

    fn get_user_data_ref(&self) -> &U {
        &self.user_data
    }
}

impl<U> Drop for LinuxVfsIpcFileHandle<U> {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.addr, self.length);
        }
    }
}

