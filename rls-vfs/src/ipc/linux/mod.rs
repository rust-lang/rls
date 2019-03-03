#[macro_use]
mod error;

pub use error::{LibcError, RlsVfsIpcError};
//use error::{would_block_or_error, handle_libc_error, fake_libc_error};

use super::*;
use std::sync::Arc;
use std::clone::Clone;
use std::rc::Rc;
use std::cell::RefCell;
use std::boxed::Box;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use mio::{Poll, Token};
use serde::{Serialize, Deserialize};

pub type Result<T> = std::result::Result<T, RlsVfsIpcError>;
pub type LibcResult<T> = std::result::Result<T, LibcError>;

pub enum Fd {
    Closed,
    Open(libc::c_int),
}

impl Fd {
    pub fn from_raw(fd: libc::c_int) -> Self {
        Fd::Open(fd)
    }

    pub fn close(&mut self) -> LibcResult<()> {
        match self {
            Fd::Closed => {
                // fake a libc error of invalid fd, otherwise would complicate our error hierarchy
                fake_libc_error!("close", libc::EBADF);
            },
            Fd::Open(fd) => {
                let res = unsafe {
                    libc::close(*fd)
                };
                if res < 0 {
                    handle_libc_error!("close");
                } else {
                    *self = Fd::Closed;
                    Ok(())
                }
            }
        }
    }

    pub fn unwrap(&self) -> LibcResult<libc::c_int> {
        match self {
            Fd::Closed => {
                fake_libc_error!("Fd::unwrap", libc::EBADF);
            }
            Fd::Open(fd) => {
                Ok(*fd)
            }
        }
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        match self {
            Fd::Open(fd) => {
                if unsafe { libc::close(*fd) } < 0 {
                    panic!("error while closing Fd");
                }
            }
            Fd::Closed => ()
        }
    }
}

struct Pipe {
    read_fd: Fd,
    write_fd: Fd,
}

impl Pipe {
    pub fn new() -> LibcResult<Pipe> {
        let mut fds: [libc::c_int;2] = unsafe {std::mem::uninitialized() };
         let res = unsafe {
            libc::pipe2(&mut fds[0] as *mut libc::c_int, 0)
         };
         if res < 0 {
             handle_libc_error!("pipe2");
         }
         Ok(Pipe {
             read_fd: Fd::from_raw(fds[0]),
             write_fd: Fd::from_raw(fds[1]),
         })
    }

    fn close_write(&mut self) -> LibcResult<()> {
        self.write_fd.close()
    }

    pub fn close_read(&mut self) -> LibcResult<()> {
        self.read_fd.close()
    }
}

pub struct LinuxVfsIpcChannel {
    s2c_pipe: Pipe,
    c2s_pipe: Pipe,
}

impl VfsIpcChannel for LinuxVfsIpcChannel {
    type ServerEndPoint = LinuxVfsIpcServerEndPoint;
    type ClientEndPoint = LinuxVfsIpcClientEndPoint;
    type Error = LibcError;

    fn new_prefork() -> LibcResult<Self> {
        Ok( LinuxVfsIpcChannel {
            s2c_pipe: Pipe::new()?,
            c2s_pipe: Pipe::new()?,
        }
    }

    fn into_server_end_point_postfork(mut self) -> LibcResult<Self::ServerEndPoint> {
        self.s2c_pipe.close_read()?;
        self.c2s_pipe.close_write()?;
        Self::ServerEndPoint::new(self.c2s_pipe.read_fd, self.s2c_pipe.write_fd)
    }

    fn into_client_end_point_postfork(mut self) -> LibcResult<Self::ClientEndPoint> {
        self.s2c_pipe.close_write()?;
        self.c2s_pipe.close_read()?;
        Self::ClientEndPoint::new(self.s2c_pipe.read_fd, self.c2s_pipe.write_fd)
    }
}

struct PipeReadState {
    buf: Vec<u8>
}

struct PipeWriteState {
    buf: Vec<u8>
}

// information about a connection that is kept on the server side
struct ConnectionInfo {
    server_end_point: LinuxVfsIpcServerEndPoint,
    // NB: it is assumed clients's requests are unique (with respect to their canonical path), duplicated open for the same file should be
    // handled on the client side.
    opened_files: HashMap<PathBuf, Rc<MapInfo>>,
    read_state: PipeReadState,
    write_state: PipeWriteState,
}


// information about a established mmap,
// the ref-count is kept implicitly by Rc<MapInfo>
// the real_path is kept by the key of a HashMap<PathBuf, Rc<MapInfo>>
// NB: real_path should be canonical when appears in HashMap
struct MapInfo {
    // NB: make sure shm_name is null-terminated
    shm_name: String,
    length: libc::size_t,
}

impl MapInfo {
    // construct a mmap, currently you can not query vfs for the version of a file
    pub fn open(cont: &[u8], shm_name:String) -> LibcResult<Self> {
        let length = cont.len() as libc::size_t;
        unsafe {
            let shm_oflag = libc::O_CREAT | libc::O_EXCL | libc::O_RDWR;
            let shm_mode = libc::S_IRUSR | libc::S_IWUSR;
            let shm_fd = libc::shm_open(shm_name.as_ptr() as *const libc::c_char, shm_oflag, shm_mode);

            if shm_fd < 0 {
                handle_libc_error!("shm_open");
            }

            if libc::ftruncate(shm_fd, length as libc::off_t) < 0 {
                handle_libc_error!("ftruncate");
            }

            let mmap_prot = libc::PROT_READ | libc::PROT_WRITE;
            // shared map to save us a few memory pages
            // only the server write to the mapped area, the clients only read them, so no problem here
            let mmap_flags = libc::MAP_SHARED;
            let mmap_addr = libc::mmap(0 as *mut libc::c_void, length, mmap_prot, mmap_flags, shm_fd, 0);
            if mmap_addr == libc::MAP_FAILED {
                handle_libc_error!("mmap");
            }
            std::ptr::copy_nonoverlapping(cont.as_ptr() as *const u8, mmap_addr as *mut u8, length);
            if libc::munmap(mmap_addr, length as libc::size_t) < 0 {
                handle_libc_error!("munmap");
            }

            if libc::close(shm_fd) < 0 {
                handle_libc_error!("close");
            }
        }

        Ok(Self {
            shm_name,
            length,
        })
    }

    // close a shared memory, after closing, clients won't be able to "connect to" this mmap, buf existing
    // shms are not invalidated.
    pub fn close(&self) -> LibcResult<()> {
        if unsafe {
            libc::shm_unlink(self.shm_name.as_ptr() as *const libc::c_char)
        } < 0 {
            handle_libc_error!("shm_unlink");
        }
        Ok(())
    }
}

// a server that takes care of handling client's requests and managin mmap
pub struct LinuxVfsIpcServer<U: Serialize + Deserialize + Clone> {
    // need a Rc<RefCell<_>>, because we didn't want to consume the &mut self when taking a &mut
    // ConnectionInfo
    connection_infos: HashMap<Token, Rc<RefCell<ConnectionInfo>>>,
    live_maps: HashMap<PathBuf, Rc<MapInfo>>,
    poll: Poll,
    vfs: Arc<Vfs<U>>,
    server_pid: u32,
    timestamp: usize
}

impl<U: Serialize + Deserialize + Clone> LinuxVfsIpcServer<U> {
    fn handle_request(&mut self, tok: Token, ci: Rc<RefCell<ConnectionInfo>>, req: VfsRequestMsg) -> Result<()> {
        match req {
            VfsRequestMsg::OpenFile(path) => {
                self.handle_open_request(tok, ci, path)
            },
            VfsRequestMsg::CloseFile(path) => {
                self.handle_close_request(tok, ci, path)
            },
        }
    }

    fn try_set_up_mmap(&mut self, path: &Path) -> Result<(Rc<MapInfo>, U)> {
        // TODO: currently, vfs doesn't restrict which files are allowed to be opened, this may
        // need some change in the future.
        let path = path.canonicalize()?;

        // TODO: more efficient impl, less memory copy and lookup
        use std::collections::hash_map::RawEntryMut;
        use super::super::FileContents;
        let mi = match self.live_maps.raw_entry_mut().from_key(&path) {
            RawEntryMut::Occupied(occ) => {
                occ.get().clone()
            },
            RawEntryMut::Vacant(vac) => {
                let shm_name = self.generate_shm_name(&path);
                match self.vfs.load_file(&path)? {
                    FileContents::Text(s) => {
                        Rc::new(MapInfo::open(s.as_bytes(), shm_name)?)
                    }
                    FileContents::Binary(v) => {
                        Rc::new(MapInfo::open(&v, shm_name)?)
                    }
                }
            },
        };
        let u = self.vfs.with_user_data(&path, |res| {
            match res {
                Err(err) => Err(err),
                Ok((_, u)) => {
                    Ok(u.clone())
                },
            }
        })?;
        Ok((mi, u))
    }

    fn handle_open_request(&mut self, token: Token, ci: Rc<RefCell<ConnectionInfo>>, path: PathBuf) -> Result<()> {
        let (map_info, user_data) = self.try_set_up_mmap(&path)?;
        let reply_msg = VfsReplyMsg::<U> {
            path: map_info.shm_name.clone(),
            length: map_info.length as u32,
            user_data
        };
        ci.borrow_mut().opened_files.insert(path, map_info);
        self.write_reply(token, ci, reply_msg)
    }

    fn write_reply(&mut self, token: Token, ci: Rc<RefCell<ConnectionInfo>>, reply_msg: VfsReplyMsg<U>) -> Result<()> {
        let old_len = ci.borrow().write_state.buf.len();
        {
            let mut ext = match bincode::serialize(&reply_msg) {
                Ok(ext) => ext,
                Err(err) => {
                    return Err(RlsVfsIpcError::SerializeError(err))
                }
            };
            ci.borrow_mut().write_state.buf.append(&mut ext);
        }

        if old_len == 0usize {
            // this means the write-fd is not in the poll
            self.initial_write(token, ci)?;
        }
        Ok(())
        // else, there are on-going write on the event poll, which will carry this message
    }

    // the write-fd is not in the poll, first write as much as possible until EWOULDBLOCK, if still
    // some contents remain, register the write-fd to the poll
    fn initial_write(&mut self, token: Token, ci: Rc<RefCell<ConnectionInfo>>) -> Result<()> {
        let write_fd = ci.borrow().server_end_point.write_fd.unwrap()?;
        let mut ci = ci.borrow_mut();
        let len = ci.write_state.buf.len();
        let mut start_pos = 0usize;
        while start_pos < len {
            let res = 
            unsafe {
                libc::write(write_fd, &ci.write_state.buf[0] as *const u8 as *const libc::c_void, len - start_pos)
            };
            if res > 0 {
                start_pos += res as usize;
            } else if res == 0 {
                // same as EWOULDBLOCK
                break;
            } else {
                handle_libc_error!("write");
            }
        }
        ci.write_state.buf = ci.write_state.buf.split_off(start_pos);
        if start_pos < len {
            use mio::{event::Evented, unix::EventedFd};
            EventedFd(&write_fd).register(&self.poll, token, mio::Ready::readable(), mio::PollOpt::edge()|mio::PollOpt::oneshot())?;
        }
        Ok(())
    }

    fn handle_close_request(&mut self, _tok: Token, ci: Rc<RefCell<ConnectionInfo>>, path: PathBuf) -> Result<()> {
        let mut ci = ci.borrow_mut();
        match ci.opened_files.remove(&path) {
            Some(mi) => {
                self.try_remove_last_map(mi, &path);
            }
            None => {
                panic!()
            }
        }
        Ok(())
    }

    // a eof is met when reading a pipe, the connection's read side will not be used again(write
    // side may still be used to send replies)
    fn finish_read(&mut self, tok: Token, ci: Rc<RefCell<ConnectionInfo>>) -> Result<()> {
        // TODO
        Ok(())
    }

    // try to read some requests and handle them
    fn handle_read(&mut self, token: Token, ci: Rc<RefCell<ConnectionInfo>>) -> Result<()> {
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
        let read_fd = ci.borrow().server_end_point.read_fd.unwrap()?;
        loop {
            let res = unsafe {
                libc::read(read_fd, &mut buf1[0] as *mut u8 as *mut libc::c_void, std::mem::size_of_val(&buf1))
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
                        if would_block_or_error!("read") {
                            break;
                        }
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
                    return Err(RlsVfsIpcError::DeserializeError(err));
                }
            };
            if msg_len + start_pos > len {
                break;
            }
            let msg:VfsRequestMsg = match bincode::deserialize(&buf!()[(start_pos+4)..(start_pos+msg_len)]) {
                Ok(msg) => msg,
                Err(err) => {
                    return Err(RlsVfsIpcError::DeserializeError(err));
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
                return Err(RlsVfsIpcError::PipeCloseMiddle);
            }
        }
        Ok(())
    }

    // try to write some replies to the pipe
    fn handle_write(&mut self, token: Token, ci: Rc<RefCell<ConnectionInfo>>) -> Result<()> {
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
        let write_fd = ci.borrow().server_end_point.write_fd.unwrap()?;
        while len > start_pos {
            let res = unsafe {
                libc::write(write_fd, &buf!()[0] as *const u8 as *const libc::c_void, (len - start_pos) as libc::size_t)
            };
            if res > 0 {
                start_pos += res as usize;
            } else if res == 0 {
                // NB: same as EWOULDBLOCK
                break;
            } else {
                if would_block_or_error!("write") {
                    break;
                }
            }
        }

        {
            buf_mut!().split_off(start_pos);
        }
        if buf!().is_empty() {
            use mio::{event::Evented, unix::EventedFd};
            EventedFd(&write_fd).deregister(&self.poll)?;
        }
        Ok(())
    }

    fn generate_shm_name(&mut self, file_path: &Path) -> String {
        let ret = std::format!("/rls-{}-{}-{}\u{0000}", self.server_pid, file_path.display(), self.timestamp);
        self.timestamp += 1;
        ret
    }

    fn try_remove_last_map(&mut self, mi: Rc<MapInfo>, file_path: &Path) -> Result<()> {
        if Rc::<MapInfo>::strong_count(&mi) == 2 {
            mi.close();
            self.live_maps.remove(file_path);
        }
        Ok(())
    }
}

impl<U: Serialize + Deserialize + Clone> VfsIpcServer<U> for LinuxVfsIpcServer<U> {
    type Channel = LinuxVfsIpcChannel;
    type ServerEndPoint = LinuxVfsIpcServerEndPoint;
    type ClientEndPoint = LinuxVfsIpcClientEndPoint;
    type Error = RlsVfsIpcError;

    fn new(vfs: Arc<Vfs<U>>) -> Result<Self> {
        Ok(Self {
            connection_infos: HashMap::new(),
            live_maps: HashMap::new(),
            poll: Poll::new()?,
            vfs,
            server_pid: std::process::id(),
            timestamp: 0
        })
    }

    fn roll_the_loop(&mut self) -> Result<()> {
        // FIXME: a better capacity
        let mut events = mio::Events::with_capacity(64);
        loop {
            self.poll.poll(&mut events, None)?;
            for event in &events {
                let token = event.token();
                let ci = match self.connection_infos.get_mut(&token) {
                    Some(ci) => ci.clone(),
                    None => return Err(RlsVfsIpcError::TokenNotFound),
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

    fn add_server_end_point(&mut self, s_ep: Self::ServerEndPoint) -> Result<Token> {
        use mio::{event::Evented, unix::EventedFd};
        let read_fd = s_ep.read_fd.unwrap()?;
        // fd's are unique
        let tok_usize = read_fd as usize;
        let tok = Token(tok_usize);
        EventedFd(&read_fd).register(&self.poll, tok, mio::Ready::readable(), mio::PollOpt::edge())?;
        Ok(tok)
    }

    fn remove_server_end_point(&mut self, tok: Token) -> Result<()>{
        use mio::{event::Evented, unix::EventedFd};
        match self.connection_infos.remove(&tok) {
            Some(ci) => {
                let read_fd = ci.borrow().server_end_point.read_fd.unwrap()?;
                let write_fd = ci.borrow().server_end_point.write_fd.unwrap()?;
                EventedFd(&read_fd).deregister(&self.poll)?;
                EventedFd(&write_fd).deregister(&self.poll)?;
                for (file_path, mi) in ci.borrow_mut().opened_files.drain() {
                    self.try_remove_last_map(mi, &file_path);
                }
            },
            None => {
            }
        }
        Ok(())
    }
}

pub struct LinuxVfsIpcClientEndPoint {
    read_fd: Fd,
    write_fd: Fd,
}

impl LinuxVfsIpcClientEndPoint {
    pub fn new(read_fd: Fd, write_fd: Fd) -> LibcResult<Self> {
        Ok(Self {
            read_fd,
            write_fd,
        })
    }

    fn write_request(&mut self, req_msg: VfsRequestMsg) -> Result<()> {
        let buf = match bincode::serialize(&req_msg) {
            Ok(buf) => buf,
            Err(err) => {
                return Err(RlsVfsIpcError::SerializeError(err));
            }
        };
        let len = buf.len();
        let mut start_pos = 0;
        let write_fd = self.write_fd.unwrap()?;
        while start_pos < len {
            let res = unsafe {
                libc::write(write_fd, &buf[start_pos] as *const u8 as *const libc::c_void, len - start_pos)
            };
            if res < 0 {
                // TODO: more fine grained error handling
                handle_libc_error!("write");
            }
            start_pos += res as usize;
        }
        Ok(())
    }

    fn read_reply<U: Serialize + Deserialize + Clone>(&mut self) -> Result<VfsReplyMsg<U>> {
        let buf1:[u8;4096] = unsafe {std::mem::uninitialized()};
        let buf:Vec<u8>;
        let read_fd = self.read_fd.unwrap()?;
        macro_rules! read_and_append {
            () => {
                let res = unsafe {
                    libc::read(read_fd, &buf1[0] as *mut u8 as *mut libc::c_void, std::mem::size_of_val(&buf1))
                };
                if res < 0 {
                    handle_libc_error!("read");
                }
            }
        }
        loop {
            read_and_append!();
            if buf.len() >= 4 {
                break;
            }
        }
        let len = match bincode::deserialize(&buf[0..4]) {
            Ok(len) => len as usize,
            Err(err) => {
                return Err(RlsVfsIpcError::DeserializeError(err));
            },
        };
        buf.reserve(len);
        while buf.len() < len {
            read_and_append!();
        }
        match bincode::deserialize(&buf[4..len]) {
            Ok(ret) => {
                Ok(ret)
            },
            Err(err) => {
                Err(RlsVfsIpcError::DeserializeError(err));
            }
        }
    }
}

impl VfsIpcClientEndPoint for LinuxVfsIpcClientEndPoint {
    type Error = RlsVfsIpcError;
    type FileHandle = LinuxVfsIpcFileHandle;
    fn request_file<U: Serialize + Deserialize + Clone>(&mut self, path: &Path) -> Result<(Self::FileHandle, U)> {
        let req_msg = VfsRequestMsg::OpenFile(path.to_owned());
        self.write_request(req_msg)?;
        let rep_msg = self.read_reply::<U>()?;
        let res = Self::FileHandle::from_reply(rep_msg)?;
        Ok(res)
    }
}

pub struct LinuxVfsIpcServerEndPoint {
    read_fd: Fd,
    write_fd: Fd,
}

impl LinuxVfsIpcServerEndPoint {
    fn new(read_fd: Fd, write_fd: Fd) -> LibcResult<Self> {
        let r_fd = match read_fd {
            Fd::Open(fd) => {
                fd
            },
            Fd::Closed => {
                // TODO
                panic!()
            }
        };
        let w_fd = match write_fd {
            Fd::Open(fd) => {
                fd
            },
            Fd::Closed => {
                // TODO
                panic!()
            }
        };
        unsafe {
            if libc::fcntl(r_fd, libc::F_SETFL, libc::O_NONBLOCK) < 0 ||  libc::fcntl(w_fd, libc::F_SETFL, libc::O_NONBLOCK) < 0 {
                handle_libc_error!("fcntl");
            }
        }
        Ok(Self {
            read_fd,
            write_fd,
        })
    }
}

impl VfsIpcServerEndPoint for LinuxVfsIpcServerEndPoint {
}

pub struct LinuxVfsIpcFileHandle {
    addr: *mut libc::c_void,
    length: libc::size_t,
}

impl LinuxVfsIpcFileHandle {
    pub fn from_reply<U: Serialize + Deserialize + Clone>(reply: VfsReplyMsg<U>) -> LibcResult<(Self, U)> {
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
        Ok((Self {
            addr,
            length,
        }, reply.user_data))
    }
}

impl VfsIpcFileHandle for LinuxVfsIpcFileHandle {
    type Error = RlsVfsIpcError;
    fn get_file_ref(&self) -> Result<&str> {
        // NB: whether the file contents are valid utf8 are never checked
        unsafe {
            let slice = std::slice::from_raw_parts(self.addr as *const u8, self.length as usize);
            std::str::from_utf8_unchecked(&slice);
        }
        unimplemented!()
    }
}

impl Drop for LinuxVfsIpcFileHandle {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.addr, self.length);
        }
    }
}
