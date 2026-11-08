use std::time::SystemTime;

use kcp::{get_conv, Kcp, KCP_OVERHEAD};
use tracing::{error, info, instrument, span, trace, warn, Level};

use crate::bytes_as_hex;

pub(crate) struct KcpSniffer {
    conv_id: u32,
    kcp: Kcp<Vec<u8>>,
    time_start: SystemTime,
    /// Segments of another conversation seen since this was last read.
    mismatches: u32,
    /// Whether it was looked at where in the conversation the first segment is.
    primed: bool,
}

impl KcpSniffer {
    #[instrument(skip(segment))]
    pub fn try_new(segment: &[u8]) -> Option<Self> {
        validate_kcp_segment(segment).map(Self::new).or_else(|| {
            error!("could not create new kcp instance");
            None
        })
    }

    #[instrument]
    fn new(conv_id: u32) -> Self {
        info!("new connection, created new kcp instance");

        KcpSniffer {
            conv_id,
            kcp: new_kcp(conv_id),
            time_start: SystemTime::now(),
            mismatches: 0,
            primed: false,
        }
    }

    pub fn conv_id(&self) -> u32 {
        self.conv_id
    }

    /// Returns how many segments of another conversation arrived since the
    /// last call.
    pub fn take_mismatches(&mut self) -> u32 {
        std::mem::take(&mut self.mismatches)
    }

    #[instrument(skip_all, fields(conv_id = self.conv_id, len = segments.len()))]
    pub fn receive_segments(&mut self, segments: &[u8]) -> Vec<Vec<u8>> {
        let Some(conv_id) = validate_kcp_segment(segments) else {
            return Vec::new();
        };

        trace!("message data: {}", bytes_as_hex(segments));

        if conv_id != self.conv_id {
            self.mismatches += 1;
            warn!(
                expected = self.conv_id,
                "packet did not belong to conversation"
            );
            return Vec::new();
        }

        // game uses special format which adds 4 bytes at index 4,
        // reprocess to discard bytes 4..8 of every segment
        let segments = reformat_kcp_segments(segments);

        // Joined in the middle of a conversation (the program was started while the game
        // was running): the first segment is not number 0, and KCP would wait for the
        // ones before it for good.  So it is told they came.
        // (A packet of only acknowledgements has no number to go by: the next is looked at.)
        if !self.primed
            && let Some(sn) = first_push_sn(&segments)
        {
            self.primed = true;
            if sn > RESYNC_FROM {
                self.skip_to(sn, conv_id);
            }
        }

        match self.kcp.input(&segments) {
            Ok(size) => trace!(size, "input successful"),
            Err(e) => warn!("could not input to kcp: {e}"),
        }

        let mut recv = Vec::new();
        while let Ok(size) = self.kcp.peeksize() {
            let span = span!(Level::TRACE, "receiving", size);
            let _enter = span.enter();

            let mut bytes = vec![0; size];

            match self.kcp.recv(&mut bytes) {
                Ok(_size) => {
                    recv.push(bytes);
                }
                Err(e) => {
                    warn!(%e, "could not receive kcp bytes");
                }
            }
        }

        if let Err(e) = self.kcp.update(self.clock()) {
            warn!(%e, "could not update kcp state");
        }

        recv
    }

    /// Tells KCP that the segments before number `sn` came (one byte each, thrown away).
    fn skip_to(&mut self, sn: u32, conv_id: u32) {
        info!(sn, "joined in the middle of a conversation, skipping to the first segment seen");
        let mut at = 0u32;
        while at < sn {
            let count = (sn - at).min(256);
            let mut bytes = Vec::with_capacity(count as usize * 25);
            for n in 0..count {
                bytes.extend_from_slice(&conv_id.to_le_bytes());
                bytes.push(81); // push
                bytes.push(0); // no more fragments
                bytes.extend_from_slice(&1024u16.to_le_bytes());
                bytes.extend_from_slice(&0u32.to_le_bytes());
                bytes.extend_from_slice(&(at + n).to_le_bytes());
                bytes.extend_from_slice(&0u32.to_le_bytes());
                bytes.extend_from_slice(&1u32.to_le_bytes());
                bytes.push(0);
            }
            if let Err(e) = self.kcp.input(&bytes) {
                warn!("could not skip segments: {e}");
                return;
            }
            while let Ok(size) = self.kcp.peeksize() {
                let mut sink = vec![0; size.max(1)];
                if self.kcp.recv(&mut sink).is_err() {
                    break;
                }
            }
            at += count;
        }
    }

    #[inline]
    fn clock(&self) -> u32 {
        SystemTime::now()
            .duration_since(self.time_start)
            .expect("time went backwards")
            .as_millis() as u32
    }
}

#[inline]
fn new_kcp(conv_id: u32) -> Kcp<Vec<u8>> {
    let mut kcp = Kcp::new(conv_id, Vec::new());
    kcp.set_wndsize(1024, 1024);
    kcp
}

/// A conversation that is joined with a segment numbered more than this is joined
/// in the middle (an early one can come out of order).
const RESYNC_FROM: u32 = 64;

/// The number of the first push segment in segments as they are after
/// `reformat_kcp_segments` (conversation 4 bytes, then cmd, frg, wnd 2, ts 4, sn 4,
/// una 4, len 4, then the data).
fn first_push_sn(data: &[u8]) -> Option<u32> {
    let mut lowest: Option<u32> = None;
    let mut i = 0;
    while i + KCP_OVERHEAD as usize <= data.len() {
        let cmd = data[i + 4];
        let sn = u32::from_le_bytes(data[i + 12..i + 16].try_into().ok()?);
        let len = u32::from_le_bytes(data[i + 20..i + 24].try_into().ok()?) as usize;
        if cmd == 81 {
            lowest = Some(lowest.map_or(sn, |was| was.min(sn)));
        }
        i += 24 + len;
    }
    lowest
}

fn validate_kcp_segment(payload: &[u8]) -> Option<u32> {
    if payload.len() <= KCP_OVERHEAD {
        warn!(
            len = payload.len(),
            data = bytes_as_hex(payload),
            "kcp header was too short"
        );
        return None;
    }
    Some(get_conv(payload))
}

// reformat to skip bytes 4..8
fn reformat_kcp_segments(data: &[u8]) -> Vec<u8> {
    let span = span!(Level::TRACE, "split");
    let _enter = span.enter();

    let mut reformatted_bytes = Vec::new();

    trace!("before split: {}", bytes_as_hex(data));

    let mut i = 0;
    while i < data.len() {
        let conv_id = &data[i..i + 4];

        let _ = &data[i + 4..i + 8]; // skip

        let remaining_header = &data[i + 8..i + 28];
        let content_len = u32::from_le_bytes(data[i + 24..i + 28].try_into().unwrap()) as usize;
        let content = &data[i + 28 + 4..i + 28 + 4 + content_len];

        for b in conv_id.iter().chain(remaining_header).chain(content) {
            reformatted_bytes.push(*b);
        }

        i += 28 + 4 + content_len;
    }

    trace!(" after split: {}", bytes_as_hex(&reformatted_bytes));

    reformatted_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A push segment as the game sends it: conversation, 4 bytes more, cmd, frg, wnd,
    /// ts, sn, una, len, 4 bytes more, the data.
    fn segment(conv: u32, sn: u32, frg: u8, data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&conv.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.push(81);
        bytes.push(frg);
        bytes.extend_from_slice(&256u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&sn.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn a_conversation_joined_in_the_middle_is_followed() {
        let first = segment(7, 40_000, 0, b"hello");
        let mut kcp = KcpSniffer::try_new(&first).unwrap();
        assert_eq!(kcp.receive_segments(&first), vec![b"hello".to_vec()]);
        let next = segment(7, 40_001, 0, b"world");
        assert_eq!(kcp.receive_segments(&next), vec![b"world".to_vec()]);
    }

    #[test]
    fn a_new_conversation_is_not_skipped_into() {
        let first = segment(7, 0, 0, b"one");
        let mut kcp = KcpSniffer::try_new(&first).unwrap();
        assert_eq!(kcp.receive_segments(&first), vec![b"one".to_vec()]);
        // Out of order at the start: the later one waits for the earlier.
        let third = segment(7, 2, 0, b"three");
        assert!(kcp.receive_segments(&third).is_empty());
        let second = segment(7, 1, 0, b"two");
        assert_eq!(kcp.receive_segments(&second), vec![b"two".to_vec(), b"three".to_vec()]);
    }

    #[test]
    fn a_packet_of_only_acknowledgements_does_not_stop_the_joining() {
        // The first thing seen in a direction is an acknowledgement (cmd 82), then data.
        let mut ack = segment(7, 39_990, 0, b"");
        ack[8] = 82;
        let mut kcp = KcpSniffer::try_new(&ack).unwrap();
        assert!(kcp.receive_segments(&ack).is_empty());
        let data = segment(7, 40_000, 0, b"hello");
        assert_eq!(kcp.receive_segments(&data), vec![b"hello".to_vec()]);
    }
}
