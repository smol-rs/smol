#[cfg(target_os = "linux")]
pub mod eventfd {
    pub use nix::sys::eventfd::{eventfd, EfdFlags};
}

#[cfg(target_os = "linux")]
pub mod unistd {
    pub use nix::unistd::{close, dup, read, write};
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
    pub use nix::sys::event::{kevent_ts, kqueue, EventFilter, EventFlag, FilterFlag, KEvent};
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
