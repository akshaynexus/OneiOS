// SPDX-License-Identifier: MIT
//! The phone's own uplink as iOS's Wi-Fi (xnuhost `wifi=cell`, docs/hardware.md 3.4b): a made-up station that is
//! already associated with one open network, whose frames go to a tap on One UI's kernel and are routed and NATed by
//! `net/oneios-net.sh`. That makes iOS's data work with no Wi-Fi network — including over the phone's mobile data —
//! without a baseband, which an Android modem cannot give the guest. The tap needs `/dev/tun` and root.
//!
//! No 802.11 frame crosses: the guest's driver sees a joined network (`OW_STATE`), configd leases an address from the
//! bridge's own DHCP answer, and every frame that is not a station message goes to the tap. The network is open
//! because the tap has no 802.11 security; its name is what iOS shows in Settings.

use super::frames::ethertype;
use super::proto::*;
use super::station::{Bss, Link, Radio, Station};
use super::CONTROL_TYPE;
use crate::br_info;
use crate::net::{Net, NetBackend, NetConfig, NetOpen, RxFn};
use std::sync::{Arc, Mutex, PoisonError};

/// The joined network's name, as iOS shows it.
const SSID: &[u8] = b"OneiOS uplink";
const BSSID: [u8; 6] = [0x02, 0x4f, 0x43, 0x45, 0x4c, 0x4c];

/// The network's record and elements (an open 2.4 GHz network with the basic rates).
fn record() -> (OwBss, Vec<u8>) {
    let mut ssid = [0u8; 32];
    ssid[..SSID.len()].copy_from_slice(SSID);
    let mut ies = vec![0, SSID.len() as u8];
    ies.extend_from_slice(SSID);
    ies.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x24, 0x30, 0x48, 0x6c]);
    let bss = OwBss {
        bssid: BSSID,
        capability: 0x0401, // ESS, privacy off
        freq_mhz: 2412,
        signal_dbm: -50,
        beacon_interval: 100,
        age_ms: 0,
        ie_length: ies.len() as u16,
        ssid_length: SSID.len() as u8,
        ssid,
        pad: 0,
    };
    (bss, ies)
}

/// The made-up radio: associated from the start, and associated again if the guest leaves and joins.
struct CellRadio {
    joined: Mutex<bool>,
}

impl CellRadio {
    fn new() -> CellRadio {
        CellRadio { joined: Mutex::new(true) }
    }
}

impl Radio for CellRadio {
    fn scan(&self, st: &Station) -> Result<(), i32> {
        let (bss, ies) = record();
        st.scan_add(&bss, &ies);
        st.scan_done(0);
        Ok(())
    }

    fn join(&self, st: &Station, j: &OwJoin, _bss: Option<&Bss>) -> Result<(), i32> {
        // The only network a scan can show is this one; another name is not there (as with no radio).
        if j.ssid() != SSID {
            st.join_done(OW_JOIN_NO_NETWORK, 0);
            return Ok(());
        }
        *self.joined.lock().unwrap_or_else(PoisonError::into_inner) = true;
        let (bss, ies) = record();
        st.assume_joined(Bss { bss, ies });
        st.join_done(OW_JOIN_OK, 0);
        Ok(())
    }

    fn leave(&self, _st: &Station) -> Result<(), i32> {
        *self.joined.lock().unwrap_or_else(PoisonError::into_inner) = false;
        Ok(())
    }

    fn link(&self) -> Result<Link, i32> {
        if !*self.joined.lock().unwrap_or_else(PoisonError::into_inner) {
            return Ok(Link { up: false, reason: 3, ..Default::default() });
        }
        let (bss, _) = record();
        Ok(Link {
            up: true,
            rssi_dbm: bss.signal_dbm.into(),
            noise_dbm: -95,
            tx_rate_mbps: 144,
            freq_mhz: bss.freq_mhz,
            reason: 0,
            bssid: bss.bssid,
        })
    }

    fn channels(&self, max: usize) -> Option<Result<Vec<OwChannel>, i32>> {
        // What a 2.4 GHz network uses at most; the guest only needs a channel to display.
        Some(Ok((1..=13u16).take(max).map(|c| OwChannel { freq_mhz: 2407 + 5 * c, ..Default::default() }).collect()))
    }
}

/// The guest's frames: the station's messages, or a data frame for the tap.
struct CellNet {
    station: Arc<Station>,
    net: Net,
}

impl NetBackend for CellNet {
    fn send(&self, frame: &[u8]) -> Result<(), i32> {
        if ethertype(frame) == CONTROL_TYPE {
            self.station.message(frame);
            Ok(())
        } else {
            self.net.send(frame) // DHCP is answered here; the rest goes to the tap
        }
    }
}

/// The made-up associated station over the tap, as the virtio-net device's back end. `mac`: the guest's en0 address
/// (the station's and the tap's).
pub fn prepare(mac: [u8; 6]) -> NetOpen {
    Box::new(move |c: &NetConfig, guest: RxFn| {
        let to_guest = Arc::clone(&guest);
        let station = Station::new(
            Box::new(CellRadio::new()),
            mac,
            None,
            Box::new(move |f: &[u8]| {
                let _ = to_guest(f);
            }),
        );
        let (bss, ies) = record();
        station.assume_joined(Bss { bss, ies });
        let net = Net::open_tap(c, guest)?;
        br_info!("wifi: cell: an open network over the phone's uplink (the tap), already joined; no 802.11");
        Some(Box::new(CellNet { station: Arc::new(station), net }) as Box<dyn NetBackend>)
    })
}

#[cfg(test)]
mod tests {
    use super::super::frames::tests::MAC;
    use super::super::station::guest_frame;
    use super::*;
    use crate::proto::wire::Wire;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::mpsc::{channel, Receiver};
    use std::time::Duration;

    fn host_message(s: &Mutex<Vec<Vec<u8>>>, i: usize) -> (u16, Vec<u8>) {
        let s = s.lock().unwrap();
        let Some(f) = s.get(i) else { return (0, Vec::new()) };
        match message(f) {
            Some((h, p)) if f[..6] == MAC && f[6..12] == OW_ADDRESS => (h.kind, p.to_vec()),
            _ => (0, Vec::new()),
        }
    }

    fn station<F: Fn(&[u8]) + Send + Sync + 'static>(guest: F) -> Station {
        let station = Station::new(Box::new(CellRadio::new()), MAC, None, Box::new(guest));
        let (bss, ies) = record();
        station.assume_joined(Bss { bss, ies });
        station
    }

    fn socketpair() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        // SAFETY: a valid array of two descriptors for socketpair to fill.
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) }, 0);
        // SAFETY: socketpair gave two descriptors that nothing else owns.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    /// A DHCP DISCOVER from the guest (what configd sends first on en0).
    fn dhcp_discover(mac: [u8; 6]) -> Vec<u8> {
        let mut f = vec![0u8; 14 + 20 + 8 + 240 + 4];
        f[..6].fill(0xff);
        f[6..12].copy_from_slice(&mac);
        f[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
        {
            let ip = &mut f[14..34];
            ip[0] = 0x45;
            ip[8] = 64;
            ip[9] = 17;
            ip[16..20].copy_from_slice(&[0xff; 4]);
        }
        {
            let udp = &mut f[34..42];
            udp[0..2].copy_from_slice(&68u16.to_be_bytes());
            udp[2..4].copy_from_slice(&67u16.to_be_bytes());
            udp[4..6].copy_from_slice(&((8 + 240 + 4) as u16).to_be_bytes());
        }
        {
            let b = &mut f[42..];
            b[0] = 1;
            b[1] = 1;
            b[2] = 6;
            b[4..8].copy_from_slice(&0x1234_5678u32.to_be_bytes());
            b[28..34].copy_from_slice(&mac);
            b[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
            b[240..244].copy_from_slice(&[53, 1, 1, 255]);
        }
        f
    }

    fn read_frame(fd: &OwnedFd) -> Vec<u8> {
        use std::os::fd::AsRawFd;
        let mut b = [0u8; 2048];
        // SAFETY: a writable buffer of its length.
        let n = unsafe { libc::read(fd.as_raw_fd(), b.as_mut_ptr().cast(), b.len()) };
        assert!(n > 0, "the tap side got no frame");
        b[..n as usize].to_vec()
    }

    fn write_frame(fd: &OwnedFd, frame: &[u8]) {
        use std::os::fd::AsRawFd;
        // SAFETY: a readable buffer of its length.
        let n = unsafe { libc::write(fd.as_raw_fd(), frame.as_ptr().cast(), frame.len()) };
        assert_eq!(n, frame.len() as isize, "the tap side took no frame");
    }

    #[test]
    fn uplink_station() {
        let s: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let t = s.clone();
        let station = station(move |f: &[u8]| t.lock().unwrap().push(f.to_vec()));
        let send = |kind, payload: &[u8]| station.message(&guest_frame(&MAC, kind, payload));
        // The first message: the link is already up on the made-up network, and its record comes with it.
        send(OW_HELLO, &OwHello { version: OW_VERSION }.to_bytes());
        let (kind, p) = host_message(&s, 0);
        assert_eq!(kind, OW_STATE);
        let st = OwState::parse(&p).unwrap();
        assert_eq!(st.link, OW_LINK_UP);
        assert_eq!(st.bss.ssid(), SSID);
        assert_eq!(st.bss.bssid, BSSID);
        assert_eq!(st.rssi_dbm, -50);
        // Its channels follow, then a scan finds exactly it.
        s.lock().unwrap().clear();
        send(OW_SCAN, &[]);
        assert!(s.lock().unwrap().len() == 2);
        let (kind, p) = host_message(&s, 0);
        assert_eq!(kind, OW_BSS);
        let b = OwBss::parse(&p).unwrap();
        assert_eq!(b.ssid(), SSID);
        assert_eq!(b.capability & 0x10, 0); // open
        assert_eq!(host_message(&s, 1).0, OW_SCAN_DONE);
        // A join of another name fails; the made-up one joins.
        s.lock().unwrap().clear();
        send(OW_JOIN, &OwJoin::new(b"somewhere else", OW_SECURITY_OPEN, b"", false).to_bytes());
        assert_eq!(host_message(&s, 0).0, OW_JOIN_RESULT);
        assert_eq!(OwJoinResult::parse(&host_message(&s, 0).1).unwrap().status, OW_JOIN_NO_NETWORK);
        s.lock().unwrap().clear();
        send(OW_JOIN, &OwJoin::new(SSID, OW_SECURITY_OPEN, b"", false).to_bytes());
        assert_eq!(OwJoinResult::parse(&host_message(&s, 1).1).unwrap().status, OW_JOIN_OK);
        // Leaving says the link is down; the radio says it too.
        s.lock().unwrap().clear();
        send(OW_LEAVE, &[]);
        assert_eq!(host_message(&s, 0).0, OW_STATE);
        assert_eq!(OwState::parse(&host_message(&s, 0).1).unwrap().link, OW_LINK_IDLE);
        assert!(!station.radio().link().unwrap().up);
    }

    // The data path over a socketpair instead of the tap: a DHCP discover is answered by the bridge itself, a data
    // frame reaches the tap, and a frame from the tap reaches the guest.
    #[test]
    fn uplink_data_path() {
        let (near, far) = socketpair();
        let (tx, answers): (std::sync::mpsc::Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel();
        let rx: RxFn = Arc::new(move |f: &[u8]| {
            let _ = tx.send(f.to_vec());
            true
        });
        let c = NetConfig { guest_mac: MAC, ..NetConfig::default() };
        let net = Net::open_fd(&c, near, Arc::clone(&rx)).unwrap();
        let station = {
            let rx = Arc::clone(&rx);
            station(move |f: &[u8]| {
                let _ = rx(f);
            })
        };
        let backend = CellNet { station: Arc::new(station), net };
        // A DHCP DISCOVER for the guest is answered with an offer to its address, without touching the tap.
        backend.send(&dhcp_discover(MAC)).unwrap();
        let offer = answers.recv_timeout(Duration::from_secs(1)).expect("no DHCP answer");
        assert_eq!(&offer[12..14], &0x0800u16.to_be_bytes());
        assert_eq!(&offer[58..62], &c.guest_ip.to_be_bytes()); // yiaddr
                                                               // A data frame goes to the tap as it is.
        let data = [0xffu8; 60];
        backend.send(&data).unwrap();
        assert_eq!(read_frame(&far), data);
        // A frame from the tap reaches the guest.
        write_frame(&far, &[0x01; 42]);
        assert_eq!(answers.recv_timeout(Duration::from_secs(5)).expect("the guest got no tap frame"), vec![0x01; 42]);
    }
}
