//! Native compiler metadata in FSSM 0x101. Offsets are payload-relative.
//! Layout interpretations are checked against SGD5 reflection and PRIV roots;
//! user-data locations describe compiler inputs, not an ISA data-flow trace.

use super::{
    payload_offset, read_u32, read_u64, CompactRootSignature, DescriptorKind, FssmShader,
    RootBindingLocation, RootParameter,
};
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeStage {
    Ls,
    Hs,
    Es,
    Gs,
    Vs,
    Ps,
    Mesh,
    Compute,
    Unknown(usize),
}

impl NativeStage {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Ls => "LS",
            Self::Hs => "HS",
            Self::Es => "ES",
            Self::Gs => "GS",
            Self::Vs => "VS",
            Self::Ps => "PS",
            Self::Mesh => "MS",
            Self::Compute => "CS",
            Self::Unknown(_) => "unknown",
        }
    }
    pub fn user_sgpr(self, dword: u8) -> Option<u16> {
        match self {
            Self::Ls | Self::Hs | Self::Es | Self::Gs | Self::Mesh => Some(u16::from(dword) + 8),
            Self::Vs | Self::Ps | Self::Compute => Some(u16::from(dword)),
            Self::Unknown(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRootType {
    Constants,
    Descriptor,
    Table,
    ImmediateSampler,
    ResourceHeap,
    SamplerHeap,
    Unknown(u32),
}

impl NativeRootType {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Constants,
            1 => Self::Descriptor,
            3 => Self::Table,
            4 => Self::ImmediateSampler,
            5 => Self::ResourceHeap,
            6 => Self::SamplerHeap,
            value => Self::Unknown(value),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeRootLayout {
    pub root_type: NativeRootType,
    pub table_offset_dwords: u32,
    pub user_sgpr_count: u32,
    pub base_register: u32,
    pub first_user_data_dword: u8,
    /// Sampler/heap slots have a separate meaning from ordinary root indices.
    pub root_slot: u8,
    pub reserved: u16,
}

#[derive(Clone, Debug)]
pub struct NativeResourceUsage {
    pub resource_kind: Option<DescriptorKind>,
    pub resource_kind_raw: u8,
    pub root_type: NativeRootType,
    pub reserved: u16,
    pub table_offset_dwords: u32,
    pub lower_bound_register: u32,
    pub stride: u32,
    pub descriptor_dword_count: u8,
    pub first_user_data_dword: u8,
    pub root_slot: u8,
    pub reserved_tail: u8,
}

#[derive(Clone, Debug)]
pub struct NativeInput {
    pub usage: u8,
    pub signature_row: u8,
    pub channel_mask: u8,
    /// VS fetch destination VGPR or PS interpolation attribute.
    pub location: u8,
    pub raw: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct NativeOutput {
    pub usage: u8,
    pub signature_row: u8,
    pub export_parameter: u8,
    pub flags: u8,
    pub raw: [u8; 6],
}

#[derive(Clone, Debug)]
pub struct PixelExportPatch {
    pub render_target: u8,
    pub code_offset_dwords: u32,
    /// Compiler alternatives for ZERO, 32_R, 32_GR, 32_AR, FP16,
    /// UNORM16, SNORM16, UINT16, SINT16, and 32_ABGR. Original ISA is preserved.
    pub alternatives: [Vec<u32>; 10],
}

#[derive(Clone, Debug)]
pub enum NativeStageIo {
    Vertex {
        inputs: Vec<NativeInput>,
        outputs: Vec<NativeOutput>,
    },
    Pixel {
        inputs: Vec<NativeInput>,
        input_addr: u32,
        db_shader_control: u32,
        patches: Vec<PixelExportPatch>,
    },
    Compute(super::FssmComputeMetadata),
    Mesh {
        outputs: Vec<NativeOutput>,
        registers: [u32; 8],
        thread_count: u16,
        max_vertices: u16,
        max_primitives: u16,
        user_sgpr_count: u8,
        topology: u8,
    },
    /// Merged/other stage extensions have a different layout.
    Unparsed,
}

#[derive(Clone, Debug)]
pub struct FssmNativeMetadata {
    pub stage: NativeStage,
    pub flags: u32,
    pub lds_byte_size: u32,
    pub register_word: u32,
    pub vgpr_count: u16,
    pub sgpr_count: u8,
    pub root_layout: Vec<NativeRootLayout>,
    pub used_resources: Vec<NativeResourceUsage>,
    pub io: NativeStageIo,
}

fn read_u16(data: &[u8], at: usize) -> Result<u16, String> {
    let bytes = data
        .get(at..at.checked_add(2).ok_or("u16 offset overflow")?)
        .ok_or("truncated u16")?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn records(data: &[u8], relative: u64, count: u32) -> Result<&[u8], String> {
    if count == 0 {
        return Ok(&[]);
    }
    let at = payload_offset(relative)?;
    let size = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(20))
        .ok_or("native table size overflow")?;
    data.get(at..at.checked_add(size).ok_or("native table extent overflow")?)
        .ok_or_else(|| "native table outside FSSM payload".into())
}

fn semantics(data: &[u8], at: usize, width: usize, count_at: usize) -> Result<&[u8], String> {
    let count = read_u32(data, count_at)? as usize;
    if count > 32 {
        return Err("native signature has more than 32 entries".into());
    }
    data.get(
        at..at
            .checked_add(count * width)
            .ok_or("native signature overflow")?,
    )
    .ok_or_else(|| "native signature outside stage metadata".into())
}

fn outputs(data: &[u8]) -> Result<Vec<NativeOutput>, String> {
    semantics(data, 0xc8, 6, 0x188)?
        .chunks_exact(6)
        .map(|e| {
            Ok(NativeOutput {
                usage: e[0],
                signature_row: e[1],
                export_parameter: e[2],
                flags: e[5],
                raw: e.try_into().unwrap(),
            })
        })
        .collect()
}

impl FssmShader {
    pub fn hardware_stage(&self) -> NativeStage {
        match self.stage_pointer_offset {
            0x18 => NativeStage::Ls,
            0x20 => NativeStage::Hs,
            0x28 => NativeStage::Es,
            0x30 => NativeStage::Gs,
            0x38 => NativeStage::Vs,
            0x40 => NativeStage::Ps,
            0x58 => NativeStage::Mesh,
            0x70 => NativeStage::Compute,
            offset => NativeStage::Unknown(offset),
        }
    }

    pub fn native_metadata(&self) -> Result<FssmNativeMetadata, String> {
        let at = self.stage_descriptor_offset;
        let end = at.checked_add(0x54).ok_or("native descriptor overflow")?;
        if end > self.code_offset {
            return Err("native descriptor overlaps program".into());
        }
        let desc = self
            .data
            .get(at..end)
            .ok_or("truncated native descriptor")?;
        let flags = read_u32(desc, 0x14)?;
        let register_word = read_u32(desc, 0x50)?;
        let vgpr_count = (register_word & 0x3ff) as u16;
        if vgpr_count > 256 {
            return Err("native VGPR count exceeds 256".into());
        }
        let root_layout = records(&self.data, read_u64(desc, 0x28)?, read_u32(desc, 0x38)?)?
            .chunks_exact(20)
            .map(|e| {
                Ok(NativeRootLayout {
                    root_type: NativeRootType::from_raw(read_u32(e, 0)?),
                    table_offset_dwords: read_u32(e, 4)?,
                    user_sgpr_count: read_u32(e, 8)?,
                    base_register: read_u32(e, 12)?,
                    first_user_data_dword: e[16],
                    root_slot: e[17],
                    reserved: read_u16(e, 18)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let used_resources = records(&self.data, read_u64(desc, 0x30)?, read_u32(desc, 0x3c)?)?
            .chunks_exact(20)
            .map(|e| {
                Ok(NativeResourceUsage {
                    // FSSM enum order differs from SGD5's resource category bits.
                    resource_kind: match e[0] {
                        0 => Some(DescriptorKind::Srv),
                        1 => Some(DescriptorKind::Sampler),
                        2 => Some(DescriptorKind::Cbv),
                        3 => Some(DescriptorKind::Uav),
                        _ => None,
                    },
                    resource_kind_raw: e[0],
                    root_type: NativeRootType::from_raw(u32::from(e[1])),
                    reserved: read_u16(e, 2)?,
                    table_offset_dwords: read_u32(e, 4)?,
                    lower_bound_register: read_u32(e, 8)?,
                    stride: read_u32(e, 12)?,
                    descriptor_dword_count: e[16],
                    first_user_data_dword: e[17],
                    root_slot: e[18],
                    reserved_tail: e[19],
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let data = self
            .data
            .get(end..self.code_offset)
            .ok_or("invalid native stage extent")?;
        let stage = self.hardware_stage();
        let io = match stage {
            NativeStage::Vs => NativeStageIo::Vertex {
                inputs: semantics(data, 0x24, 5, 0xc4)?
                    .chunks_exact(5)
                    .map(|e| NativeInput {
                        usage: e[0],
                        signature_row: e[1],
                        channel_mask: e[2] & 15,
                        location: e[3],
                        raw: e.to_vec(),
                    })
                    .collect(),
                outputs: outputs(data)?,
            },
            NativeStage::Ps => {
                let inputs = semantics(data, 0x24, 8, 0x124)?
                    .chunks_exact(8)
                    .map(|e| NativeInput {
                        usage: e[0],
                        signature_row: e[1],
                        channel_mask: e[2] & 15,
                        location: e[3],
                        raw: e.to_vec(),
                    })
                    .collect();
                let size = read_u32(data, 0x148)? as usize;
                if size > 10 {
                    return Err("pixel export patch exceeds its slot".into());
                }
                let mut patches = Vec::new();
                for target in 0..8 {
                    let p = 0x14c + target * 404;
                    let offset = read_u32(data, p)?;
                    if offset == u32::MAX {
                        continue;
                    }
                    let patch_end = usize::try_from(offset)
                        .ok()
                        .and_then(|n| n.checked_add(size))
                        .and_then(|n| n.checked_mul(4))
                        .ok_or("pixel export patch overflow")?;
                    if patch_end > self.code_size {
                        return Err("pixel export patch outside program".into());
                    }
                    let mut alternatives: [Vec<u32>; 10] = Default::default();
                    for (format, words) in alternatives.iter_mut().enumerate() {
                        for word in 0..size {
                            words.push(read_u32(data, p + 4 + format * 40 + word * 4)?);
                        }
                    }
                    patches.push(PixelExportPatch {
                        render_target: target as u8,
                        code_offset_dwords: offset,
                        alternatives,
                    });
                }
                NativeStageIo::Pixel {
                    inputs,
                    input_addr: read_u32(data, 0x134)?,
                    db_shader_control: read_u32(data, 0x138)?,
                    patches,
                }
            }
            NativeStage::Compute => NativeStageIo::Compute(
                self.compute_metadata()
                    .ok_or("invalid native compute dimensions")?,
            ),
            NativeStage::Mesh => {
                let mut registers = [0; 8];
                for (i, word) in registers.iter_mut().enumerate() {
                    *word = read_u32(data, 0x18c + 4 * i)?;
                }
                let info = data.get(0x448..0x450).ok_or("truncated native mesh info")?;
                NativeStageIo::Mesh {
                    outputs: outputs(data)?,
                    registers,
                    thread_count: read_u16(info, 0)?,
                    max_vertices: read_u16(info, 2)?,
                    max_primitives: read_u16(info, 4)?,
                    user_sgpr_count: info[6],
                    topology: info[7],
                }
            }
            _ => NativeStageIo::Unparsed,
        };
        Ok(FssmNativeMetadata {
            stage,
            flags,
            lds_byte_size: read_u32(desc, 0x4c)?,
            register_word,
            vgpr_count,
            sgpr_count: ((register_word >> 20) & 255) as u8,
            root_layout,
            used_resources,
            io,
        })
    }
}

impl NativeResourceUsage {
    /// Resolve compiler metadata to an original PRIV binding. Never treat a
    /// compiler sampler slot or heap slot as an ordinary root-parameter index.
    pub fn binding(
        &self,
        root: &CompactRootSignature,
    ) -> Option<(DescriptorKind, u32, u8, RootBindingLocation)> {
        let kind = self.resource_kind?;
        let slot = usize::from(self.root_slot);
        let register = self.lower_bound_register;
        match self.root_type {
            NativeRootType::Table => {
                if self.table_offset_dwords % 8 != 0 {
                    return None;
                }
                let RootParameter::DescriptorTable { space, ranges } = root.parameters.get(slot)?
                else {
                    return None;
                };
                let index = usize::try_from(self.table_offset_dwords / 8).ok()?;
                let range = ranges.get(index)?;
                (range.kind == kind && range.register == register).then_some((
                    kind,
                    register,
                    *space,
                    RootBindingLocation::DescriptorTable {
                        parameter: slot,
                        descriptor_index: index,
                    },
                ))
            }
            NativeRootType::Constants => {
                let RootParameter::Constants {
                    register: reg,
                    dword_count,
                } = root.parameters.get(slot)?
                else {
                    return None;
                };
                (kind == DescriptorKind::Cbv && *reg == register).then_some((
                    kind,
                    register,
                    0,
                    RootBindingLocation::Constants {
                        parameter: slot,
                        dword_count: *dword_count,
                    },
                ))
            }
            NativeRootType::Descriptor => {
                let RootParameter::Descriptor {
                    kind: k,
                    register: reg,
                    space,
                    ..
                } = root.parameters.get(slot)?
                else {
                    return None;
                };
                (*k == kind && *reg == register).then_some((
                    kind,
                    register,
                    *space,
                    RootBindingLocation::RootDescriptor { parameter: slot },
                ))
            }
            NativeRootType::ImmediateSampler if kind == DescriptorKind::Sampler => {
                let mut matches = root
                    .static_samplers
                    .iter()
                    .enumerate()
                    .filter(|(_, sampler)| u32::from(sampler.register) == register);
                let (index, sampler) = matches.next()?;
                if matches.next().is_some() {
                    return None;
                }
                Some((
                    kind,
                    register,
                    sampler.space,
                    RootBindingLocation::StaticSampler { index },
                ))
            }
            _ => None,
        }
    }
}

pub fn format_user_data(stage: NativeStage, first: u8, count: u32) -> String {
    if count == 0 {
        return "no user SGPRs".into();
    }
    let Some(end) = u32::from(first).checked_add(count - 1) else {
        return "invalid user-data extent".into();
    };
    let mut text = format!("user-data[{first}..{end}]");
    if stage == NativeStage::Compute && first >= 16 {
        text.push_str(" (argument memory; no inline SGPR assignment)");
    } else if let Some(sgpr) = stage.user_sgpr(first) {
        if let Some(last) = u32::from(sgpr).checked_add(count - 1) {
            let _ = write!(text, " -> s[{sgpr}..{last}]");
        }
    }
    text
}

pub fn format_native_metadata(meta: &FssmNativeMetadata) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "Native {}: wave={} VGPRs={} SGPRs={} LDS={} bytes flags=0x{:08x} register_word=0x{:08x}",
        meta.stage.tag(),
        if meta.flags & 32 != 0 { 32 } else { 64 },
        meta.vgpr_count,
        meta.sgpr_count,
        meta.lds_byte_size,
        meta.flags,
        meta.register_word
    );
    let _ = writeln!(
        text,
        "Compiler layout={} entries; used-resource ranges={} (different from effect binding lists)",
        meta.root_layout.len(),
        meta.used_resources.len()
    );
    let mut shown = std::collections::HashSet::new();
    for p in &meta.root_layout {
        let key = (
            format!("{:?}", p.root_type),
            p.root_slot,
            p.first_user_data_dword,
            p.user_sgpr_count,
        );
        if p.root_type == NativeRootType::ImmediateSampler || !shown.insert(key) {
            continue;
        }
        let _ = writeln!(
            text,
            "  {:?} slot={} {}",
            p.root_type,
            p.root_slot,
            format_user_data(meta.stage, p.first_user_data_dword, p.user_sgpr_count)
        );
    }
    for resource in &meta.used_resources {
        let register = resource
            .resource_kind
            .map(|k| format!("{}{}", k.register_prefix(), resource.lower_bound_register))
            .unwrap_or_else(|| {
                format!(
                    "kind{}:register{}",
                    resource.resource_kind_raw, resource.lower_bound_register
                )
            });
        let _ = writeln!(text, "  used {register}: {:?} slot={} table_byte_offset={} descriptor={} dwords stride={} user-data={}",
            resource.root_type, resource.root_slot, u64::from(resource.table_offset_dwords) * 4,
            resource.descriptor_dword_count, resource.stride, resource.first_user_data_dword);
    }
    let input_lines = |text: &mut String, inputs: &[NativeInput], vertex: bool| {
        for input in inputs {
            let location = if vertex {
                format!("v{} + channel", input.location)
            } else {
                format!("interpolation attr{}", input.location)
            };
            let _ = writeln!(
                text,
                "  input row{} mask=0x{:x} usage={} -> {location} raw={:02x?}",
                input.signature_row, input.channel_mask, input.usage, input.raw
            );
            if !vertex {
                let flags = input.raw[7];
                let _ = writeln!(
                    text,
                    "    flat={} linear={} fp16={} packed_pair={} second_row={}",
                    flags & 1 != 0,
                    flags & 2 != 0,
                    flags & 8 != 0,
                    flags & 16 != 0,
                    input.raw[5]
                );
            }
        }
    };
    let output_lines = |text: &mut String, outputs: &[NativeOutput]| {
        for output in outputs {
            let _ = writeln!(
                text,
                "  output row{} -> {}param{} usage={} fp16={} packed={} raw={:02x?}",
                output.signature_row,
                if output.export_parameter & 128 != 0 {
                    "per-primitive "
                } else {
                    ""
                },
                output.export_parameter & 127,
                output.usage,
                output.flags & 1 != 0,
                output.flags & 2 != 0,
                output.raw
            );
        }
    };
    match &meta.io {
        NativeStageIo::Vertex { inputs, outputs } => {
            input_lines(&mut text, inputs, true);
            output_lines(&mut text, outputs);
        }
        NativeStageIo::Pixel {
            inputs,
            input_addr,
            db_shader_control,
            patches,
        } => {
            let _ = writeln!(text, "PS SPI_PS_INPUT_ADDR=0x{input_addr:08x} DB_SHADER_CONTROL=0x{db_shader_control:08x}");
            input_lines(&mut text, inputs, false);
            for patch in patches {
                let _ = writeln!(text, "  MRT{} export patch at program+0x{:x}: {} dwords, 10 format alternatives (original code preserved); 32_ABGR={:08x?}",
                    patch.render_target, u64::from(patch.code_offset_dwords) * 4, patch.alternatives[0].len(), patch.alternatives[9]);
            }
        }
        NativeStageIo::Compute(compute) => {
            let _ = writeln!(text, "CS threads={:?} RSRC2=0x{:08x} group-id SGPRs={:?} thread-id VGPRs={} LDS allocation={} bytes",
                compute.num_threads, compute.rsrc2, compute.group_id_sgprs(), compute.thread_id_vgpr_count(), compute.lds_allocation_bytes());
        }
        NativeStageIo::Mesh {
            outputs,
            registers,
            thread_count,
            max_vertices,
            max_primitives,
            user_sgpr_count,
            topology,
        } => {
            let _ = writeln!(text, "Mesh group lanes={thread_count} max_vertices={max_vertices} max_primitives={max_primitives} user_sgprs={user_sgpr_count} topology={topology} registers={registers:08x?}");
            output_lines(&mut text, outputs);
        }
        NativeStageIo::Unparsed => {
            let _ = writeln!(
                text,
                "Stage-specific extension remains unparsed for {}",
                meta.stage.tag()
            );
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(stage_pointer: usize, descriptor: usize) -> FssmShader {
        let stage_data = descriptor + 0x54;
        let code = stage_data + 0xe00;
        let layout = code + 4;
        let used = layout + 20;
        let mut data = vec![0; used + 20];
        data[..4].copy_from_slice(b"FSSM");
        data[4..8].copy_from_slice(&0x101u32.to_le_bytes());
        let len = data.len() as u64 - 16;
        data[8..16].copy_from_slice(&len.to_le_bytes());
        data[16..24].copy_from_slice(&6u64.to_le_bytes());
        for p in (0x18..0x78).step_by(8) {
            data[p..p + 8].copy_from_slice(&super::super::FSSM_EMPTY_STAGE.to_le_bytes());
        }
        data[stage_pointer..stage_pointer + 8]
            .copy_from_slice(&((descriptor - 16) as u64).to_le_bytes());
        data[descriptor..descriptor + 8].copy_from_slice(&((code - 16) as u64).to_le_bytes());
        data[descriptor + 8..descriptor + 16].copy_from_slice(&4u64.to_le_bytes());
        data[descriptor + 0x14..descriptor + 0x18].copy_from_slice(&32u32.to_le_bytes());
        data[descriptor + 0x28..descriptor + 0x30]
            .copy_from_slice(&((layout - 16) as u64).to_le_bytes());
        data[descriptor + 0x30..descriptor + 0x38]
            .copy_from_slice(&((used - 16) as u64).to_le_bytes());
        data[descriptor + 0x38..descriptor + 0x3c].copy_from_slice(&1u32.to_le_bytes());
        data[descriptor + 0x3c..descriptor + 0x40].copy_from_slice(&1u32.to_le_bytes());
        data[descriptor + 0x50..descriptor + 0x54]
            .copy_from_slice(&(24u32 | (31 << 20)).to_le_bytes());
        for (i, value) in [3u32, 0, 1, 14].iter().enumerate() {
            data[layout + i * 4..layout + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        data[layout + 16] = 8;
        data[used + 1] = 3;
        data[used + 8..used + 12].copy_from_slice(&14u32.to_le_bytes());
        data[used + 12..used + 16].copy_from_slice(&16u32.to_le_bytes());
        data[used + 16] = 8;
        data[used + 17] = 8;
        data[code..code + 4].copy_from_slice(&0xbf810000u32.to_le_bytes());
        for target in 0..8 {
            let p = stage_data + 0x14c + target * 404;
            data[p..p + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        super::super::parse_fssm(data).unwrap()
    }

    #[test]
    fn relocated_descriptor_decodes_wave_registers_and_resource_roots() {
        let program = fixture(0x38, 0xe0);
        assert_eq!(program.compiled_wave_size(), Some(32));
        let meta = program.native_metadata().unwrap();
        assert_eq!((meta.vgpr_count, meta.sgpr_count), (24, 31));
        assert_eq!(meta.root_layout[0].first_user_data_dword, 8);
        let root = CompactRootSignature {
            flags: 0,
            unknown: 0,
            static_samplers: vec![],
            padding: vec![],
            parameters: vec![RootParameter::DescriptorTable {
                space: 1,
                ranges: vec![super::super::compact_root::DescriptorRange {
                    kind: DescriptorKind::Srv,
                    register: 14,
                }],
            }],
        };
        let usage = &meta.used_resources[0];
        let binding = usage.binding(&root).unwrap();
        assert_eq!(
            (binding.0, binding.1, binding.2),
            (DescriptorKind::Srv, 14, 1)
        );
        assert_eq!((usage.stride, usage.descriptor_dword_count), (16, 8));
        assert_eq!(
            format_user_data(NativeStage::Mesh, 8, 1),
            "user-data[8..8] -> s[16..16]"
        );
        assert_eq!(
            format_user_data(NativeStage::Compute, 5, 1),
            "user-data[5..5] -> s[5..5]"
        );
    }

    #[test]
    fn malformed_native_tables_fail_without_guessing_and_unknown_kinds_survive() {
        let mut program = fixture(0x38, 0xc0);
        let field = program.stage_descriptor_offset + 0x28;
        program.data[field..field + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(program.native_metadata().is_err());
        let mut program = fixture(0x38, 0xc0);
        let used = payload_offset(
            read_u64(&program.data, program.stage_descriptor_offset + 0x30).unwrap(),
        )
        .unwrap();
        program.data[used] = 255;
        let metadata = program.native_metadata().unwrap();
        assert_eq!(metadata.used_resources[0].resource_kind_raw, 255);
        assert_eq!(metadata.used_resources[0].resource_kind, None);
        program.data
            [program.stage_descriptor_offset + 0x50..program.stage_descriptor_offset + 0x54]
            .copy_from_slice(&257u32.to_le_bytes());
        assert!(program.native_metadata().is_err());
    }

    #[test]
    fn pixel_interpolation_and_export_alternatives_preserve_original_code() {
        let mut program = fixture(0x40, 0xc0);
        let e = program.stage_descriptor_offset + 0x54;
        program.data[e + 0x124..e + 0x128].copy_from_slice(&1u32.to_le_bytes());
        program.data[e + 0x24..e + 0x2c].copy_from_slice(&[6, 2, 15, 3, 0, 4, 0, 0x19]);
        program.data[e + 0x148..e + 0x14c].copy_from_slice(&1u32.to_le_bytes());
        program.data[e + 0x14c..e + 0x150].copy_from_slice(&0u32.to_le_bytes());
        program.data[e + 0x150 + 9 * 40..e + 0x154 + 9 * 40]
            .copy_from_slice(&0x12345678u32.to_le_bytes());
        let original = program.code().to_vec();
        let meta = program.native_metadata().unwrap();
        let NativeStageIo::Pixel {
            inputs, patches, ..
        } = &meta.io
        else {
            panic!("expected pixel I/O");
        };
        assert_eq!((inputs[0].signature_row, inputs[0].location), (2, 3));
        assert_eq!(patches[0].alternatives[9], vec![0x12345678]);
        assert_eq!(program.code(), original);
        assert!(format_native_metadata(&meta)
            .contains("flat=true linear=false fp16=true packed_pair=true"));
        program.data[e + 0x14c..e + 0x150].copy_from_slice(&2u32.to_le_bytes());
        assert!(program.native_metadata().is_err());
    }
}
