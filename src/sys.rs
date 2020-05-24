#[cfg(target_os = "linux")]
pub mod eventfd {
    use super::check_err;
    use std::os::unix::io::RawFd;

    pub type EfdFlags = libc::c_int;

    pub fn eventfd(initval: libc::c_uint, flags: EfdFlags) -> Result<RawFd, std::io::Error> {
        let res = unsafe { libc::eventfd(initval, flags) };

        check_err(res).map(|r| r as RawFd)
    }
}

#[cfg(target_os = "linux")]
pub mod unistd {
    use super::check_err;
    use std::os::unix::io::RawFd;

    pub fn close(fd: RawFd) -> Result<(), std::io::Error> {
        let res = unsafe { libc::close(fd) };

        check_err(res).map(drop)
    }

    pub fn dup(oldfd: RawFd) -> Result<RawFd, std::io::Error> {
        let res = unsafe { libc::dup(oldfd) };
        check_err(res)
    }

    pub fn read(fd: RawFd, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let res = unsafe {
            libc::read(
                fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len() as libc::size_t,
            )
        };

        check_err(res as _).map(|r| r as usize)
    }

    pub fn write(fd: RawFd, buf: &[u8]) -> Result<usize, std::io::Error> {
        let res = unsafe {
            libc::write(
                fd,
                buf.as_ptr() as *const libc::c_void,
                buf.len() as libc::size_t,
            )
        };

        check_err(res as _).map(|r| r as usize)
    }
}

#[cfg(target_os = "linux")]
pub use nix::Error;

#[cfg(unix)]
pub mod fcntl {
    use super::check_err;
    use std::os::unix::io::RawFd;

    pub type OFlag = libc::c_int;
    pub type FdFlag = libc::c_int;

    #[allow(non_camel_case_types)]
    #[allow(dead_code)]
    /// Arguments passed to `fcntl`.
    pub enum FcntlArg {
        F_GETFL,
        F_SETFL(OFlag),
        F_SETFD(FdFlag),
    }

    /// Thin wrapper around `libc::fcntl`.
    ///
    /// See [`fcntl(2)`](http://man7.org/linux/man-pages/man2/fcntl.2.html) for details.
    pub fn fcntl(fd: RawFd, arg: FcntlArg) -> Result<libc::c_int, std::io::Error> {
        let res = unsafe {
            match arg {
                FcntlArg::F_GETFL => libc::fcntl(fd, libc::F_GETFL),
                FcntlArg::F_SETFL(flag) => libc::fcntl(fd, libc::F_SETFL, flag),
                FcntlArg::F_SETFD(flag) => libc::fcntl(fd, libc::F_SETFD, flag),
            }
        };
        check_err(res)
    }
}

fn check_err(res: libc::c_int) -> Result<libc::c_int, std::io::Error> {
    if res == -1 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(res)
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
/// Kqueue.
pub mod event {
    use super::check_err;
    use std::os::unix::io::RawFd;

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd"
    ))]
    #[allow(non_camel_case_types)]
    type type_of_nchanges = libc::c_int;
    #[cfg(target_os = "netbsd")]
    #[allow(non_camel_case_types)]
    type type_of_nchanges = libc::size_t;

    #[cfg(target_os = "netbsd")]
    #[allow(non_camel_case_types)]
    type type_of_event_filter = u32;
    #[cfg(not(target_os = "netbsd"))]
    #[allow(non_camel_case_types)]
    type type_of_event_filter = i16;

    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "ios",
        target_os = "macos",
        target_os = "openbsd"
    ))]
    #[allow(non_camel_case_types)]
    type type_of_udata = *mut libc::c_void;
    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "ios",
        target_os = "macos"
    ))]
    #[allow(non_camel_case_types)]
    type type_of_data = libc::intptr_t;
    #[cfg(any(target_os = "netbsd"))]
    #[allow(non_camel_case_types)]
    type type_of_udata = libc::intptr_t;
    #[cfg(any(target_os = "netbsd", target_os = "openbsd"))]
    #[allow(non_camel_case_types)]
    type type_of_data = libc::int64_t;

    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    #[repr(C)]
    pub struct KEvent(libc::kevent);

    unsafe impl Send for KEvent {}

    impl KEvent {
        pub fn new(
            ident: libc::uintptr_t,
            filter: EventFilter,
            flags: EventFlag,
            fflags: FilterFlag,
            data: libc::intptr_t,
            udata: libc::intptr_t,
        ) -> KEvent {
            KEvent(libc::kevent {
                ident,
                filter: filter as type_of_event_filter,
                flags,
                fflags,
                data: data as type_of_data,
                udata: udata as type_of_udata,
            })
        }

        pub fn filter(&self) -> EventFilter {
            unsafe { std::mem::transmute(self.0.filter as type_of_event_filter) }
        }

        pub fn flags(&self) -> EventFlag {
            self.0.flags
        }

        pub fn data(&self) -> libc::intptr_t {
            self.0.data as libc::intptr_t
        }

        pub fn udata(&self) -> libc::intptr_t {
            self.0.udata as libc::intptr_t
        }
    }

    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "ios",
        target_os = "macos",
        target_os = "openbsd"
    ))]
    pub type EventFlag = u16;
    #[cfg(any(target_os = "netbsd"))]
    pub type EventFlag = u32;

    pub type FilterFlag = u32;

    #[cfg(target_os = "netbsd")]
    pub type EventFilter = u32;
    #[cfg(not(target_os = "netbsd"))]
    pub type EventFilter = i16;

    pub fn kqueue() -> Result<RawFd, std::io::Error> {
        let res = unsafe { libc::kqueue() };

        check_err(res)
    }

    pub fn kevent_ts(
        kq: RawFd,
        changelist: &[KEvent],
        eventlist: &mut [KEvent],
        timeout_opt: Option<libc::timespec>,
    ) -> Result<usize, std::io::Error> {
        let res = unsafe {
            libc::kevent(
                kq,
                changelist.as_ptr() as *const libc::kevent,
                changelist.len() as type_of_nchanges,
                eventlist.as_mut_ptr() as *mut libc::kevent,
                eventlist.len() as type_of_nchanges,
                if let Some(ref timeout) = timeout_opt {
                    timeout as *const libc::timespec
                } else {
                    std::ptr::null()
                },
            )
        };

        check_err(res).map(|r| r as usize)
    }
}

#[cfg(unix)]
pub use libc;

#[cfg(any(target_os = "linux", target_os = "android", target_os = "illumos"))]
/// Epoll.
pub mod epoll {
    pub use nix::sys::epoll::{
        epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
    };
}
