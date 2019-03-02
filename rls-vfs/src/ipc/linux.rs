use super::*;
use std::sync::Arc;
use std::rc::Rc;
use std::cell::RefCell;
use std::boxed::Box;
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

struct PipeReadState {
    buf: Vec<u8>
}

struct PipeWriteState {
    buf: Vec<u8>
}

struct ConnectionInfo<U> {
    server_end_point: LinuxVfsIpcServerEndPoint<U>,
    opened_files: HashMap<PathBuf, Rc<MapInfo>>,
    read_state: PipeReadState,
    write_state: PipeWriteState,
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
    // need a Rc<RefCell<_>>, because we didn't want to consume the &mut self when taking a &mut
    // ConnectionInfo
    connection_infos: HashMap<Token, Rc<RefCell<ConnectionInfo<U>>>>,
    live_maps: HashMap<PathBuf, Rc<MapInfo>>,
    poll: Poll,
    vfs: Arc<Vfs>,
    _u: PhantomData<U>
}

struct TokenNotFound;

impl TokenNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "token not found")
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

struct BrokenPipe;

impl BrokenPipe {
    fn new() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, BrokenPipe)
    }

    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "pipe breaks while still reading/writing")
    }
}

impl std::fmt::Display for BrokenPipe {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        BrokenPipe::fmt(&self, f)
    }
}

impl std::fmt::Debug for BrokenPipe {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        BrokenPipe::fmt(&self, f)
    }
}

impl std::error::Error for BrokenPipe {
    fn source(&self) -> Option<&'static dyn std::error::Error> {
        None
    }
}

struct PipeCloseMiddle;

impl PipeCloseMiddle {
    fn new() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::ConnectionAborted, Box::new(PipeCloseMiddle))
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

impl<U> LinuxVfsIpcServer<U> {
    fn handle_request(&mut self, tok: Token, ci: Rc<RefCell<ConnectionInfo<U>>>, req: VfsRequestMsg) {
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

    fn finish_read(&mut self, tok: Token, ci: Rc<RefCell<ConnectionInfo<U>>>) -> std::io::Result<()> {
        unimplemented!();
    }

    fn handle_read(&mut self, token: Token, ci: Rc<RefCell<ConnectionInfo<U>>>) -> std::io::Result<()> {
        // FIXME: this is ugly, but I don't want to spell a long name
        macro_rules! buf_mut {
            () => {
                ci.borrow_mut().read_state.buf
            }
        };
        macro_rules! buf {
            () => {
                ci.borrow().read_state.buf
            }
        }

        let mut buf1:[u8;4096] = unsafe { std::mem::uninitialized() };
        let mut met_eof = false;
        loop {
            let res = unsafe {
                libc::read(ci.borrow().server_end_point.read_fd, &mut buf1[0] as *mut u8 as *mut libc::c_void, std::mem::size_of_val(&buf1))
            };
            if res > 0 {
                buf_mut!().extend_from_slice(&buf1[..(res as usize)]);
            } else {
                match res {
                    0 => {
                        met_eof = true;
                        break;
                    },
                    _ => {
                        let err = std::io::Error::last_os_error();
                        let err_code = err.raw_os_error().unwrap();
                        // I'm not sure whether EWOULDBLOCK and EAGAIN are guaranteed to be the
                        // same, so let's be safe
                        if err_code == libc::EWOULDBLOCK {
                            break;
                        }
                        if err_code == libc::EAGAIN {
                            break;
                        }
                        return Err(err);
                    }
                }
            }
        }

        let len = buf!().len();
        let mut start_pos = 0;
        while start_pos + 4 <= len {
            let msg_len = match bincode::deserialize::<u32>(&buf!()[start_pos..(start_pos + 4)]) {
                Ok(msg_len) => msg_len as usize,
                Err(err) => {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err));
                }
            };
            if msg_len + start_pos > len {
                break;
            }
            let msg:VfsRequestMsg = match bincode::deserialize(&buf!()[(start_pos+4)..(start_pos+msg_len)]) {
                Ok(msg) => msg,
                Err(err) => {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err));
                }
            };
            self.handle_request(token, ci.clone(), msg);
            start_pos += msg_len;
        }

        {
            buf_mut!() = buf_mut!().split_off(start_pos);
        }

        if met_eof {
            if buf!().is_empty() {
                self.finish_read(token, ci.clone())?;
            } else {
                return Err(PipeCloseMiddle::new());
            }
        }
        Ok(())
    }

    fn handle_write(&mut self, token: Token, ci: Rc<RefCell<ConnectionInfo<U>>>) -> std::io::Result<()> {
        // FIXME: this is ugly, but I don't want to spell a long name
        macro_rules! buf {
            () => {
                ci.borrow().write_state.buf
            }
        };

        macro_rules! buf_mut {
            () => {
                ci.borrow_mut().write_state.buf
            }
        };

        let len = buf!().len();
        let mut start_pos:usize = 0;
        while len > start_pos {
            let res = unsafe {
                libc::write(ci.borrow().server_end_point.write_fd, &buf!()[0] as *const u8 as *const libc::c_void, (len - start_pos) as libc::size_t)
            };
            if res > 0 {
                start_pos += res as usize;
            } else if res == 0 {
                panic!("write zero byte on pipe, how could this happen?");
            } else {
                let err_code = std::io::Error::last_os_error().raw_os_error().unwrap();
                // I'm not sure whether EWOULDBLOCK and EAGAIN are guaranteed to be the
                // same, so let's be safe
                if err_code == libc::EWOULDBLOCK {
                    break;
                }
                if err_code == libc::EAGAIN {
                    break;
                }
                return Err(BrokenPipe::new());
            }
        }

        {
            buf_mut!().split_off(start_pos);
        }
        if buf!().is_empty() {
            use mio::{event::Evented, unix::EventedFd};
            EventedFd(&ci.borrow().server_end_point.write_fd).deregister(&self.poll)?;
        }
        Ok(())
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
                let token = event.token();
                let ci = match self.connection_infos.get_mut(&token) {
                    Some(ci) => ci.clone(),
                    None => return Err(std::io::Error::new(std::io::ErrorKind::NotFound, Box::new(TokenNotFound))),
                };

                let ready = event.readiness();
                if ready.contains(mio::Ready::readable()) {
                    self.handle_read(token, ci.clone())?;
                }
                if ready.contains(mio::Ready::writable()) {
                    self.handle_write(token, ci.clone())?;
                }
            }
        }
    }

    fn add_server_end_point(&mut self, s_ep: Self::ServerEndPoint) -> std::io::Result<Token> {
        use mio::{event::Evented, unix::EventedFd};
        // fd's are unique
        let tok_usize = s_ep.read_fd as usize;
        let tok = Token(tok_usize);
        EventedFd(&s_ep.read_fd).register(&self.poll, tok, mio::Ready::readable(), mio::PollOpt::edge())?;
        Ok(tok)
    }

    fn remove_server_end_point(&mut self, tok: Token) -> std::io::Result<()>{
        use mio::{event::Evented, unix::EventedFd};
        match self.connection_infos.remove(&tok) {
            Some(ci) => {
                EventedFd(&ci.borrow().server_end_point.read_fd).deregister(&self.poll)?;
                EventedFd(&ci.borrow().server_end_point.write_fd).deregister(&self.poll)?;
                for (file_path, mi) in ci.borrow().opened_files.iter() {
                    if Rc::<MapInfo>::strong_count(mi) == 2 {
                        mi.close();
                        self.live_maps.remove(file_path);
                    }
                }
            },
            None => {
            }
        }
        Ok(())
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

