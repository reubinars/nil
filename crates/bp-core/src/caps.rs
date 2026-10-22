use std::collections::BTreeSet;

use crate::format::{DeviceFormat, SampleEncoding};

/// Opaque device identifier.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct DeviceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub is_default: bool,
}

/// Set of (sample_rate, channels, encoding) combinations a device accepts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub formats: BTreeSet<(u32, u16, SampleEncoding)>,
}

impl Capabilities {
    fn key(f: &DeviceFormat) -> (u32, u16, SampleEncoding) {
        (f.sample_rate, f.channels, f.encoding)
    }

    pub fn supports(&self, f: &DeviceFormat) -> bool {
        self.formats.contains(&Self::key(f))
    }

    pub fn max_rate(&self, channels: u16) -> Option<u32> {
        self.formats
            .iter()
            .filter(|(_, c, _)| *c == channels)
            .map(|(r, _, _)| *r)
            .max()
    }

    pub fn has_channels(&self, channels: u16) -> bool {
        self.formats.iter().any(|(_, c, _)| *c == channels)
    }

    pub fn remove(&mut self, f: &DeviceFormat) {
        self.formats.remove(&Self::key(f));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::*;

    #[test]
    fn caps_max_rate_and_supports() {
        let mut c = Capabilities::default();
        let i32_24 = SampleEncoding::Int {
            container: 32,
            valid: 24,
        };
        c.formats.insert((44_100, 2, i32_24));
        c.formats.insert((192_000, 2, i32_24));
        assert_eq!(c.max_rate(2), Some(192_000));
        assert_eq!(c.max_rate(6), None);
        assert!(c.supports(&DeviceFormat {
            sample_rate: 192_000,
            channels: 2,
            encoding: i32_24
        }));
        assert!(!c.has_channels(1));
    }
}
