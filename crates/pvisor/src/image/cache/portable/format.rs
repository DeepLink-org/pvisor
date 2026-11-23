use super::*;
use crate::env::cache::BLOB_TARGET_ENV;

const MIB: usize = 1024 * 1024;
const FAMILY: &str = "pvisor-image-pack";
pub(super) const LEGACY: &[u8] = br#"{"format_version":2,"hash_algorithm":"sha256","encoding":"zstd-seekable","chunk_bytes":1048576,"metadata_encoding":"pvisor-radix-v2","metadata_frame_bytes":65536,"route_hash":"xxh3-64","route_seed":"707669736f727632","route_radix_bits":4}"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Version {
    ImagePackV1,
    LegacyV2,
}

impl Version {
    pub(super) fn from_record(value: u32) -> anyhow::Result<Self> {
        match value {
            1 => Ok(Self::ImagePackV1),
            2 => Ok(Self::LegacyV2),
            _ => bail!("unsupported image storage version {value}"),
        }
    }

    pub(super) const fn record(self) -> u32 {
        match self {
            Self::ImagePackV1 => 1,
            Self::LegacyV2 => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format {
    V1(Packing),
    LegacyV2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Packing {
    blob_target_bytes: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    format: String,
    format_version: u32,
    packing: Packing,
}

impl Packing {
    pub(super) const DEFAULT: Self = Self {
        blob_target_bytes: 32 * MIB as u32,
    };

    pub(super) fn target(self) -> usize {
        self.blob_target_bytes as usize
    }

    pub(super) fn validate(self) -> anyhow::Result<Self> {
        ensure!(
            self.target() == 32 * MIB || self.target() == 64 * MIB,
            "unsupported Image Pack blob target"
        );
        Ok(self)
    }

    fn configured() -> anyhow::Result<Option<Self>> {
        std::env::var_os(BLOB_TARGET_ENV)
            .map(|value| {
                let mib = match value.to_str() {
                    Some("32") => 32,
                    Some("64") => 64,
                    _ => bail!("{BLOB_TARGET_ENV} must be 32 or 64"),
                };
                Ok(Self {
                    blob_target_bytes: (mib * MIB) as u32,
                })
            })
            .transpose()
    }
}

impl Format {
    pub(super) fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        ensure!(
            bytes.len() <= MAX_CONTROL,
            "cache format marker is too large"
        );
        if bytes == LEGACY {
            return Ok(Self::LegacyV2);
        }
        let marker: Marker = serde_json::from_slice(bytes).context("unsupported cache format")?;
        ensure!(
            marker.format == FAMILY && marker.format_version == Version::ImagePackV1.record(),
            "unsupported cache format"
        );
        let format = Self::V1(marker.packing.validate()?);
        ensure!(
            format.encode()? == bytes,
            "noncanonical Image Pack format marker"
        );
        Ok(format)
    }

    pub(super) fn encode(self) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::V1(packing) => Ok(serde_json::to_vec(&Marker {
                format: FAMILY.into(),
                format_version: Version::ImagePackV1.record(),
                packing,
            })?),
            Self::LegacyV2 => Ok(LEGACY.to_vec()),
        }
    }

    pub(super) fn from_commit(commit: &Commit) -> anyhow::Result<Self> {
        match (
            Version::from_record(commit.format_version)?,
            &commit.format,
            commit.packing,
        ) {
            (Version::ImagePackV1, Some(family), Some(packing)) if family == FAMILY => {
                Ok(Self::V1(packing.validate()?))
            }
            (Version::LegacyV2, None, None) => Ok(Self::LegacyV2),
            _ => bail!("COMMIT format identity mismatch"),
        }
    }

    pub(super) fn for_write(storage: &Storage) -> anyhow::Result<Packing> {
        Self::write_policy(
            storage
                .read("format.json", None, MAX_CONTROL)?
                .map(|object| Self::decode(&object.bytes))
                .transpose()?,
            Packing::configured()?,
        )
    }

    fn write_policy(
        existing: Option<Self>,
        configured: Option<Packing>,
    ) -> anyhow::Result<Packing> {
        match existing {
            Some(Self::V1(existing)) => {
                ensure!(
                    configured.is_none_or(|requested| requested == existing),
                    "{BLOB_TARGET_ENV} does not match the initialized Image Pack pool"
                );
                Ok(existing)
            }
            Some(Self::LegacyV2) => {
                bail!("prerelease V2 is read-only; publish to a new Image Pack V1 root")
            }
            None => Ok(configured.unwrap_or(Packing::DEFAULT)),
        }
    }
}

pub(super) fn family() -> String {
    FAMILY.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::new(None, None, Some(32))]
    #[case::new_large(None, Some(64), Some(64))]
    #[case::existing(Some(64), None, Some(64))]
    #[case::same(Some(64), Some(64), Some(64))]
    #[case::conflict(Some(64), Some(32), None)]
    fn policy(
        #[case] marker: Option<u32>,
        #[case] config: Option<u32>,
        #[case] result: Option<u32>,
    ) {
        let pack = |mib| Packing {
            blob_target_bytes: mib * MIB as u32,
        };
        assert_eq!(
            Format::write_policy(marker.map(|mib| Format::V1(pack(mib))), config.map(pack)).ok(),
            result.map(pack),
        );
    }

    #[test]
    fn legacy_v2_and_new_markers_are_distinct() {
        assert_eq!(Format::decode(LEGACY).unwrap(), Format::LegacyV2);
        assert!(Format::write_policy(Some(Format::LegacyV2), None).is_err());
        let bytes = Format::V1(Packing::DEFAULT).encode().unwrap();
        assert_eq!(
            bytes,
            br#"{"format":"pvisor-image-pack","format_version":1,"packing":{"blob_target_bytes":33554432}}"#
        );
        assert_eq!(
            Format::decode(&bytes).unwrap(),
            Format::V1(Packing::DEFAULT)
        );
        assert!(Format::decode(br#"{"format":"pvisor-image-pack","format_version":2,"packing":{"blob_target_bytes":33554432}}"#).is_err());
    }
}
