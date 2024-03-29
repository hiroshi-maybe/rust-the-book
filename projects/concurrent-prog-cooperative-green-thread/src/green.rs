use nix::sys::mman::{mprotect, ProtFlags};
use nix::unistd::SysconfVar;
use std::alloc::{alloc, dealloc, Layout};
use std::collections::{HashMap, HashSet, LinkedList};
use std::ffi::c_void;
use std::ptr;

/// References:
/// * https://github.com/oreilly-japan/conc_ytakano/blob/main/chap6/ch6_mult-x86_64-linux
/// * https://c9x.me/articles/gthreads/mach.html
/// * https://cs.brown.edu/courses/csci1310/2020/notes/l08.html#:~:text=The%20%25rip%20register%20on%20x86,in%20the%20program's%20code%20segment.
/// * https://www.cs.princeton.edu/courses/archive/spr18/cos217/lectures/15_AssemblyFunctions.pdf
/// * https://www.imperialviolet.org/2017/01/18/cfi.html

#[derive(Debug)]
#[repr(C)]
struct Registers {
    // should be preserved for calling function - start
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    // should be preserved for calling function - end
    rsp: u64,
    rdx: u64,
}

impl Registers {
    fn new(rsp: u64) -> Self {
        // x86_64 16 byte alignment, but it should be taken care by `Layout::from_size_align()` call
        // See https://cfsamson.gitbook.io/green-threads-explained-in-200-lines-of-rust/the-stack
        let aligned_rsp = rsp & !15;
        assert_eq!(aligned_rsp, rsp);
        Registers {
            rbx: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rsp,
            rdx: entry_point as u64, // <4>
        }
    }
}

extern "C" {
    fn set_context(ctx: *mut Registers) -> u64;
    fn switch_context(ctx: *const Registers, rsp_pad: u64) -> !;
}

#[allow(unused_macros)]
macro_rules! debug_reg {
    () => {
        let mut reg = Registers::new(0);
        let r = &mut reg as *mut Registers;
        unsafe {
            let res = set_context(r);
            assert_eq!(res, 0);
        }
        println!("[DEBUG] curent reg: {:?}", reg);
        unsafe {
            assert_eq!((*r).rbp % 16, 0);
        }
    };
}

type Entry = fn();

fn get_page_size() -> usize {
    // 4KiB in my Mac, that is the same value as Linux
    nix::unistd::sysconf(SysconfVar::PAGE_SIZE)
        .unwrap()
        .unwrap() as usize
}

#[derive(Debug)]
struct Context {
    regs: Registers,
    stack: *mut u8,
    stack_layout: Layout,
    entry: Entry,
    id: u64,
}

impl Context {
    fn get_regs_mut(&mut self) -> *mut Registers {
        &mut self.regs as *mut Registers
    }
    fn get_regs(&self) -> *const Registers {
        &self.regs as *const Registers
    }

    #[inline(never)]
    fn new(func: Entry, stack_size: usize, id: u64) -> Self {
        let layout = Layout::from_size_align(stack_size, get_page_size()).unwrap();
        let stack = unsafe { alloc(layout) };

        // Protect stack for potential stack overflow
        unsafe { mprotect(stack as *mut c_void, get_page_size(), ProtFlags::PROT_NONE).unwrap() };

        let regs = Registers::new(stack as u64 + stack_size as u64);

        let stack_bottom = stack as u64 + stack_size as u64;
        assert_eq!(stack_bottom % 16, 0);

        Context {
            regs,
            stack,
            stack_layout: layout,
            entry: func,
            id,
        }
    }
}

static mut CTX_MAIN: Option<Box<Registers>> = None;
static mut UNUSED_STACK: (*mut u8, Layout) = (ptr::null_mut(), Layout::new::<u8>());

static mut CONTEXTS: LinkedList<Box<Context>> = LinkedList::new();
static mut ID: *mut HashSet<u64> = ptr::null_mut();

static mut MESSAGES: *mut MappedList<u64> = ptr::null_mut();
static mut WAITING: *mut HashMap<u64, Box<Context>> = ptr::null_mut();

fn get_id() -> u64 {
    loop {
        let rnd = rand::random::<u64>();

        unsafe {
            if (*ID).insert(rnd) {
                return rnd;
            }
        }
    }
}

pub fn spawn(func: Entry, stack_size: usize) -> u64 {
    unsafe {
        let id = get_id();
        CONTEXTS.push_back(Box::new(Context::new(func, stack_size, id)));
        schedule();
        id
    }
}

pub fn schedule() {
    unsafe {
        if CONTEXTS.len() == 1 {
            return;
        }

        let mut ctx = CONTEXTS.pop_front().unwrap();
        let regs = ctx.get_regs_mut();
        CONTEXTS.push_back(ctx);

        let set_context_res = set_context(regs);
        if set_context_res == 0 {
            let next = CONTEXTS.front().unwrap();
            let reg = (**next).get_regs();
            switch_context(
                reg,
                if (*reg).rdx == entry_point as u64 {
                    8
                } else {
                    0
                },
            );
        } else {
            // println!(
            //     "jump back to right after set_context() ({:?}): {:?}",
            //     set_context_res, *regs
            // );
        }

        rm_unused_stack();
    }
}

#[no_mangle]
pub extern "C" fn entry_point() {
    unsafe {
        let ctx = CONTEXTS.front().unwrap();
        ((**ctx).entry)();

        let ctx = CONTEXTS.pop_front().unwrap();

        (*ID).remove(&ctx.id);
        UNUSED_STACK = ((*ctx).stack, (*ctx).stack_layout);

        match CONTEXTS.front() {
            Some(c) => {
                switch_context((**c).get_regs(), 0);
            }
            None => {
                if let Some(c) = &CTX_MAIN {
                    switch_context(&**c as *const Registers, 0);
                }
            }
        };
    }
    panic!("entry_point");
}

pub fn spawn_from_main(func: Entry, stack_size: usize) {
    unsafe {
        if let Some(_) = &CTX_MAIN {
            panic!("spawn_from_main is called twice");
        }

        CTX_MAIN = Some(Box::new(Registers::new(0)));
        if let Some(ctx) = &mut CTX_MAIN {
            let mut msgs = MappedList::new();
            MESSAGES = &mut msgs as *mut MappedList<u64>;

            let mut waiting = HashMap::new();
            WAITING = &mut waiting as *mut HashMap<u64, Box<Context>>;

            let mut ids = HashSet::new();
            ID = &mut ids as *mut HashSet<u64>;

            let set_context_res = set_context(&mut **ctx as *mut Registers);
            if set_context_res == 0 {
                CONTEXTS.push_back(Box::new(Context::new(func, stack_size, get_id())));
                let first = CONTEXTS.front().unwrap();
                switch_context(first.get_regs(), 8);
            }

            rm_unused_stack();

            CTX_MAIN = None;
            CONTEXTS.clear();
            MESSAGES = ptr::null_mut();
            WAITING = ptr::null_mut();
            ID = ptr::null_mut();

            msgs.clear();
            waiting.clear();

            ids.clear();
        }
    }
}

unsafe fn rm_unused_stack() {
    if UNUSED_STACK.0 != ptr::null_mut() {
        mprotect(
            UNUSED_STACK.0 as *mut c_void,
            get_page_size(),
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
        )
        .unwrap();
        dealloc(UNUSED_STACK.0, UNUSED_STACK.1);
        UNUSED_STACK = (ptr::null_mut(), Layout::new::<u8>());
    }
}

struct MappedList<T> {
    map: HashMap<u64, LinkedList<T>>,
}

impl<T> MappedList<T> {
    fn new() -> Self {
        MappedList {
            map: HashMap::new(),
        }
    }

    fn push_back(&mut self, key: u64, val: T) {
        if let Some(list) = self.map.get_mut(&key) {
            list.push_back(val);
        } else {
            let mut list = LinkedList::new();
            list.push_back(val);
            self.map.insert(key, list);
        }
    }

    fn pop_front(&mut self, key: u64) -> Option<T> {
        if let Some(list) = self.map.get_mut(&key) {
            let val = list.pop_front();
            if list.len() == 0 {
                self.map.remove(&key);
            }
            val
        } else {
            None
        }
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

pub fn send(key: u64, msg: u64) {
    unsafe {
        (*MESSAGES).push_back(key, msg);
        if let Some(ctx) = (*WAITING).remove(&key) {
            CONTEXTS.push_back(ctx);
        }
    }
    schedule();
}

pub fn recv() -> Option<u64> {
    unsafe {
        let key = CONTEXTS.front().unwrap().id;

        if let Some(msg) = (*MESSAGES).pop_front(key) {
            return Some(msg);
        }

        if CONTEXTS.len() == 1 {
            panic!("deadlock");
        }

        let mut ctx = CONTEXTS.pop_front().unwrap();
        let regs = ctx.get_regs_mut();
        (*WAITING).insert(key, ctx);

        if set_context(regs) == 0 {
            let next = CONTEXTS.front().unwrap();
            let reg = (**next).get_regs();
            switch_context(reg, 0);
        }

        rm_unused_stack();
        (*MESSAGES).pop_front(key)
    }
}
