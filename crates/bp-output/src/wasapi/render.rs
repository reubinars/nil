//! The render loop's per-buffer step, free of COM so it is tested on every OS.
//!
//! The Windows render thread calls [`Renderer::fill`] once per device event and writes the
//! whole buffer. Nothing here allocates or locks after [`Renderer::new`].

use crate::{Feed, Silence, fill_silence};

/// What one [`Renderer::fill`] put into the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fill {
    /// Frames popped from the ring (the only frames `frames_played` counts).
    pub popped: usize,
    /// Silence frames that count as an underrun.
    pub underrun: usize,
}

pub(crate) struct Renderer {
    feed: Feed,
    /// Copy of the last frame written, so DoP silence continues its markers.
    last: Vec<u8>,
    has_last: bool,
    /// A real frame has been popped: silence from now on is an underrun.
    played_real: bool,
    /// The last frame written was silence (not a ring frame).
    last_silent: bool,
    /// DoP marker pad frames before the popped ones in the last `fill`.
    pad: usize,
}

impl Renderer {
    pub fn new(feed: Feed) -> Renderer {
        Renderer {
            last: vec![0; feed.frame_bytes],
            feed,
            has_last: false,
            played_real: false,
            last_silent: false,
            pad: 0,
        }
    }

    pub fn frame_bytes(&self) -> usize {
        self.feed.frame_bytes
    }

    /// Where the frames popped by the last `fill` start in its buffer, in frames.
    pub fn popped_at(&self) -> usize {
        self.pad
    }

    pub fn is_dop(&self) -> bool {
        matches!(self.feed.silence, Silence::Dop { .. })
    }

    /// Fills `buf` (a whole number of frames) for one device buffer: pops as many whole frames
    /// as the ring holds, up to the buffer, and fills the rest with silence.
    ///
    /// The silence counts as an underrun only after the first real frame was played and
    /// while the producer is still attached. Once the producer is dropped (the decoder
    /// finished this stream) the ring running dry is the end of the stream, not an underrun.
    pub fn fill(&mut self, buf: &mut [u8]) -> Fill {
        let fb = self.feed.frame_bytes;
        let frames = buf.len() / fb;
        // Checked before reading the fill level: if the producer was already gone, everything
        // it pushed is visible below and any shortfall is the end of the stream.
        let ended = self.feed.consumer.is_abandoned();
        let available = self.feed.consumer.slots() / fb;
        let played_before = self.played_real;
        // DoP: one extra silence frame before data that would repeat the last marker.
        let pad = usize::from(available > 0 && frames > 0 && self.needs_marker_pad());
        let popped = available.min(frames - pad);
        self.pad = pad;
        let (head, tail) = buf.split_at_mut((pad + popped) * fb);
        let (pad_buf, data) = head.split_at_mut(pad * fb);
        if pad > 0 {
            fill_silence(&self.feed.silence, fb, Some(&self.last), pad_buf);
        }
        if popped > 0 {
            let chunk = self
                .feed
                .consumer
                .read_chunk(popped * fb)
                .expect("the ring holds these frames");
            let (a, b) = chunk.as_slices();
            data[..a.len()].copy_from_slice(a);
            data[a.len()..].copy_from_slice(b);
            chunk.commit_all();
            self.played_real = true;
            self.last_silent = false;
        }
        if !tail.is_empty() {
            let last = if popped > 0 {
                Some(&data[data.len() - fb..])
            } else if pad > 0 {
                Some(&pad_buf[..])
            } else {
                self.has_last.then_some(&self.last[..])
            };
            fill_silence(&self.feed.silence, fb, last, tail);
            self.last_silent = true;
        }
        self.remember_last(buf);
        let silent_after = frames - pad - popped;
        let mut underrun = 0;
        if played_before {
            underrun += pad;
        }
        if self.played_real && !ended {
            underrun += silent_after;
        }
        Fill { popped, underrun }
    }

    /// The next ring frame would repeat the DoP marker of the silence frame written last.
    ///
    /// DoP silence continues the markers from the last frame written, while the packer's
    /// frames continue from the last *real* frame; after an odd-length silence run (an
    /// underrun, or the pre-fill before a fresh packer's first frame) the two disagree. One
    /// extra silence frame (completing an even run) realigns them without touching the data.
    /// Only after silence: real frames are never delayed relative to real frames.
    fn needs_marker_pad(&mut self) -> bool {
        let Silence::Dop { container } = self.feed.silence else {
            return false;
        };
        if !(self.has_last && self.last_silent) {
            return false;
        }
        let at = usize::from(container) / 8 - 1;
        let Ok(chunk) = self.feed.consumer.read_chunk(at + 1) else {
            return false;
        };
        let (a, b) = chunk.as_slices();
        let next = if at < a.len() { a[at] } else { b[at - a.len()] };
        // Dropped without commit: nothing is consumed.
        next == self.last[at]
    }

    /// Fills `buf` with silence only (DoP pause tail, drain before close). Never an underrun.
    pub fn fill_silence(&mut self, buf: &mut [u8]) {
        let fb = self.feed.frame_bytes;
        let last = self.has_last.then_some(&self.last[..]);
        fill_silence(&self.feed.silence, fb, last, buf);
        self.last_silent |= !buf.is_empty();
        self.remember_last(buf);
    }

    fn remember_last(&mut self, buf: &[u8]) {
        let fb = self.feed.frame_bytes;
        if buf.len() >= fb {
            self.last.copy_from_slice(&buf[buf.len() - fb..]);
            self.has_last = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use rtrb::{Producer, RingBuffer};

    use super::*;

    fn renderer(capacity: usize, frame_bytes: usize, silence: Silence) -> (Producer<u8>, Renderer) {
        let (prod, consumer) = RingBuffer::new(capacity);
        let feed = Feed {
            consumer,
            frame_bytes,
            silence,
        };
        (prod, Renderer::new(feed))
    }

    fn push(prod: &mut Producer<u8>, data: &[u8]) {
        for &b in data {
            prod.push(b).unwrap();
        }
    }

    #[test]
    fn full_buffer_comes_from_the_ring() {
        let (mut prod, mut r) = renderer(64, 4, Silence::Pcm);
        assert_eq!(r.frame_bytes(), 4);
        assert!(!r.is_dop());
        let data: Vec<u8> = (1..=16).collect();
        push(&mut prod, &data);
        let mut buf = [0xEE; 12];
        let fill = r.fill(&mut buf);
        assert_eq!(
            fill,
            Fill {
                popped: 3,
                underrun: 0
            }
        );
        assert_eq!(buf, data[..12]);
        assert_eq!(prod.slots(), 64 - 4, "one frame left in the ring");
    }

    #[test]
    fn short_ring_fills_silence_and_counts_an_underrun() {
        let (mut prod, mut r) = renderer(64, 4, Silence::Pcm);
        push(&mut prod, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]); // 2 frames + a partial one
        let mut buf = [0xEE; 16];
        let fill = r.fill(&mut buf);
        assert_eq!(
            fill,
            Fill {
                popped: 2,
                underrun: 2
            }
        );
        assert_eq!(buf, [1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0]);
        // The partial frame stays in the ring.
        assert_eq!(prod.slots(), 64 - 2);
    }

    #[test]
    fn silence_before_the_first_real_frame_is_not_an_underrun() {
        let (mut prod, mut r) = renderer(64, 4, Silence::Pcm);
        let mut buf = [0xEE; 8];
        assert_eq!(
            r.fill(&mut buf),
            Fill {
                popped: 0,
                underrun: 0
            }
        );
        assert_eq!(buf, [0; 8]);
        push(&mut prod, &[9; 4]);
        assert_eq!(
            r.fill(&mut buf),
            Fill {
                popped: 1,
                underrun: 1
            }
        );
        assert_eq!(
            r.fill(&mut buf),
            Fill {
                popped: 0,
                underrun: 2
            }
        );
    }

    #[test]
    fn end_of_stream_is_not_an_underrun() {
        let (mut prod, mut r) = renderer(64, 4, Silence::Pcm);
        push(&mut prod, &[7; 12]);
        drop(prod);
        let mut buf = [0xEE; 16];
        assert_eq!(
            r.fill(&mut buf),
            Fill {
                popped: 3,
                underrun: 0
            }
        );
        assert_eq!(buf[12..], [0; 4]);
        assert_eq!(
            r.fill(&mut buf),
            Fill {
                popped: 0,
                underrun: 0
            }
        );
    }

    #[test]
    fn reads_across_the_ring_wrap() {
        let (mut prod, mut r) = renderer(12, 4, Silence::Pcm);
        let mut buf = [0; 8];
        push(&mut prod, &[1; 8]);
        r.fill(&mut buf);
        // Write position wraps: 4 bytes at the end of the storage, 4 at the start.
        push(&mut prod, &[2, 3, 4, 5, 6, 7, 8, 9]);
        let fill = r.fill(&mut buf);
        assert_eq!(fill.popped, 2);
        assert_eq!(buf, [2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn dop_silence_continues_markers_from_the_last_frame() {
        // 32-bit container, 2 channels: marker in byte 3 of each sample.
        let (mut prod, mut r) = renderer(64, 8, Silence::Dop { container: 32 });
        assert!(r.is_dop());
        push(&mut prod, &[0, 0x12, 0x34, 0x05, 0, 0x56, 0x78, 0x05]);
        let mut buf = [0xEE; 24];
        let fill = r.fill(&mut buf);
        assert_eq!(fill.popped, 1);
        assert_eq!(fill.underrun, 2);
        let markers = |b: &[u8]| -> Vec<u8> { b.chunks(4).map(|s| s[3]).collect() };
        assert_eq!(markers(&buf), [0x05, 0x05, 0xFA, 0xFA, 0x05, 0x05]);
        assert_eq!(buf[9..11], [0x69, 0x69]);
        // The next buffer of pure silence continues from the last silence frame.
        let mut buf2 = [0xEE; 16];
        r.fill(&mut buf2);
        assert_eq!(markers(&buf2), [0xFA, 0xFA, 0x05, 0x05]);
        // So does a pause tail, and it is never an underrun (fill_silence returns nothing).
        r.fill_silence(&mut buf2);
        assert_eq!(markers(&buf2), [0xFA, 0xFA, 0x05, 0x05]);
        r.fill_silence(&mut buf2);
        assert_eq!(markers(&buf2), [0xFA, 0xFA, 0x05, 0x05]);
        // Data after the tail continues from the real frames' marker state (packer-owned).
        push(&mut prod, &[0, 1, 2, 0xFA, 0, 3, 4, 0xFA]);
        r.fill(&mut buf2);
        assert_eq!(markers(&buf2)[..2], [0xFA, 0xFA]);
        assert_eq!(markers(&buf2)[2..], [0x05, 0x05]);
    }

    #[test]
    fn pcm_tail_is_zeros() {
        let (mut prod, mut r) = renderer(64, 4, Silence::Pcm);
        push(&mut prod, &[5; 8]);
        let mut buf = [0; 8];
        r.fill(&mut buf);
        r.fill_silence(&mut buf);
        assert_eq!(buf, [0; 8]);
        assert_eq!(prod.slots(), 64, "the tail does not touch the ring");
    }

    /// DoP frame, 32-bit container, 2 channels, with `marker` and payload `n`.
    fn dop_frame(marker: u8, n: u8) -> [u8; 8] {
        [0, n, n, marker, 0, n, n, marker]
    }

    /// Markers of every frame (first channel) of DoP 32/2 output.
    fn frame_markers(out: &[u8]) -> Vec<u8> {
        out.chunks(8).map(|f| f[3]).collect()
    }

    fn assert_alternating(out: &[u8]) {
        let m = frame_markers(out);
        for (i, w) in m.windows(2).enumerate() {
            assert_ne!(w[0], w[1], "marker repeats at frame {}: {m:02X?}", i + 1);
        }
        // Both channels of each frame carry the same marker.
        assert!(out.chunks(8).all(|f| f[3] == f[7]));
    }

    #[test]
    fn dop_odd_underrun_then_data_keeps_markers_alternating() {
        let (mut prod, mut r) = renderer(256, 8, Silence::Dop { container: 32 });
        let mut out = Vec::new();
        // The packer's frames alternate 05, FA, 05, …
        push(&mut prod, &dop_frame(0x05, 1));
        let mut buf = [0xEE; 32];
        // 1 real frame + an odd run of 3 silence frames.
        assert_eq!(
            r.fill(&mut buf),
            Fill {
                popped: 1,
                underrun: 3
            }
        );
        out.extend_from_slice(&buf);
        // Data resumes with the packer's next marker (FA), which the odd run also ended on.
        push(&mut prod, &dop_frame(0xFA, 2));
        push(&mut prod, &dop_frame(0x05, 3));
        let fill = r.fill(&mut buf);
        // One more silence frame first (completing an even run), then the data, then silence.
        assert_eq!(
            fill,
            Fill {
                popped: 2,
                underrun: 2
            }
        );
        assert_eq!(
            buf[8..24],
            [dop_frame(0xFA, 2), dop_frame(0x05, 3)].concat()
        );
        out.extend_from_slice(&buf);
        assert_alternating(&out);
    }

    #[test]
    fn dop_data_fills_the_buffer_after_one_extra_silence_frame() {
        let (mut prod, mut r) = renderer(256, 8, Silence::Dop { container: 32 });
        let mut out = Vec::new();
        push(&mut prod, &dop_frame(0x05, 1));
        let mut buf = [0xEE; 16];
        r.fill(&mut buf); // real 05 + silence FA: odd run of 1
        out.extend_from_slice(&buf);
        for (i, m) in [0xFA, 0x05, 0xFA].into_iter().enumerate() {
            push(&mut prod, &dop_frame(m, i as u8 + 2));
        }
        // The extra silence frame takes one slot: one frame fewer is popped.
        assert_eq!(r.fill(&mut buf).popped, 1);
        out.extend_from_slice(&buf);
        assert_eq!(r.fill(&mut buf).popped, 2);
        out.extend_from_slice(&buf);
        assert_alternating(&out);
        assert_eq!(prod.slots(), 256, "every real frame was played");
    }

    #[test]
    fn dop_prefill_silence_then_data_keeps_markers_alternating() {
        let (mut prod, mut r) = renderer(256, 8, Silence::Dop { container: 32 });
        let mut out = Vec::new();
        // Pre-fill with an empty ring: an odd run of 3 silence frames (05, FA, 05).
        let mut buf = [0xEE; 24];
        assert_eq!(
            r.fill(&mut buf),
            Fill {
                popped: 0,
                underrun: 0
            }
        );
        out.extend_from_slice(&buf);
        // A fresh packer starts at 05.
        push(&mut prod, &dop_frame(0x05, 1));
        push(&mut prod, &dop_frame(0xFA, 2));
        let fill = r.fill(&mut buf);
        // The extra frame precedes the first real frame, so it is not an underrun.
        assert_eq!(
            fill,
            Fill {
                popped: 2,
                underrun: 0
            }
        );
        out.extend_from_slice(&buf);
        assert_alternating(&out);
    }

    #[test]
    fn dop_even_run_needs_no_extra_frame() {
        let (mut prod, mut r) = renderer(256, 8, Silence::Dop { container: 32 });
        let mut out = Vec::new();
        let mut buf = [0xEE; 16];
        r.fill(&mut buf); // pre-fill: 05, FA
        out.extend_from_slice(&buf);
        push(&mut prod, &dop_frame(0x05, 1));
        push(&mut prod, &dop_frame(0xFA, 2));
        assert_eq!(r.fill(&mut buf).popped, 2);
        assert_eq!(buf[..], [dop_frame(0x05, 1), dop_frame(0xFA, 2)].concat());
        out.extend_from_slice(&buf);
        assert_alternating(&out);
    }

    #[test]
    fn pcm_resume_never_inserts_a_frame() {
        let (mut prod, mut r) = renderer(64, 4, Silence::Pcm);
        push(&mut prod, &[1; 4]);
        let mut buf = [0xEE; 12];
        r.fill(&mut buf); // 1 real frame + 2 silence frames
        push(&mut prod, &[2; 12]);
        assert_eq!(r.fill(&mut buf).popped, 3);
        assert_eq!(buf, [2; 12]);
    }
}
