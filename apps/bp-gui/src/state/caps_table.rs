//! The device capability table shown in the device popover.

use bp_core::{PROBE_ENCODINGS, PROBE_RATES, SampleEncoding, SourceFormat, SourceKind};

use super::support::track_support;
use crate::backend::DeviceCaps;

const DSD_MULTIPLES: [u16; 3] = [64, 128, 256];

#[derive(Debug, Clone, PartialEq)]
pub struct CapTable {
    /// Probed rates the device accepts in some 2-channel format.
    pub rates: Vec<u32>,
    /// One row per encoding the device accepts in some 2-channel format; one cell per rate.
    pub rows: Vec<(String, Vec<bool>)>,
    /// `(DSD multiple, playable over DoP)`.
    pub dsd: Vec<(u16, bool)>,
}

fn label(e: SampleEncoding) -> String {
    match e {
        SampleEncoding::Int { container, valid } => format!("{container}/{valid}"),
        SampleEncoding::Float32 => "32f".to_owned(),
    }
}

pub fn cap_table(dev: &DeviceCaps) -> CapTable {
    let stereo = |rate: Option<u32>, enc: Option<SampleEncoding>| {
        dev.caps.formats.iter().any(|(r, ch, e)| {
            *ch == 2 && rate.is_none_or(|x| x == *r) && enc.is_none_or(|x| x == *e)
        })
    };
    let rates: Vec<u32> = PROBE_RATES
        .into_iter()
        .filter(|r| stereo(Some(*r), None))
        .collect();
    let rows = PROBE_ENCODINGS
        .into_iter()
        .filter(|e| stereo(None, Some(*e)))
        .map(|e| {
            let cells = rates
                .iter()
                .map(|r| dev.caps.formats.contains(&(*r, 2, e)))
                .collect();
            (label(e), cells)
        })
        .collect();
    let dsd = DSD_MULTIPLES
        .into_iter()
        .map(|multiple| {
            let src = SourceFormat {
                codec: bp_core::Codec::Dsf,
                sample_rate: 2_822_400 * u32::from(multiple) / 64,
                channels: 2,
                kind: SourceKind::Dsd { multiple },
            };
            (multiple, track_support(&src, dev).is_ok())
        })
        .collect();
    CapTable { rates, rows, dsd }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::support::testutil::caps_up_to;

    #[test]
    fn cap_table_rows_and_dsd() {
        let t = cap_table(&caps_up_to(192_000, true));
        assert_eq!(
            t.rates,
            vec![44_100, 48_000, 88_200, 96_000, 176_400, 192_000]
        );
        assert_eq!(t.rows[0].0, "16/16");
        let labels: Vec<_> = t.rows.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["16/16", "24/24", "32/24", "32/32", "32f"]);
        assert!(t.rows.iter().all(|(_, cells)| cells.iter().all(|&c| c)));
        assert_eq!(t.dsd, vec![(64, true), (128, false), (256, false)]);
    }
}
