#[cfg(target_os = "linux")]
pub mod linux {
    pub use nix::sys::eventfd::{eventfd, EfdFlags};

    pub mod unistd {
        pub use nix::unistd::{close, dup, read, write};
    }

    pub use nix::Error;
}

#[cfg(unix)]
pub mod fcntl {
    pub use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
pub mod event {
    pub use nix::sys::event::{kevent_ts, kqueue, EventFilter, EventFlag, FilterFlag, KEvent};
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
pub mod errno {
    pub use nix::errno::Errno;
}

#[cfg(unix)]
pub use nix::libc;

#[cfg(any(target_os = "linux", target_os = "android", target_os = "illumos"))]
pub mod epoll {
    pub use nix::sys::epoll::{
        epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
    };
}
