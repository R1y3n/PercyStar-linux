//! Compact version-1.1 root signatures stored in Scarlett DXBC PRIV chunks.
//! Reconstructed from the executable decoder; this is not the RTS0 format.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorKind {
    Srv,
    Uav,
    Sampler,
    Cbv,
}

impl DescriptorKind {
    pub fn register_prefix(self) -> char {
        match self {
            Self::Srv => 't',
            Self::Uav => 'u',
            Self::Sampler => 's',
            Self::Cbv => 'b',
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptorRange {
    pub kind: DescriptorKind,
    pub register: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootParameter {
    DescriptorTable {
        space: u8,
        ranges: Vec<DescriptorRange>,
    },
    Constants {
        register: u32,
        dword_count: u8,
    },
    Descriptor {
        kind: DescriptorKind,
        register: u32,
        space: u8,
        unknown_bits: u8,
    },
}

#[derive(Clone, Debug)]
pub struct StaticSampler {
    pub filter: u16,
    pub address_u: u8,
    pub address_v: u8,
    pub address_w: u8,
    pub max_anisotropy: u8,
    pub comparison: u8,
    pub border_color: u8,
    pub mip_bias: f32,
    pub min_lod: f32,
    pub max_lod: f32,
    pub register: u8,
    pub space: u8,
}

#[derive(Clone, Debug)]
pub struct CompactRootSignature {
    pub flags: u16,
    pub unknown: u16,
    pub parameters: Vec<RootParameter>,
    pub static_samplers: Vec<StaticSampler>,
    pub padding: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootBindingLocation {
    DescriptorTable {
        parameter: usize,
        descriptor_index: usize,
    },
    Constants {
        parameter: usize,
        dword_count: u8,
    },
    RootDescriptor {
        parameter: usize,
    },
    StaticSampler {
        index: usize,
    },
}

impl CompactRootSignature {
    pub fn find_binding(
        &self,
        kind: DescriptorKind,
        register: u32,
        space: u8,
    ) -> Option<RootBindingLocation> {
        for (parameter, entry) in self.parameters.iter().enumerate() {
            match entry {
                RootParameter::DescriptorTable {
                    space: table_space,
                    ranges,
                } if *table_space == space => {
                    if let Some(descriptor_index) = ranges
                        .iter()
                        .position(|range| range.kind == kind && range.register == register)
                    {
                        return Some(RootBindingLocation::DescriptorTable {
                            parameter,
                            descriptor_index,
                        });
                    }
                }
                RootParameter::Constants {
                    register: binding_register,
                    dword_count,
                } if kind == DescriptorKind::Cbv && space == 0 && *binding_register == register => {
                    return Some(RootBindingLocation::Constants {
                        parameter,
                        dword_count: *dword_count,
                    });
                }
                RootParameter::Descriptor {
                    kind: binding_kind,
                    register: binding_register,
                    space: binding_space,
                    ..
                } if *binding_kind == kind
                    && *binding_register == register
                    && *binding_space == space =>
                {
                    return Some(RootBindingLocation::RootDescriptor { parameter });
                }
                _ => {}
            }
        }
        if kind == DescriptorKind::Sampler {
            return self
                .static_samplers
                .iter()
                .position(|sampler| {
                    u32::from(sampler.register) == register && sampler.space == space
                })
                .map(|index| RootBindingLocation::StaticSampler { index });
        }
        None
    }
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(count)
            .ok_or("compact root offset overflow")?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or("truncated compact root signature")?;
        self.position = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn compact_float(&mut self) -> Result<f32, String> {
        let value = self.byte()?;
        if value == 255 {
            Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
        } else {
            Ok(f32::from(value) * 0.125 - 3.0)
        }
    }
    fn registers(
        &mut self,
        kind: DescriptorKind,
        ranges: &mut Vec<DescriptorRange>,
    ) -> Result<(), String> {
        let header = self.byte()?;
        if header == 0 {
            return Ok(());
        }
        let count = usize::from(header >> 4) | (usize::from(self.byte()?) << 4);
        let width = usize::from(header & 15) + 3;
        let packed = self.take((count * width).div_ceil(8))?;
        for index in 0..count {
            let mut register = 0;
            for bit in index * width..(index + 1) * width {
                register = (register << 1) | u32::from((packed[bit / 8] >> (7 - bit % 8)) & 1);
            }
            ranges.push(DescriptorRange { kind, register });
        }
        Ok(())
    }
}

pub fn parse_compact_root_signature(payload: &[u8]) -> Result<CompactRootSignature, String> {
    if payload.get(..2) != Some(&[0xdc, 0xe2]) {
        return Err("unsupported PRIV root signature prefix".into());
    }
    let mut reader = Reader {
        data: payload,
        position: 2,
    };
    let parameter_count = reader.byte()?;
    let flags = reader.u16()?;
    let unknown = reader.u16()?;
    let sampler_count = reader.byte()?;
    let mut parameters = Vec::with_capacity(usize::from(parameter_count));
    for _ in 0..parameter_count {
        let first = reader.byte()?;
        let second = reader.byte()?;
        let parameter = match first & 3 {
            0 => {
                let count = usize::from(first >> 4) | (usize::from(second) << 4);
                let mut ranges = Vec::new();
                for kind in [
                    DescriptorKind::Srv,
                    DescriptorKind::Uav,
                    DescriptorKind::Cbv,
                    DescriptorKind::Sampler,
                ] {
                    reader.registers(kind, &mut ranges)?;
                }
                if ranges.len() != count {
                    return Err("compact root descriptor count mismatch".into());
                }
                RootParameter::DescriptorTable {
                    space: (first >> 2) & 3,
                    ranges,
                }
            }
            1 => RootParameter::Constants {
                register: u32::from(first >> 7) | (u32::from(second) << 1),
                dword_count: (first >> 2) & 31,
            },
            kind => RootParameter::Descriptor {
                kind: if kind == 2 {
                    DescriptorKind::Srv
                } else {
                    DescriptorKind::Uav
                },
                register: u32::from(first >> 6) | (u32::from(second) << 2),
                space: (first >> 4) & 3,
                unknown_bits: first & 12,
            },
        };
        parameters.push(parameter);
    }
    let mut static_samplers = Vec::with_capacity(usize::from(sampler_count));
    for _ in 0..sampler_count {
        let filter = reader.u16()?;
        let uv = reader.byte()?;
        let wa = reader.byte()?;
        let cb = reader.byte()?;
        let mip_bias = reader.compact_float()?;
        let min_lod = reader.compact_float()?;
        let max_lod = reader.compact_float()?;
        let rs = reader.byte()?;
        static_samplers.push(StaticSampler {
            filter,
            address_u: uv & 15,
            address_v: uv >> 4,
            address_w: wa & 15,
            max_anisotropy: wa >> 4,
            comparison: cb & 31,
            border_color: cb >> 5,
            mip_bias,
            min_lod,
            max_lod,
            register: rs & 63,
            space: rs >> 6,
        });
    }
    let trailing = &payload[reader.position..];
    if trailing.len() > 3 {
        return Err("unexpected compact root signature trailing data".into());
    }
    let padding = trailing.to_vec();
    Ok(CompactRootSignature {
        flags,
        unknown,
        parameters,
        static_samplers,
        padding,
    })
}

pub fn extract_compact_root_signature(dxbc: &[u8]) -> Result<CompactRootSignature, String> {
    parse_compact_root_signature(
        super::find_dxbc_chunk(dxbc, b"PRIV")?.ok_or("DXBC has no PRIV root signature")?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic root: table in space1 containing t1,t365,u22,b23; b500 constants;
    // t513 root SRV in space3; one static sampler with an escaped max LOD.
    fn fixture() -> Vec<u8> {
        vec![
            0xdc, 0xe2, 3, 0, 4, 0x34, 0x12, 1, 0x44, 0, 0x26, 0, 0, 0xdb, 0x40, 0x12, 0, 0xb0,
            0x12, 0, 0xb8, 0, 9, 250, 0x72, 128, 0x55, 0, 0x21, 0x81, 2, 24, 24, 255, 0, 0, 0x80,
            0x7f, 0x43,
        ]
    }

    #[test]
    fn compact_roots_join_packed_registers_and_static_samplers() {
        let root = parse_compact_root_signature(&fixture()).unwrap();
        assert_eq!(root.unknown, 0x1234);
        assert_eq!(
            root.find_binding(DescriptorKind::Srv, 365, 1),
            Some(RootBindingLocation::DescriptorTable {
                parameter: 0,
                descriptor_index: 1
            })
        );
        assert_eq!(
            root.find_binding(DescriptorKind::Uav, 22, 1),
            Some(RootBindingLocation::DescriptorTable {
                parameter: 0,
                descriptor_index: 2
            })
        );
        assert_eq!(
            root.find_binding(DescriptorKind::Cbv, 23, 1),
            Some(RootBindingLocation::DescriptorTable {
                parameter: 0,
                descriptor_index: 3
            })
        );
        assert_eq!(
            root.find_binding(DescriptorKind::Cbv, 500, 0),
            Some(RootBindingLocation::Constants {
                parameter: 1,
                dword_count: 2
            })
        );
        assert_eq!(
            root.find_binding(DescriptorKind::Srv, 513, 3),
            Some(RootBindingLocation::RootDescriptor { parameter: 2 })
        );
        assert_eq!(
            root.find_binding(DescriptorKind::Sampler, 3, 1),
            Some(RootBindingLocation::StaticSampler { index: 0 })
        );
        assert!(root.static_samplers[0].max_lod.is_infinite());
        assert_eq!(root.static_samplers[0].mip_bias, 0.0);
        assert_eq!(root.find_binding(DescriptorKind::Srv, 109, 1), None);
    }

    #[test]
    fn compact_roots_reject_truncation_and_inconsistent_counts() {
        let data = fixture();
        for end in 0..data.len() {
            assert!(
                parse_compact_root_signature(&data[..end]).is_err(),
                "accepted prefix {end}"
            );
        }
        let mut bad = data.clone();
        bad[8] = 0x54;
        assert!(parse_compact_root_signature(&bad)
            .unwrap_err()
            .contains("count mismatch"));
        let mut padded = data;
        padded.extend_from_slice(&[0; 3]);
        assert!(parse_compact_root_signature(&padded).is_ok());
        padded.push(0);
        assert!(parse_compact_root_signature(&padded).is_err());
    }
}
