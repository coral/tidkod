//! Bounded request matching shared by all transport adapters.
use crate::{Error, clock::Exchange};
use crate::{decode_probe, encode, wire};

#[derive(Clone)]
pub struct Probes {
    outstanding: [(u64, u64); 128],
    sequence: u64,
    publication: [Option<u64>; 128],
    last_publication: Option<u64>,
}
impl Default for Probes {
    fn default() -> Self {
        Self {
            outstanding: [(0, 0); 128],
            sequence: 0,
            publication: [None; 128],
            last_publication: None,
        }
    }
}
impl Probes {
    pub fn request(&mut self, now: u64) -> Result<Vec<u8>, Error> {
        if now > i64::MAX as u64 {
            return Err(Error::Invalid("timestamp"));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(Error::Invalid("probe sequence exhausted"))?;
        let p = wire::Probe {
            version: 1,
            sequence: self.sequence,
            t1: now,
            t2: 0,
            t3: 0,
        };
        let bytes = encode(&p)?;
        self.publication[self.sequence as usize % 128] = None;
        self.outstanding[self.sequence as usize % 128] = (self.sequence, now);
        Ok(bytes)
    }
    /// Call immediately after synchronous publication returns, in the same clock domain.
    /// When publication includes the socket send, the matched exchange measures from this
    /// instant: a send call can block until the datagram departs (for example behind a
    /// network content filter), and that local time is not path delay. The wire t1 remains
    /// only the request identifier.
    pub fn publication_finished(&mut self, now: u64) {
        let i = self.sequence as usize % 128;
        if self.outstanding[i].0 == self.sequence && self.sequence != 0 {
            self.publication[i] = now.checked_sub(self.outstanding[i].1);
        }
    }
    pub fn last_publication_ns(&self) -> Option<u64> {
        self.last_publication
    }
    pub fn reply(&mut self, bytes: &[u8], now: u64) -> Result<Option<Exchange>, Error> {
        self.last_publication = None;
        let p = decode_probe(bytes)?;
        let slot = &mut self.outstanding[p.sequence as usize % 128];
        if p.sequence == 0 || *slot != (p.sequence, p.t1) || p.t2 == 0 {
            return Ok(None);
        }
        if now < p.t1 || p.t3 - p.t2 > now - p.t1 {
            return Ok(None);
        }
        if now - p.t1 > crate::clock::MAX_EXCHANGE_NS {
            *slot = (0, 0);
            return Ok(None);
        }
        *slot = (0, 0);
        self.last_publication = self.publication[p.sequence as usize % 128];
        let sent = self.last_publication.map_or(p.t1, |d| p.t1 + d);
        Ok(Some(Exchange {
            // Never let a late publication timestamp produce a negative path delay.
            t1: if sent <= now && p.t3 - p.t2 <= now - sent {
                sent
            } else {
                p.t1
            },
            t2: p.t2,
            t3: p.t3,
            t4: now,
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_matching_rejects_replays_and_evicted_probes() {
        let mut p = Probes::default();
        let mut first = decode_probe(&p.request(1).unwrap()).unwrap();
        first.t2 = 2;
        first.t3 = 3;
        let reply = encode(&first).unwrap();
        assert!(p.reply(&reply, 4).unwrap().is_some());
        assert!(p.reply(&reply, 4).unwrap().is_none());
        let mut evicted = decode_probe(&p.request(2).unwrap()).unwrap();
        evicted.t2 = 3;
        evicted.t3 = 4;
        for n in 3..132 {
            p.request(n).unwrap();
        }
        assert!(p.reply(&encode(&evicted).unwrap(), 140).unwrap().is_none());
        assert!(p.request(u64::MAX).is_err());
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;
    #[test]
    fn invalid_echo_does_not_consume_request_and_stalled_reply_expires() {
        let mut probes = Probes::default();
        let mut p = decode_probe(&probes.request(1_000_000_000).unwrap()).unwrap();
        p.t2 = 2_000_000_000;
        p.t3 = 2_100_000_000;
        assert!(
            probes
                .reply(&encode(&p).unwrap(), 1_010_000_000)
                .unwrap()
                .is_none()
        );
        p.t3 = p.t2;
        assert!(
            probes
                .reply(&encode(&p).unwrap(), 1_010_000_000)
                .unwrap()
                .is_some()
        );
        let mut p = decode_probe(&probes.request(2_000_000_000).unwrap()).unwrap();
        p.t2 = 3_000_000_000;
        p.t3 = 33_000_000_000;
        assert!(
            probes
                .reply(&encode(&p).unwrap(), 32_010_000_000)
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn publication_is_correlated_to_request_not_reply_order() {
        let mut p = Probes::default();
        let mut a = decode_probe(&p.request(100).unwrap()).unwrap();
        p.publication_finished(110);
        let mut b = decode_probe(&p.request(200).unwrap()).unwrap();
        p.publication_finished(230);
        a.t2 = 1000;
        a.t3 = 1010;
        b.t2 = 1100;
        b.t3 = 1110;
        p.reply(&encode(&b).unwrap(), 300).unwrap().unwrap();
        assert_eq!(p.last_publication_ns(), Some(30));
        p.reply(&encode(&a).unwrap(), 400).unwrap().unwrap();
        assert_eq!(p.last_publication_ns(), Some(10));
        assert!(p.reply(&encode(&a).unwrap(), 500).unwrap().is_none());
        assert_eq!(p.last_publication_ns(), None);
    }
}
