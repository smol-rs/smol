#[cfg(unix)]
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

    #[derive(Clone, Copy)]
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

#[cfg(any(target_os = "linux", target_os = "android", target_os = "illumos"))]
/// Epoll.
pub mod epoll {
    use super::check_err;
    use std::os::unix::io::RawFd;

    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    #[repr(i32)]
    pub enum EpollOp {
        EpollCtlAdd = libc::EPOLL_CTL_ADD,
        EpollCtlDel = libc::EPOLL_CTL_DEL,
        EpollCtlMod = libc::EPOLL_CTL_MOD,
    }

    pub type EpollFlags = libc::c_int;

    pub fn epoll_create1() -> Result<RawFd, std::io::Error> {
        // According to libuv, `EPOLL_CLOEXEC` is not defined on Android API < 21.
        // But `EPOLL_CLOEXEC` is an alias for `O_CLOEXEC` on that platform, so we use it instead.
        #[cfg(target_os = "android")]
        const CLOEXEC: libc::c_int = libc::O_CLOEXEC;
        #[cfg(not(target_os = "android"))]
        const CLOEXEC: libc::c_int = libc::EPOLL_CLOEXEC;

        let fd = unsafe {
            // Check if the `epoll_create1` symbol is available on this platform.
            let ptr = libc::dlsym(
                libc::RTLD_DEFAULT,
                "epoll_create1\0".as_ptr() as *const libc::c_char,
            );

            if ptr.is_null() {
                // If not, use `epoll_create` and manually set `CLOEXEC`.
                let fd = check_err(libc::epoll_create(1024))?;
                let flags = libc::fcntl(fd, libc::F_GETFD);
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                fd
            } else {
                // Use `epoll_create1` with `CLOEXEC`.
                let epoll_create1 = std::mem::transmute::<
                    *mut libc::c_void,
                    unsafe extern "C" fn(libc::c_int) -> libc::c_int,
                >(ptr);
                check_err(epoll_create1(CLOEXEC))?
            }
        };

        Ok(fd)
    }

    pub fn epoll_ctl<'a, T>(
        epfd: RawFd,
        op: EpollOp,
        fd: RawFd,
        event: T,
    ) -> Result<(), std::io::Error>
    where
        T: Into<Option<&'a mut EpollEvent>>,
    {
        let mut event: Option<&mut EpollEvent> = event.into();
        if event.is_none() && op != EpollOp::EpollCtlDel {
            Err(std::io::Error::from_raw_os_error(libc::EINVAL))
        } else {
            let res = unsafe {
                if let Some(ref mut event) = event {
                    libc::epoll_ctl(epfd, op as libc::c_int, fd, &mut event.event)
                } else {
                    libc::epoll_ctl(epfd, op as libc::c_int, fd, std::ptr::null_mut())
                }
            };
            check_err(res).map(drop)
        }
    }

    pub fn epoll_wait(
        epfd: RawFd,
        events: &mut [EpollEvent],
        timeout_ms: isize,
    ) -> Result<usize, std::io::Error> {
        let res = unsafe {
            libc::epoll_wait(
                epfd,
                events.as_mut_ptr() as *mut libc::epoll_event,
                events.len() as libc::c_int,
                timeout_ms as libc::c_int,
            )
        };

        check_err(res).map(|r| r as usize)
    }

    #[derive(Clone, Copy)]
    #[repr(transparent)]
    pub struct EpollEvent {
        event: libc::epoll_event,
    }

    impl EpollEvent {
        pub fn new(events: EpollFlags, data: u64) -> Self {
            EpollEvent {
                event: libc::epoll_event {
                    events: events as u32,
                    u64: data,
                },
            }
        }

        pub fn empty() -> Self {
            unsafe { std::mem::zeroed::<EpollEvent>() }
        }

        pub fn events(&self) -> EpollFlags {
            self.event.events as libc::c_int
        }

        pub fn data(&self) -> u64 {
            self.event.u64
        }
    }
}
