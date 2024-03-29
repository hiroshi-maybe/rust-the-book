use futures::{
    future::BoxFuture,
    task::{waker_ref, ArcWake},
    Future, FutureExt,
};
use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, BufWriter, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::{io::RawFd, prelude::AsRawFd},
    pin::Pin,
    ptr,
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex,
    },
    task::{Context, Poll, Waker},
};

/// Echo server with async/await using kqueue instead of epoll for MacOS
///
/// Usage:
/// $ socat stdio tcp:localhost:10000
///
/// References:
/// * http://web.mit.edu/freebsd/head/tools/regression/kqueue/user.c
/// * https://fergofrog.com/code/cbowser/xnu/bsd/sys/event.h.html
/// * https://stackoverflow.com/questions/16072395/using-kqueue-for-evfilt-user
/// * https://cfsamsonbooks.gitbook.io/epoll-kqueue-iocp-explained/part-1-an-express-explanation/kqueue-the-express-version
/// * https://habr.com/en/post/600123/#freebsdmacos-and-kqueue
/// * https://stackoverflow.com/questions/26603615/os-x-alternative-to-eventfd
///   *  pipe or EVFILT_USER
/// * https://github.com/rust-lang/libc/blob/c8aa8ec72d631bc35099bcf5d634cf0a0b841be0/src/unix/bsd/apple/mod.rs#L497
/// * https://github.com/rust-lang/libc/blob/c8aa8ec72d631bc35099bcf5d634cf0a0b841be0/src/unix/bsd/apple/mod.rs#L207
/// * https://wiki.netbsd.org/tutorials/kqueue_tutorial/
/// * https://www.freebsd.org/cgi/man.cgi?query=eventfd&apropos=0&sektion=2&manpath=FreeBSD+13.0-RELEASE+and+Ports&arch=default&format=html
/// * https://gist.github.com/nineright/9431572


struct Task {
    future: Mutex<BoxFuture<'static, ()>>,
    sender: SyncSender<Arc<Task>>,
}

impl ArcWake for Task {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        println!("[Task] waker (=Task itself) awoken");
        let self0 = arc_self.clone();
        println!("[Task] sent by waker (=Task itself) to the queue");
        arc_self.sender.send(self0).unwrap();
    }
}

struct Executor {
    sender: SyncSender<Arc<Task>>,
    receiver: Receiver<Arc<Task>>,
}

impl Executor {
    fn new() -> Self {
        let (sender, receiver) = sync_channel(1024);
        println!("[Executor] instantiated");
        Executor {
            sender: sender.clone(),
            receiver,
        }
    }

    fn get_spawner(&self) -> Spawner {
        println!("[Executor] created spawner");
        Spawner {
            sender: self.sender.clone(),
        }
    }

    fn run(&self) {
        println!("[Executor] started running");
        let mut cnt = 1;
        while let Ok(task) = self.receiver.recv() {
            println!("[Executor][{}] received task", cnt);
            let mut future = task.future.lock().unwrap();
            let waker = waker_ref(&task);
            println!("[Executor][{}] obtained waker from task", cnt);
            let mut ctx = Context::from_waker(&waker);
            println!("[Executor][{}] started polling", cnt);
            let _ = future.as_mut().poll(&mut ctx);
            println!("[Executor][{}] finished polling", cnt);
            cnt += 1;
        }
        println!("[Executor] finished running");
    }
}

struct Spawner {
    sender: SyncSender<Arc<Task>>,
}

impl Spawner {
    fn spawn(&self, future: impl Future<Output = ()> + 'static + Send) {
        let future = future.boxed();
        let task = Arc::new(Task {
            future: Mutex::new(future),
            sender: self.sender.clone(),
        });

        println!("[Spawner] sent task to the queue");
        self.sender.send(task).unwrap();
    }
}

fn main() {
    let executor = Executor::new();
    let selector = IOSelector::new();
    let spawner = executor.get_spawner();

    let server = async move {
        println!("[ServerFuture] instantiated");
        let listener = AsyncListener::listen("127.0.0.1:10000", selector.clone());

        loop {
            println!("[ServerFuture] waiting for connection accept");
            let (mut reader, mut writer, addr) = listener.accept().await;
            println!("[ServerFuture] accept: {}", addr);

            spawner.spawn(async move {
                while let Some(buf) = reader.read_line().await {
                    print!("[ServerFuture] read: {}, {}", addr, buf);
                    writer.write(buf.as_bytes()).unwrap();
                    writer.flush().unwrap();
                }
                println!("[ServerFuture] close: {}", addr);
            });
        }
    };

    executor.get_spawner().spawn(server);
    executor.run();
}

fn write_eventfd(kq: RawFd, ident: usize) {
    println!("[write_eventfd] New EVFILT_USER event: {}", ident);
    let ev = ffi::Kevent {
        ident: ident as u64,
        filter: ffi::EVFILT_USER,
        flags: 0,
        fflags: ffi::NOTE_TRIGGER,
        data: 0,
        udata: 100,
    };
    let read_events = vec![ev];
    let res = unsafe { ffi::kevent(kq, read_events.as_ptr(), 1, ptr::null_mut(), 0, ptr::null()) };
    if res < 0 {
        panic!(
            "[write_eventfd] Cannot trigger events for EVFILT_USER: {}",
            std::io::Error::last_os_error()
        );
    }
}

enum IOOps {
    Add(i16, RawFd, Waker), // EVFILT_X
    Remove(i16, RawFd),
}

struct IOSelector {
    wakers: Mutex<HashMap<RawFd, Waker>>,
    queue: Mutex<VecDeque<IOOps>>,
    kqfd: RawFd,
    event_ident: usize,
}

impl IOSelector {
    fn new() -> Arc<Self> {
        let kq = unsafe { ffi::kqueue() };
        assert!(kq >= 0);
        let s = IOSelector {
            wakers: Mutex::new(HashMap::new()),
            queue: Mutex::new(VecDeque::new()),
            kqfd: kq,
            event_ident: 1234,
        };
        let result = Arc::new(s);
        let s = result.clone();

        println!("[IOSelector] instantiated with kq fd {}", kq);
        std::thread::spawn(move || s.select());

        result
    }

    fn add_event(
        &self,
        filter_flag: i16,
        fd: RawFd,
        waker: Waker,
        wakers: &mut HashMap<RawFd, Waker>,
    ) {
        println!(
            "[IOSelector] add event for fd {:?} and filter {}",
            fd, filter_flag
        );

        let ev = ffi::Kevent {
            ident: fd as u64,
            filter: filter_flag,
            flags: ffi::EV_ADD | ffi::EV_ONESHOT,
            fflags: 0,
            data: 0,
            udata: 0,
        };
        let read_events = vec![ev];
        let res = unsafe {
            ffi::kevent(
                self.kqfd,
                read_events.as_ptr(),
                1,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        };

        if res < 0 {
            panic!(
                "[IOSelector] Cannot register EV_ADD event for fd {:?}: {}",
                fd,
                std::io::Error::last_os_error()
            );
        }

        wakers.insert(fd, waker);
    }

    fn rm_event(&self, filter_flag: i16, fd: RawFd, wakers: &mut HashMap<RawFd, Waker>) {
        println!(
            "[IOSelector] remove event for fd {:?} and filter {}",
            fd, filter_flag
        );
        let ev = ffi::Kevent {
            ident: fd as u64,
            filter: filter_flag,
            flags: ffi::EV_DELETE,
            fflags: 0,
            data: 0,
            udata: 0,
        };
        let read_events = vec![ev];
        let res = unsafe {
            ffi::kevent(
                self.kqfd,
                read_events.as_ptr(),
                1,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        };

        if res < 0 {
            panic!(
                "[IOSelector] Cannot register EV_DELETE event for fd {:?}: {}",
                fd,
                std::io::Error::last_os_error()
            );
        }

        unsafe { ffi::close(fd) };
        wakers.remove(&fd);
    }

    fn select(&self) {
        // https://stackoverflow.com/questions/16072395/using-kqueue-for-evfilt-user
        let ev = ffi::Kevent {
            ident: self.event_ident as u64,
            filter: ffi::EVFILT_USER,
            flags: ffi::EV_ADD,
            fflags: ffi::NOTE_FFCOPY,
            data: 0,
            udata: 100,
        };
        let read_events = vec![ev];
        let res = unsafe {
            ffi::kevent(
                self.kqfd,
                read_events.as_ptr(),
                1,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        };

        println!(
            "[IOSelector] started observing user event: {}",
            self.event_ident
        );

        if res < 0 {
            panic!(
                "[IOSelector] Cannot register events: {}",
                std::io::Error::last_os_error()
            );
        }

        loop {
            let mut events: Vec<ffi::Kevent> = Vec::with_capacity(1000);

            let res = unsafe {
                ffi::kevent(
                    self.kqfd,
                    ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.capacity() as i32,
                    ptr::null(),
                )
            };
            if res < 0 {
                break;
            }

            unsafe { events.set_len(res as usize) };

            for n in 0..res {
                let e = &events[n as usize];
                let mut t = self.wakers.lock().unwrap();
                if e.ident == self.event_ident as u64 {
                    let mut q = self.queue.lock().unwrap();
                    while let Some(op) = q.pop_front() {
                        match op {
                            IOOps::Add(flag, fd, waker) => self.add_event(flag, fd, waker, &mut t),
                            IOOps::Remove(flag, fd) => self.rm_event(flag, fd, &mut t),
                        }
                    }
                } else {
                    let fd = events[n as usize].ident as RawFd;
                    let waker = t.remove(&fd).unwrap();
                    waker.wake_by_ref();
                }
            }
        }
    }

    fn register(&self, flags: i16, fd: RawFd, waker: Waker) {
        let mut q = self.queue.lock().unwrap();
        q.push_back(IOOps::Add(flags, fd, waker));
        write_eventfd(self.kqfd, self.event_ident);
    }

    fn unregister(&self, flags: i16, fd: RawFd) {
        let mut q = self.queue.lock().unwrap();
        q.push_back(IOOps::Remove(flags, fd));
        write_eventfd(self.kqfd, self.event_ident);
    }
}

// https://stackoverflow.com/questions/26603615/os-x-alternative-to-eventfd
// - pipe or EVFILT_USER
// https://habr.com/en/post/600123/#freebsdmacos-and-kqueue

// https://cfsamsonbooks.gitbook.io/epoll-kqueue-iocp-explained/part-1-an-express-explanation/kqueue-the-express-version

mod ffi {
    pub const EVFILT_READ: i16 = -1;
    pub const EVFILT_USER: i16 = -10;
    pub const EV_ADD: u16 = 0x1;
    // pub const EV_ENABLE: u16 = 0x4;
    pub const EV_ONESHOT: u16 = 0x10;
    pub const EV_DELETE: u16 = 0x2;

    /*
    #define NOTE_TRIGGER	0x01000000
    #define NOTE_FFNOP      0x00000000              /* ignore input fflags */
    #define NOTE_FFAND      0x40000000              /* and fflags */
    #define NOTE_FFOR       0x80000000              /* or fflags */
    #define NOTE_FFCOPY     0xc0000000              /* copy fflags */
    #define NOTE_FFCTRLMASK 0xc0000000              /* mask for operations */
    #define NOTE_FFLAGSMASK	0x00ffffff
     */
    pub const NOTE_TRIGGER: u32 = 0x01000000;
    pub const NOTE_FFCOPY: u32 = 0xc0000000; /* copy fflags */

    #[derive(Debug)]
    #[repr(C)]
    pub(super) struct Timespec {
        /// Seconds
        tv_sec: isize,
        /// Nanoseconds
        v_nsec: usize,
    }

    impl Timespec {
        #[allow(dead_code)]
        pub fn from_millis(milliseconds: i32) -> Self {
            let seconds = milliseconds / 1000;
            let nanoseconds = (milliseconds % 1000) * 1000 * 1000;
            Timespec {
                tv_sec: seconds as isize,
                v_nsec: nanoseconds as usize,
            }
        }
    }

    // https://github.com/rust-lang/libc/blob/c8aa8ec72d631bc35099bcf5d634cf0a0b841be0/src/unix/bsd/apple/mod.rs#L497
    // https://github.com/rust-lang/libc/blob/c8aa8ec72d631bc35099bcf5d634cf0a0b841be0/src/unix/bsd/apple/mod.rs#L207
    #[derive(Debug, Clone, Default)]
    #[repr(C)]
    pub struct Kevent {
        pub ident: u64,
        pub filter: i16,
        pub flags: u16,
        pub fflags: u32,
        pub data: i64,
        pub udata: u64,
    }

    #[link(name = "c")]
    extern "C" {
        /// Returns: positive: file descriptor, negative: error
        pub(super) fn kqueue() -> i32;

        pub(super) fn kevent(
            kq: i32,
            changelist: *const Kevent,
            nchanges: i32,
            eventlist: *mut Kevent,
            nevents: i32,
            timeout: *const Timespec,
        ) -> i32;

        pub fn close(d: i32) -> i32;
    }
}

/// TCP listener

struct AsyncListener {
    tcp_listener: TcpListener,
    selector: Arc<IOSelector>,
}

impl AsyncListener {
    fn listen(addr: &str, selector: Arc<IOSelector>) -> AsyncListener {
        println!("[AsyncListener] started listening to {}", addr);
        let tcp_listener = TcpListener::bind(addr).unwrap();

        tcp_listener.set_nonblocking(true).unwrap();

        AsyncListener {
            tcp_listener,
            selector,
        }
    }

    fn accept(&self) -> Accept {
        Accept { listener: self }
    }
}

impl Drop for AsyncListener {
    fn drop(&mut self) {
        self.selector
            .unregister(ffi::EVFILT_READ, self.tcp_listener.as_raw_fd());
    }
}

struct Accept<'a> {
    listener: &'a AsyncListener,
}

impl<'a> Future for Accept<'a> {
    type Output = (AsyncReader, BufWriter<TcpStream>, SocketAddr);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        println!("[Accept] polled");
        match self.listener.tcp_listener.accept() {
            Ok((stream, addr)) => {
                let s = stream.try_clone().unwrap();
                Poll::Ready((
                    AsyncReader::new(s, self.listener.selector.clone()),
                    BufWriter::new(stream),
                    addr,
                ))
            }
            Err(err) => {
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    self.listener.selector.register(
                        ffi::EVFILT_READ,
                        self.listener.tcp_listener.as_raw_fd(),
                        cx.waker().clone(),
                    );
                    Poll::Pending
                } else {
                    panic!("[Accept] accept: {}", err);
                }
            }
        }
    }
}

struct AsyncReader {
    tcpstream_fd: RawFd,
    reader: BufReader<TcpStream>,
    selector: Arc<IOSelector>,
}

impl AsyncReader {
    fn new(stream: TcpStream, selector: Arc<IOSelector>) -> AsyncReader {
        stream.set_nonblocking(true).unwrap();
        AsyncReader {
            tcpstream_fd: stream.as_raw_fd(),
            reader: BufReader::new(stream),
            selector: selector,
        }
    }

    fn read_line(&mut self) -> ReadLine {
        ReadLine { reader: self }
    }
}

impl Drop for AsyncReader {
    fn drop(&mut self) {
        // This drop introduces "No such file or directory (os error 2)" error
        // self.selector
        //     .unregister(ffi::EVFILT_READ, self.tcpstream_fd);
    }
}

struct ReadLine<'a> {
    reader: &'a mut AsyncReader,
}

impl<'a> Future for ReadLine<'a> {
    type Output = Option<String>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut line = String::new();

        match self.reader.reader.read_line(&mut line) {
            Ok(0) => Poll::Ready(None),
            Ok(_) => Poll::Ready(Some(line)),
            Err(err) => {
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    self.reader.selector.register(
                        ffi::EVFILT_READ,
                        self.reader.tcpstream_fd,
                        cx.waker().clone(),
                    );
                    Poll::Pending
                } else {
                    Poll::Ready(None)
                }
            }
        }
    }
}
