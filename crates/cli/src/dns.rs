//! Just enough of a DNS client to ask Tailscale's MagicDNS resolver for a tailnet name's
//! addresses (A and AAAA over UDP). The static Linux binary reads /etc/resolv.conf itself, so it
//! misses split DNS that systemd-resolved does for glibc programs, for example when another VPN
//! owns resolv.conf.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::Duration;

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;

/// The addresses `server` gives for `host`, IPv4 first.
pub fn lookup(server: SocketAddr, host: &str, timeout: Duration) -> io::Result<Vec<IpAddr>> {
    let bind: SocketAddr = if server.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(bind)?;
    socket.connect(server)?;
    socket.set_read_timeout(Some(timeout))?;
    let mut addrs = Vec::new();
    let mut last_err = None;
    for qtype in [TYPE_A, TYPE_AAAA] {
        match ask(&socket, host, qtype) {
            Ok(found) => addrs.extend(found),
            Err(e) => last_err = Some(e),
        }
    }
    match (addrs.is_empty(), last_err) {
        (false, _) => Ok(addrs),
        (true, Some(e)) => Err(e),
        (true, None) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{server} has no address for {host}"),
        )),
    }
}

fn ask(socket: &UdpSocket, host: &str, qtype: u16) -> io::Result<Vec<IpAddr>> {
    let mut id = [0u8; 2];
    getrandom::fill(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
    let id = u16::from_be_bytes(id);
    socket.send(&query(id, host, qtype)?)?;
    let mut buf = [0u8; 1500];
    // Skip stray answers to earlier queries.
    for _ in 0..4 {
        let n = socket.recv(&mut buf).map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                io::Error::new(io::ErrorKind::TimedOut, "no answer")
            }
            _ => e,
        })?;
        if let Some(addrs) = answer(id, &buf[..n])? {
            return Ok(addrs);
        }
    }
    Err(io::Error::other("no matching answer"))
}

/// A recursive query for `host`'s records of `qtype`.
pub fn query(id: u16, host: &str, qtype: u16) -> io::Result<Vec<u8>> {
    let mut q = Vec::with_capacity(32 + host.len());
    q.extend(id.to_be_bytes());
    q.extend([0x01, 0x00]); // recursion desired
    q.extend([0, 1, 0, 0, 0, 0, 0, 0]); // one question
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{host} is not a valid host name"),
            ));
        }
        q.push(label.len() as u8);
        q.extend(label.as_bytes());
    }
    q.push(0);
    q.extend(qtype.to_be_bytes());
    q.extend(1u16.to_be_bytes()); // class IN
    Ok(q)
}

/// The A and AAAA records in a response to query `id`; None when the packet answers another one.
pub fn answer(id: u16, packet: &[u8]) -> io::Result<Option<Vec<IpAddr>>> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed DNS answer");
    let u16_at = |i: usize| -> io::Result<u16> {
        packet
            .get(i..i + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .ok_or_else(bad)
    };
    if packet.len() < 12 || u16_at(0)? != id || packet[2] & 0x80 == 0 {
        return Ok(None);
    }
    match packet[3] & 0x0f {
        0 => {}
        3 => return Ok(Some(Vec::new())), // no such name
        rcode => return Err(io::Error::other(format!("DNS error code {rcode}"))),
    }
    let (questions, answers) = (u16_at(4)?, u16_at(6)?);
    let mut i = 12;
    for _ in 0..questions {
        i = skip_name(packet, i).ok_or_else(bad)? + 4;
    }
    let mut addrs = Vec::new();
    for _ in 0..answers {
        i = skip_name(packet, i).ok_or_else(bad)?;
        let (rtype, len) = (u16_at(i)?, u16_at(i + 8)? as usize);
        let data = packet.get(i + 10..i + 10 + len).ok_or_else(bad)?;
        match (rtype, len) {
            (TYPE_A, 4) => addrs.push(IpAddr::from(<[u8; 4]>::try_from(data).unwrap())),
            (TYPE_AAAA, 16) => addrs.push(IpAddr::from(<[u8; 16]>::try_from(data).unwrap())),
            _ => {}
        }
        i += 10 + len;
    }
    Ok(Some(addrs))
}

/// The index after the (possibly compressed) name at `i`.
fn skip_name(packet: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let len = *packet.get(i)?;
        match len {
            0 => return Some(i + 1),
            l if l & 0xc0 == 0xc0 => return Some(i + 2),
            l => i += 1 + l as usize,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response to `q` with the given records.
    fn respond(q: &[u8], records: &[(u16, &[u8])]) -> Vec<u8> {
        let mut r = q.to_vec();
        r[2] = 0x81;
        r[3] = 0x80;
        r[6..8].copy_from_slice(&(records.len() as u16).to_be_bytes());
        for (rtype, data) in records {
            r.extend([0xc0, 12]);
            r.extend(rtype.to_be_bytes());
            r.extend([0, 1, 0, 0, 0, 60]);
            r.extend((data.len() as u16).to_be_bytes());
            r.extend(*data);
        }
        r
    }

    #[test]
    fn reads_a_and_aaaa_records() {
        let q = query(7, "keyko.example.ts.net.", TYPE_A).unwrap();
        assert_eq!(&q[12..19], b"\x05keyko\x07");
        let v6 = Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1);
        let r = respond(&q, &[(TYPE_A, &[100, 64, 0, 1]), (TYPE_AAAA, &v6.octets())]);
        assert_eq!(
            answer(7, &r).unwrap(),
            Some(vec![IpAddr::from([100, 64, 0, 1]), IpAddr::from(v6)])
        );
        assert_eq!(answer(8, &r).unwrap(), None);
        let mut nx = respond(&q, &[]);
        nx[3] = 0x83;
        assert_eq!(answer(7, &nx).unwrap(), Some(vec![]));
        assert!(answer(7, &r[..r.len() - 2]).is_err());
        assert!(query(1, "a..b", TYPE_A).is_err());
    }
}
