// A raw AF_PACKET SOCK_DGRAM tap (needs CAP_NET_RAW) that sees every IPv4
// packet entering or leaving any interface, loopback included, without
// touching it. chat_capture.rs reads Rocket League's LAN chat out of it.
// (It used to also catch RL's LAN discovery broadcast for the old tailnet
// beacon relay; the tailcat sidecar shares RL's discovery port instead.)

use std::net::{Ipv4Addr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

// linux/if_ether.h, linux/if_packet.h
const ETH_P_ALL: u16 = 0x0003;
const ETH_P_IP: u16 = 0x0800;
const PACKET_OUTGOING: u8 = 4;

pub(super) struct RawCapture {
    fd: OwnedFd,
    pub(super) stop: AtomicBool,
}

impl RawCapture {
    pub(super) fn open() -> Result<Self, String> {
        let fd = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                // the kernel hands outgoing packets only to ETH_P_ALL
                // taps; a socket bound to ETH_P_IP only sees received ones
                i32::from(ETH_P_ALL.to_be()),
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(libc::EPERM) {
                format!(
                    "Hebnix needs the CAP_NET_RAW permission for this: run `sudo setcap {} <path to hebnix binary>` and restart Hebnix",
                    super::caps::GRANTED_CAPS
                )
            } else {
                error.to_string()
            });
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // wake up regularly so stop() is noticed without closing the fd
        // from under a blocked recvfrom
        let timeout = libc::timeval {
            tv_sec: 0,
            tv_usec: 200_000,
        };
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &timeout as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
        }
        Ok(Self {
            fd,
            stop: AtomicBool::new(false),
        })
    }

    /// the next IPv4 packet this machine sent, or with `outgoing_only` false
    /// received ones too (the chat capture needs both directions). Ok(None)
    /// on a timeout or a packet that doesn't match.
    pub(super) fn recv_filtered(
        &self,
        buffer: &mut [u8],
        outgoing_only: bool,
    ) -> std::io::Result<Option<usize>> {
        let mut address: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        let mut address_len = std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
        let len = unsafe {
            libc::recvfrom(
                self.fd.as_raw_fd(),
                buffer.as_mut_ptr() as *mut libc::c_void,
                buffer.len(),
                0,
                &mut address as *mut _ as *mut libc::sockaddr,
                &mut address_len,
            )
        };
        if len < 0 {
            let error = std::io::Error::last_os_error();
            return match error.kind() {
                std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted => Ok(None),
                _ => Err(error),
            };
        }
        let wanted_direction = !outgoing_only || address.sll_pkttype == PACKET_OUTGOING;
        // loopback shows every packet twice (once out, once in)
        let loopback_copy = address.sll_ifindex == 1 && address.sll_pkttype == PACKET_OUTGOING;
        let ipv4 = u16::from_be(address.sll_protocol) == ETH_P_IP;
        Ok((ipv4 && wanted_direction && (outgoing_only || !loopback_copy)).then_some(len as usize))
    }

    pub(super) fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// pulls the UDP payload, source address, and destination port out of a raw
/// IP packet - the SOCK_DGRAM packet socket hands us the packet starting at
/// the IP header (the link-layer header is already stripped), so this is
/// the same job a normal socket's recv_from would otherwise do
pub(super) fn parse_udp_payload(ip: &[u8]) -> Option<(Vec<u8>, SocketAddr, u16)> {
    const UDP_PROTOCOL: u8 = 17;

    if ip.len() < 20 {
        return None;
    }
    let version = ip[0] >> 4;
    if version != 4 {
        return None;
    }
    let header_len = (ip[0] & 0x0F) as usize * 4;
    if header_len < 20 || ip.len() < header_len + 8 {
        return None;
    }
    if ip[9] != UDP_PROTOCOL {
        return None;
    }
    let source_ip = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);

    let udp = &ip[header_len..];
    let source_port = u16::from_be_bytes([udp[0], udp[1]]);
    let destination_port = u16::from_be_bytes([udp[2], udp[3]]);
    let udp_length = u16::from_be_bytes([udp[4], udp[5]]) as usize;
    if udp_length < 8 || udp.len() < 8 {
        return None;
    }
    let payload_end = udp_length.min(udp.len());
    let payload = udp[8..payload_end].to_vec();

    Some((
        payload,
        SocketAddr::new(source_ip.into(), source_port),
        destination_port,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_udp_payload_out_of_a_raw_ip_packet() {
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45; // version 4, header length 20
        ip[9] = 17; // UDP
        ip[12..16].copy_from_slice(&[10, 242, 77, 1]);
        let mut udp = vec![0u8; 8];
        udp[0..2].copy_from_slice(&14001u16.to_be_bytes());
        udp[2..4].copy_from_slice(&14001u16.to_be_bytes());
        let payload = b"hello beacon";
        udp[4..6].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        udp.extend_from_slice(payload);
        ip.extend_from_slice(&udp);

        let (parsed_payload, source, destination_port) =
            parse_udp_payload(&ip).expect("should parse a well-formed UDP packet");
        assert_eq!(parsed_payload, payload);
        assert_eq!(source, "10.242.77.1:14001".parse().unwrap());
        assert_eq!(destination_port, 14001);
    }

    #[test]
    fn ignores_non_udp_and_truncated_packets() {
        assert!(parse_udp_payload(&[0u8; 10]).is_none()); // too short for an IP header
        let mut non_udp = vec![0u8; 20];
        non_udp[0] = 0x45;
        non_udp[9] = 6; // TCP, not UDP
        assert!(parse_udp_payload(&non_udp).is_none());
    }
}
