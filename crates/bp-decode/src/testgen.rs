//! Synthetic DSF and DFF (DSDIFF) file builders for tests.
//!
//! Inputs hold MSB-first DSD bytes per channel (the in-memory convention of `Block::Dsd`).

const DSF_BLOCK: usize = 4096;

fn check_inputs(channels: u16, per_channel: &[Vec<u8>]) -> usize {
    assert_eq!(
        per_channel.len(),
        channels as usize,
        "one buffer per channel"
    );
    let len = per_channel[0].len();
    assert!(
        per_channel.iter().all(|c| c.len() == len),
        "all channels must have equal length"
    );
    len
}

/// Builds a DSF file. `multiple` is the DSD multiple (64 = DSD64); only mono and stereo are supported.
pub fn dsf_bytes(
    channels: u16,
    multiple: u16,
    per_channel: &[Vec<u8>],
    trailing_id3: Option<&[u8]>,
) -> Vec<u8> {
    assert!(channels == 1 || channels == 2, "mono or stereo only");
    let len = check_inputs(channels, per_channel);
    let blocks = len.div_ceil(DSF_BLOCK);
    let data_len = blocks * DSF_BLOCK * channels as usize;

    let mut out = Vec::with_capacity(92 + data_len);
    // "DSD " chunk (28 bytes); total size and metadata pointer patched below.
    out.extend_from_slice(b"DSD ");
    out.extend_from_slice(&28u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    // "fmt " chunk (52 bytes)
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&52u64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // format version
    out.extend_from_slice(&0u32.to_le_bytes()); // format id: raw DSD
    out.extend_from_slice(&(channels as u32).to_le_bytes()); // channel type: 1 mono, 2 stereo
    out.extend_from_slice(&(channels as u32).to_le_bytes());
    out.extend_from_slice(&(44_100u32 * multiple as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // bits per sample
    out.extend_from_slice(&(len as u64 * 8).to_le_bytes());
    out.extend_from_slice(&(DSF_BLOCK as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    // "data" chunk
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(12 + data_len as u64).to_le_bytes());
    for b in 0..blocks {
        for ch in per_channel {
            let start = (b * DSF_BLOCK).min(len);
            let end = ((b + 1) * DSF_BLOCK).min(len);
            out.extend(ch[start..end].iter().map(|v| v.reverse_bits()));
            out.resize(out.len() + DSF_BLOCK - (end - start), 0);
        }
    }
    if let Some(id3) = trailing_id3 {
        let ptr = out.len() as u64;
        out[20..28].copy_from_slice(&ptr.to_le_bytes());
        out.extend_from_slice(id3);
    }
    let total = out.len() as u64;
    out[12..20].copy_from_slice(&total.to_le_bytes());
    out
}

/// Appends a DSDIFF chunk (big-endian size, padded to even length).
fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u64).to_be_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

/// Builds a DFF (DSDIFF) file with byte-interleaved, MSB-first data. With `compressed_dst`
/// only the `CMPR` type says `DST `; the payload is still written as a plain `DSD ` chunk.
pub fn dff_bytes(
    channels: u16,
    multiple: u16,
    per_channel: &[Vec<u8>],
    compressed_dst: bool,
) -> Vec<u8> {
    assert!(channels == 1 || channels == 2, "mono or stereo only");
    let len = check_inputs(channels, per_channel);

    let mut prop = b"SND ".to_vec();
    chunk(
        &mut prop,
        b"FS  ",
        &(44_100u32 * multiple as u32).to_be_bytes(),
    );
    let mut chnl = channels.to_be_bytes().to_vec();
    if channels == 1 {
        chnl.extend_from_slice(b"C   ");
    } else {
        chnl.extend_from_slice(b"SLFT");
        chnl.extend_from_slice(b"SRGT");
    }
    chunk(&mut prop, b"CHNL", &chnl);
    let (kind, name): (&[u8; 4], &[u8]) = if compressed_dst {
        (b"DST ", b"DST Encoded")
    } else {
        (b"DSD ", b"not compressed")
    };
    let mut cmpr = kind.to_vec();
    cmpr.push(name.len() as u8);
    cmpr.extend_from_slice(name);
    if cmpr.len() % 2 == 1 {
        cmpr.push(0); // pad the count + string to even length
    }
    chunk(&mut prop, b"CMPR", &cmpr);

    let mut data = Vec::with_capacity(len * channels as usize);
    for i in 0..len {
        for ch in per_channel {
            data.push(ch[i]);
        }
    }

    let mut body = b"DSD ".to_vec();
    chunk(&mut body, b"FVER", &0x0105_0000u32.to_be_bytes());
    chunk(&mut body, b"PROP", &prop);
    chunk(&mut body, b"COMT", b"hello"); // unknown, odd length: exercises skip + padding
    chunk(&mut body, b"DSD ", &data);

    let mut out = Vec::with_capacity(body.len() + 12);
    chunk(&mut out, b"FRM8", &body);
    out
}
