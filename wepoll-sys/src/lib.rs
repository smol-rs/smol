#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;
    use std::net::UdpSocket;
    use std::os::windows::io::AsRawSocket;

    #[test]
    fn test_poll() {
        unsafe {
            let socket = UdpSocket::bind("0.0.0.0:0")
                .expect("Failed to bind the UDP socket");

            let epoll = epoll_create(1);

            if epoll.is_null() {
                panic!("epoll_create(1) failed");
            }

            let mut event = epoll_event {
                events: EPOLLOUT | EPOLLONESHOT,
                data: epoll_data { u64: 42 },
            };

            epoll_ctl(
                epoll,
                EPOLL_CTL_ADD as i32,
                socket.as_raw_socket() as usize,
                &mut event as *mut _,
            );

            let mut events: [epoll_event; 1] =
                mem::MaybeUninit::uninit().assume_init();
            let received = epoll_wait(epoll, events.as_mut_ptr(), 1, -1);

            epoll_close(epoll);

            assert_eq!(received, 1);
            assert_eq!(events[0].data.u64, 42);
        }
    }
}
