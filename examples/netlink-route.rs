use std::io;

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("This example works only on Linux!");
}

#[cfg(target_os = "linux")]
fn main() -> io::Result<()> {
    use netlink_packet_core::{NetlinkHeader, NetlinkMessage, NLM_F_DUMP, NLM_F_REQUEST};
    use netlink_packet_route::{rtnl::link::nlas::Nla, LinkMessage, NetlinkPayload, RtnlMessage};
    use netlink_sys::{Protocol, Socket, SocketAddr};
    use smol::Async;
    use std::process;

    fn align(len: usize) -> usize {
        const RTA_ALIGNTO: usize = 4;

        ((len)+RTA_ALIGNTO-1) & !(RTA_ALIGNTO-1)
    }

    smol::run(async {
        let mut socket = Socket::new(Protocol::Route)?;
        socket.bind(&SocketAddr::new(process::id(), 1))?;
        let mut socket = Async::new(socket)?;

        let mut packet = NetlinkMessage {
            header: NetlinkHeader {
                sequence_number: 1,
                flags: NLM_F_DUMP | NLM_F_REQUEST,
                ..Default::default()
            },
            payload: RtnlMessage::GetLink(LinkMessage::default()).into(),
        };
        packet.finalize();

        let mut buf = vec![0; packet.header.length as usize];
        packet.serialize(&mut buf[..]);
        socket.write_with_mut(|sock| sock.send(&buf, 0)).await?;

        'out: loop {
            let mut buf = [0; 4096];
            let mut cursor = 0;

            let (count, _) = socket
                .read_with_mut(|sock| sock.recv_from(&mut buf, 0))
                .await?;
            loop {
                if cursor >= count {
                    break;
                }
                let msg_len = {
                    let mut len_buf = [0; 4];
                    len_buf.copy_from_slice(&buf[cursor..cursor + 4]);
                    u32::from_ne_bytes(len_buf) as usize
                };
                let msg_len = align(msg_len);
                let reply = NetlinkMessage::<RtnlMessage>::deserialize(&buf[cursor..cursor + msg_len])
                    .expect("Failed to deserialize message");
                match reply.payload {
                    NetlinkPayload::InnerMessage(RtnlMessage::NewLink(link)) => {
                        let name = link
                            .nlas
                            .iter()
                            .filter_map(|nla| match nla {
                                Nla::IfName(name) => Some(name.clone()),
                                _ => None,
                            })
                            .next()
                            .unwrap();
                        println!("{}: {}", link.header.index, name);
                    }
                    _ => {
                        break 'out;
                    }
                }
                cursor += msg_len;
            }
        }

        Ok(())
    })
}
