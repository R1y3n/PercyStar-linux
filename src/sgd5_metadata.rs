use crate::dxbc::{CompactRootSignature, DescriptorKind, RootBindingLocation, RootParameter};
use crate::sgd5::{
    Sgd5BindingListEntry, Sgd5BindingTable, Sgd5Effect, Sgd5File, Sgd5MemberRecord, Sgd5Pass,
    Sgd5ReflectionSource, Sgd5ReflectionTable, Sgd5ResourceRecord, Sgd5Shader,
    Sgd5VertexInputMetadata,
};
use std::collections::HashSet;
use std::fmt::Write as _;

pub fn resource_type_name(resource_type: u16) -> &'static str {
    match resource_type & !0x2080 {
        0x0002 => "Texture1D",
        0x0102 => "Texture2D",
        0x0142 => "Texture2DArray",
        0x0202 => "TextureCube",
        0x0242 => "TextureCubeArray",
        0x0302 => "Texture3D",
        0x0401 => "Buffer",
        0x0405 => "StructuredBuffer",
        0x0407 => "ByteAddressBuffer",
        0x001c => "RWTexture1D",
        0x011c => "RWTexture2D",
        0x015c => "RWTexture2DArray",
        0x031c => "RWTexture3D",
        0x040e => "Buffer subtype 0x0e",
        0x0414 => "RWStructuredBuffer",
        0x0416 => "RWStructuredBuffer subtype 0x16",
        0x0418 => "RWByteAddressBuffer",
        0x0423 => "SamplerState",
        0x0430 => "ConstantBuffer",
        _ => "unknown resource type",
    }
}

fn value_type(value_type: u8) -> (&'static str, Option<usize>) {
    match value_type {
        0 => ("bool", Some(4)),
        1 => ("uint", Some(4)),
        2 => ("uint2", Some(8)),
        3 => ("uint3", Some(12)),
        4 => ("uint4", Some(16)),
        5 => ("int", Some(4)),
        6 => ("int2", Some(8)),
        7 => ("int3", Some(12)),
        8 => ("int4", Some(16)),
        9 => ("float", Some(4)),
        10 => ("float2", Some(8)),
        11 => ("float3", Some(12)),
        12 => ("float4", Some(16)),
        13 => ("float4x3", Some(48)),
        14 => ("float4x4", Some(64)),
        _ => ("unknown value type", None),
    }
}

fn vertex_format_name(format: u8) -> &'static str {
    match format {
        2 => "R32G32B32A32_FLOAT",
        3 => "R32G32B32A32_UINT",
        4 => "R32G32B32A32_SINT",
        6 => "R32G32B32_FLOAT",
        7 => "R32G32B32_UINT",
        8 => "R32G32B32_SINT",
        10 => "R16G16B16A16_FLOAT",
        11 => "R16G16B16A16_UNORM",
        12 => "R16G16B16A16_UINT",
        13 => "R16G16B16A16_SNORM",
        14 => "R16G16B16A16_SINT",
        16 => "R32G32_FLOAT",
        17 => "R32G32_UINT",
        18 => "R32G32_SINT",
        24 => "R10G10B10A2_UNORM",
        25 => "R10G10B10A2_UINT",
        26 => "R11G11B10_FLOAT",
        28 => "R8G8B8A8_UNORM",
        30 => "R8G8B8A8_UINT",
        31 => "R8G8B8A8_SNORM",
        32 => "R8G8B8A8_SINT",
        34 => "R16G16_FLOAT",
        35 => "R16G16_UNORM",
        36 => "R16G16_UINT",
        37 => "R16G16_SNORM",
        38 => "R16G16_SINT",
        40 => "R32_TYPELESS",
        41 => "R32_FLOAT",
        42 => "R32_UINT",
        43 => "R32_SINT",
        _ => "unresolved DXGI format",
    }
}

fn input_mask_names(mask: u64) -> String {
    (0..52)
        .filter(|slot| mask & (1 << slot) != 0)
        .filter_map(|slot| {
            let (semantic, index) = Sgd5VertexInputMetadata::semantic(slot)?;
            Some(format!("{semantic}{index}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_vertex_inputs(
    text: &mut String,
    inputs: &Sgd5VertexInputMetadata,
    label: &str,
    format_label: &str,
) {
    let _ = writeln!(text, "{label}: mask=0x{:016x} flags=0x{:016x} VertexID={} InstanceID={} compact_formats={} reserved_44=0x{:08x}",
        inputs.attribute_mask, inputs.shader_flags, inputs.uses_vertex_id(), inputs.uses_instance_id(), inputs.compact_formats(), inputs.reserved_44);
    for slot in 0..52 {
        if inputs.attribute_mask & (1 << slot) == 0 {
            continue;
        }
        let (semantic, index) = Sgd5VertexInputMetadata::semantic(slot).unwrap();
        let format = inputs.dxgi_format(slot);
        let _ = writeln!(
            text,
            "  {semantic}{index}: semantic_slot={slot} {format_label}={} DXGI={} raw=0x{:02x}",
            format
                .map(vertex_format_name)
                .unwrap_or("unresolved format"),
            format
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unresolved".into()),
            inputs.attribute_formats[slot]
        );
    }
}

fn member_at(table: &Sgd5ReflectionTable, offset: usize) -> Option<&Sgd5MemberRecord> {
    let first = table.members.first()?.offset;
    let relative = offset.checked_sub(first)?;
    if relative % 24 != 0 {
        return None;
    }
    table
        .members
        .get(relative / 24)
        .filter(|member| member.offset == offset)
}

fn name(file: &Sgd5File, hash: u32, resolved: Option<&str>) -> String {
    resolved
        .or_else(|| file.resolve_name(hash))
        .unwrap_or("unresolved")
        .to_string()
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("signature offset overflow")?;
    let bytes = data.get(offset..end).ok_or("truncated signature data")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

pub fn format_dxbc_signatures(data: &[u8]) -> Result<String, String> {
    if data.get(..4) != Some(b"DXBC") {
        return Err("not a DXBC container".into());
    }
    let size = read_u32(data, 24)? as usize;
    let data = data
        .get(..size)
        .filter(|data| data.len() >= 32)
        .ok_or("invalid DXBC extent")?;
    let count = read_u32(data, 28)? as usize;
    let table_end = count
        .checked_mul(4)
        .and_then(|size| size.checked_add(32))
        .filter(|end| *end <= data.len())
        .ok_or("invalid DXBC chunk count")?;
    let mut text = String::from(
        "DXBC input/output signatures (signature registers, not ISA VGPR assignments)\n",
    );
    let mut found = false;
    for table_offset in (32..table_end).step_by(4) {
        let chunk_offset = read_u32(data, table_offset)? as usize;
        let tag = data
            .get(chunk_offset..chunk_offset.checked_add(4).ok_or("chunk offset overflow")?)
            .ok_or("invalid DXBC chunk offset")?;
        let (kind, extended) = match tag {
            b"ISG1" => ("input", true),
            b"OSG1" => ("output", true),
            b"PSG1" => ("patch constant", true),
            b"ISGN" => ("input", false),
            b"OSGN" => ("output", false),
            b"PCSG" => ("patch constant", false),
            _ => continue,
        };
        let payload_size = read_u32(
            data,
            chunk_offset.checked_add(4).ok_or("chunk offset overflow")?,
        )? as usize;
        let payload_start = chunk_offset.checked_add(8).ok_or("chunk offset overflow")?;
        let payload_end = payload_start
            .checked_add(payload_size)
            .ok_or("signature size overflow")?;
        let payload = data
            .get(payload_start..payload_end)
            .ok_or("truncated signature chunk")?;
        let element_count = read_u32(payload, 0)? as usize;
        if element_count > 4096 {
            return Err("signature element count exceeds metadata limit".into());
        }
        let elements_start = read_u32(payload, 4)? as usize;
        let stride = if extended { 32 } else { 24 };
        let elements_end = element_count
            .checked_mul(stride)
            .and_then(|size| elements_start.checked_add(size))
            .filter(|end| *end <= payload.len() && elements_start >= 8)
            .ok_or("invalid signature element count")?;
        found = true;
        let _ = writeln!(text, "  {kind}: {element_count} elements");
        for offset in (elements_start..elements_end).step_by(stride) {
            let row = &payload[offset..offset + stride];
            let base = if extended { 4 } else { 0 };
            let name_offset = read_u32(row, base)? as usize;
            if name_offset < elements_end {
                return Err("signature name overlaps records".into());
            }
            let bytes = payload
                .get(name_offset..)
                .ok_or("invalid signature name offset")?;
            let end = bytes
                .iter()
                .position(|byte| *byte == 0)
                .ok_or("unterminated signature name")?;
            if end > 1024 || text.len() > (4 << 20) {
                return Err("signature text exceeds metadata limit".into());
            }
            let semantic =
                std::str::from_utf8(&bytes[..end]).map_err(|_| "invalid signature name")?;
            let semantic_index = read_u32(row, base + 4)?;
            let system_value = read_u32(row, base + 8)?;
            let component_type = match read_u32(row, base + 12)? {
                1 => "uint32",
                2 => "int32",
                3 => "float32",
                _ => "unknown",
            };
            let register = read_u32(row, base + 16)?;
            let register = if register == u32::MAX {
                "none".into()
            } else {
                register.to_string()
            };
            let stream = if extended { read_u32(row, 0)? } else { 0 };
            let _ = writeln!(text, "    {semantic}[{semantic_index}] stream={stream} signature_register={register} mask=0x{:02x} rw_mask=0x{:02x} component={component_type} system_value={system_value}", row[base + 20], row[base + 21]);
        }
    }
    if !found {
        text.push_str("  no supported signature chunks\n");
    }
    Ok(text)
}

fn write_member(
    text: &mut String,
    file: &Sgd5File,
    table: &Sgd5ReflectionTable,
    member: &Sgd5MemberRecord,
) {
    let (type_name, _) = value_type(member.value_type);
    let array = if member.array_size == 0 {
        String::new()
    } else {
        format!("[{}]", member.array_size)
    };
    let extent = member.byte_size();
    let _ = writeln!(text, "    {}{} {} | type=0x{:02x} byte_offset={} byte_size={} name_hash=0x{:08x} secondary_hash=0x{:08x} parameter_key=0x{:016x}",
        type_name, array, name(file, member.name_hash, member.name.as_deref()), member.value_type,
        member.byte_offset, extent.map(|size| size.to_string()).unwrap_or_else(|| "unknown".into()), member.name_hash, member.secondary_hash, member.parameter_key());
    if member.default_value_offset != 0 {
        let target = member.offset + member.default_value_offset as usize;
        let table_end = table.offset + 4 + table.size as usize;
        let size = extent.unwrap_or(4);
        let available = table_end.saturating_sub(target);
        let preview_size = size.min(16).min(available);
        if target >= table.default_values_offset && target < table_end {
            if let Some(bytes) = file.data.get(target..target + preview_size) {
                let preview = bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = writeln!(
                    text,
                    "      default data at 0x{target:x}: {preview}{}",
                    if size > preview_size { " ..." } else { "" }
                );
            }
        }
    }
}

fn write_resource(
    text: &mut String,
    file: &Sgd5File,
    table: &Sgd5ReflectionTable,
    record: &Sgd5ResourceRecord,
) {
    let _ = writeln!(text, "  {} {} | type=0x{:04x} metadata_slot={} flags=0x{:02x} name_hash=0x{:08x} secondary_hash=0x{:08x} parameter_key=0x{:016x} cbuffer_bytes={} members={} auxiliary_records={}",
        resource_type_name(record.resource_type), name(file, record.name_hash, record.name.as_deref()),
        record.resource_type, record.slot, record.flags, record.name_hash, record.secondary_hash, record.parameter_key(),
        record.cbuffer_size, record.member_count, record.auxiliary_record_count);
    if record.member_count != 0 {
        let start = record.offset + record.member_data_offset as usize;
        for index in 0..record.member_count as usize {
            if let Some(member) = member_at(table, start + index * 24) {
                write_member(text, file, table, member);
            }
        }
    }
}

pub fn resource_kind(record: &Sgd5ResourceRecord) -> DescriptorKind {
    match (record.resource_type >> 4) & 3 {
        0 => DescriptorKind::Srv,
        1 => DescriptorKind::Uav,
        2 => DescriptorKind::Sampler,
        _ => DescriptorKind::Cbv,
    }
}

fn binding_kind(binding_type: u16) -> Option<DescriptorKind> {
    match binding_type & 255 {
        1 | 2 | 5 | 7 | 14 => Some(DescriptorKind::Srv),
        4 | 6 | 8 | 12 => Some(DescriptorKind::Uav),
        3 => Some(DescriptorKind::Sampler),
        0 => Some(DescriptorKind::Cbv),
        _ => None,
    }
}

pub fn format_root_location(location: RootBindingLocation) -> String {
    match location {
        RootBindingLocation::DescriptorTable { parameter, descriptor_index } => format!("root_parameter={parameter} descriptor_index={descriptor_index} descriptor_byte_offset=0x{:x}", 32 * descriptor_index),
        RootBindingLocation::Constants { parameter, dword_count } => format!("root_parameter={parameter} root_constants={dword_count} dwords"),
        RootBindingLocation::RootDescriptor { parameter } => format!("root_parameter={parameter} direct root descriptor"),
        RootBindingLocation::StaticSampler { index } => format!("static_sampler={index}"),
    }
}

fn write_compiled_binding(
    text: &mut String,
    record: &Sgd5ResourceRecord,
    root: Option<&CompactRootSignature>,
) {
    let kind = resource_kind(record);
    let register = record.shader_register();
    let space = record.register_space();
    let location = root.and_then(|root| root.find_binding(kind, register, space));
    let _ = writeln!(
        text,
        "    shader_binding={}{} space{} | {}",
        kind.register_prefix(),
        register,
        space,
        location
            .map(format_root_location)
            .unwrap_or_else(|| "compiled root binding unavailable".into())
    );
}

fn write_root_signature(text: &mut String, root: &CompactRootSignature) {
    let _ = writeln!(text, "Compiled PRIV root signature (D3D version 1.1): flags=0x{:04x} unknown=0x{:04x} parameters={} static_samplers={}", root.flags, root.unknown, root.parameters.len(), root.static_samplers.len());
    let _ = writeln!(
        text,
        "  direct_heap_indexing: resources={} samplers={} | padding_bytes={}",
        root.flags & 0x400 != 0,
        root.flags & 0x800 != 0,
        root.padding.len()
    );
    for (index, parameter) in root.parameters.iter().enumerate() {
        match parameter {
            RootParameter::DescriptorTable { space, ranges } => {
                let _ = writeln!(text, "  root[{index}] descriptor table space{space}: {} descriptors, 32 bytes per descriptor", ranges.len());
            }
            RootParameter::Constants {
                register,
                dword_count,
            } => {
                let _ = writeln!(
                    text,
                    "  root[{index}] constants b{register} space0: {dword_count} dwords"
                );
            }
            RootParameter::Descriptor {
                kind,
                register,
                space,
                unknown_bits,
            } => {
                let _ = writeln!(text, "  root[{index}] direct {}{register} space{space}: unknown_bits=0x{unknown_bits:02x}", kind.register_prefix());
            }
        }
    }
    for (index, sampler) in root.static_samplers.iter().enumerate() {
        let _ = writeln!(text, "  sampler[{index}] s{} space{} filter=0x{:04x} address={}/{}/{} anisotropy={} comparison={} border={} mip_bias={} LOD={}..{}",
            sampler.register, sampler.space, sampler.filter, sampler.address_u, sampler.address_v, sampler.address_w,
            sampler.max_anisotropy, sampler.comparison, sampler.border_color, sampler.mip_bias, sampler.min_lod, sampler.max_lod);
    }
}

fn resolve_binding_table<'a>(
    file: &'a Sgd5File,
    effect: &'a Sgd5Effect,
    selector: u8,
) -> Option<&'a Sgd5BindingTable> {
    file.binding_table(effect, selector)
}

fn write_pass_bindings(
    text: &mut String,
    file: &Sgd5File,
    effect: &Sgd5Effect,
    pass: &Sgd5Pass,
    shader: &Sgd5Shader,
    roots: &mut std::collections::HashMap<(&'static str, u32), Option<CompactRootSignature>>,
) {
    let selected_stage = if pass.shader_slots[5] != 0 {
        Some(5)
    } else {
        (0..5).rev().find(|stage| pass.shader_slots[*stage] != 0)
    };
    let Some(stage) = selected_stage else {
        return;
    };
    let Some(index) = pass.shader_slots[stage]
        .checked_sub(1)
        .and_then(|slot| effect.stage_indices[stage].get(usize::from(slot)))
    else {
        return;
    };
    let stages = ["VS", "PS", "GS", "DS", "HS", "CS"];
    let Some(owner) = file.shader(stages[stage], *index) else {
        return;
    };
    let key = (owner.stage, owner.index);
    let root = roots.entry(key).or_insert_with(|| {
        crate::sgd5::decompress_shader(file, owner)
            .ok()
            .and_then(|blob| crate::dxbc::extract_compact_root_signature(&blob).ok())
    });
    let Some(root) = root else {
        let _ = writeln!(
            text,
            "      compiled root unavailable for {}#{}",
            owner.stage, owner.index
        );
        return;
    };
    let selectors: [u8; 4] = if stage == 5 {
        owner.compute_binding_selectors().unwrap()
    } else {
        pass.binding_table_indices
    };
    let _ = writeln!(
        text,
        "      runtime root: {}#{} | space0..3 binding selectors={selectors:?}",
        owner.stage, owner.index
    );
    for (space, selector) in selectors
        .into_iter()
        .enumerate()
        .filter(|(_, selector)| *selector != 255)
    {
        let Some(table) = resolve_binding_table(file, effect, selector) else {
            continue;
        };
        let Some((parameter, ranges)) =
            root.parameters
                .iter()
                .enumerate()
                .find_map(|(index, entry)| match entry {
                    RootParameter::DescriptorTable {
                        space: root_space,
                        ranges,
                    } if usize::from(*root_space) == space => Some((index, ranges)),
                    _ => None,
                })
        else {
            continue;
        };
        let layout_matches = ranges.len() == table.entries.len()
            && table.entries.iter().all(|entry| {
                ranges
                    .get(entry.descriptor_index())
                    .is_some_and(|range| Some(range.kind) == binding_kind(entry.binding_type))
            });
        if !layout_matches {
            let _ = writeln!(
                text,
                "      table[{selector}] space{space}: layout does not match compiled root"
            );
            continue;
        }
        for entry in &table.entries {
            let range = &ranges[entry.descriptor_index()];
            if !shader.reflection.resources.iter().any(|record| {
                record.name_hash == entry.name_hash
                    && record.register_space() as usize == space
                    && resource_kind(record) == range.kind
                    && record.shader_register() == range.register
            }) {
                continue;
            }
            let _ = writeln!(
                text,
                "      table[{selector}] {} -> {}{} space{} | {}",
                name(file, entry.name_hash, entry.name.as_deref()),
                range.kind.register_prefix(),
                range.register,
                space,
                format_root_location(RootBindingLocation::DescriptorTable {
                    parameter,
                    descriptor_index: entry.descriptor_index()
                })
            );
        }
    }
    for record in &shader.reflection.resources {
        let kind = resource_kind(record);
        let register = record.shader_register();
        let space = record.register_space();
        if let Some(location) = root.find_binding(kind, register, space) {
            if !matches!(location, RootBindingLocation::DescriptorTable { .. }) {
                let _ = writeln!(
                    text,
                    "      runtime {} -> {}{register} space{space} | {}",
                    name(file, record.name_hash, record.name.as_deref()),
                    kind.register_prefix(),
                    format_root_location(location)
                );
            }
        }
    }
}

fn pass_uses_shader(
    effect: &Sgd5Effect,
    pass: &Sgd5Pass,
    shader: &Sgd5Shader,
    stage: usize,
) -> bool {
    pass.shader_slots[stage]
        .checked_sub(1)
        .and_then(|slot| effect.stage_indices[stage].get(slot as usize))
        == Some(&shader.index)
}

fn write_effect_passes(
    text: &mut String,
    effect: &Sgd5Effect,
    shader: &Sgd5Shader,
    stage: usize,
    shader_hashes: &HashSet<u32>,
    file: &Sgd5File,
    roots: &mut std::collections::HashMap<(&'static str, u32), Option<CompactRootSignature>>,
) {
    let metadata = &effect.metadata;
    let _ = writeln!(
        text,
        "  states: rasterizer={} depth_stencil={} blend={} sampler={} | techniques={} passes={}",
        metadata.rasterizer_states.len(),
        metadata.depth_stencil_states.len(),
        metadata.blend_states.len(),
        metadata.sampler_states.len(),
        metadata.techniques.len(),
        metadata.passes.len()
    );
    let source = match effect.shared_reflection_source {
        Sgd5ReflectionSource::Shared => "archive shared reflection",
        Sgd5ReflectionSource::Inline(_) => "inline reflection",
        Sgd5ReflectionSource::Absent => "absent",
    };
    let _ = writeln!(text, "  effect reflection source: {source}");
    write_vertex_inputs(
        text,
        &metadata.header.vertex_inputs,
        "  Effect vertex-input summary",
        "format_candidate",
    );
    let _ = writeln!(text, "  Aggregate formats use the inferred VS encoding; their merge rule and direct runtime consumer are unconfirmed.");
    let _ = writeln!(
        text,
        "  additional_attribute_mask_48=0x{:016x}: {} (selection policy unconfirmed)",
        metadata.header.attribute_mask_48,
        input_mask_names(metadata.header.attribute_mask_48)
    );
    for technique in &metadata.techniques {
        let start = technique.first_pass as usize;
        let end = start + technique.pass_count as usize;
        let Some(passes) = metadata.passes.get(start..end) else {
            continue;
        };
        if !passes
            .iter()
            .any(|pass| pass_uses_shader(effect, pass, shader, stage))
        {
            continue;
        }
        let _ = writeln!(
            text,
            "  technique {}: first_pass={} pass_count={}",
            technique.name.as_deref().unwrap_or("unresolved"),
            technique.first_pass,
            technique.pass_count
        );
        for (local_index, pass) in passes.iter().enumerate() {
            if !pass_uses_shader(effect, pass, shader, stage) {
                continue;
            }
            let selections = ["VS", "PS", "GS", "DS", "HS", "CS"]
                .iter()
                .enumerate()
                .filter_map(|(stage, name)| {
                    let slot = pass.shader_slots[stage].checked_sub(1)? as usize;
                    Some(format!("{name}#{}", effect.stage_indices[stage].get(slot)?))
                })
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(
                text,
                "    pass[{}] {selections} | binding_table_indices={:?} state_flags=0x{:02x}",
                start + local_index,
                pass.binding_table_indices,
                pass.state_flags
            );
            if pass.state_flags & 1 != 0 {
                if let Some(state) = metadata
                    .rasterizer_states
                    .get(pass.rasterizer_state_index as usize)
                {
                    let _ = writeln!(text, "      rasterizer[{}] fill={} cull={} front_ccw={} depth_bias={} clamp={} slope_bias={} depth_clip={} multisample={} flags=0x{:02x}",
                        pass.rasterizer_state_index, state.fill_mode, state.cull_mode, state.front_counter_clockwise,
                        state.depth_bias, state.depth_bias_clamp, state.slope_scaled_depth_bias, state.depth_clip_enable,
                        state.multisample_enable, state.flags);
                }
            }
            if pass.state_flags & 2 != 0 {
                if let Some(state) = metadata
                    .depth_stencil_states
                    .get(pass.depth_stencil_state_index as usize)
                {
                    let _ = writeln!(text, "      depth_stencil[{}] depth_enable={} write_mask={} comparison={} stencil_enable={} stencil_masks={:02x}/{:02x} depth_bounds={}",
                        pass.depth_stencil_state_index, state.depth_enable, state.depth_write_mask,
                        state.depth_comparison_func, state.stencil_enable, state.stencil_read_mask, state.stencil_write_mask,
                        state.depth_bounds_test_enable);
                }
            }
            if pass.state_flags & 4 != 0 {
                if let Some(state) = metadata.blend_states.get(pass.blend_state_index as usize) {
                    let target = &state.render_targets[0];
                    let _ = writeln!(text, "      blend[{}] alpha_to_coverage={} independent={} RT0_enable={} RT0_write_mask=0x{:x}",
                        pass.blend_state_index, state.alpha_to_coverage_enable, state.independent_blend_enable,
                        target.blend_enable, target.write_mask);
                }
            }
            if let Some(reference) = pass.selected_stencil_reference() {
                let _ = writeln!(text, "      stencil_reference={reference}");
            }
            if let Some(usage) = &pass.resource_usage {
                let groups = [
                    &usage.srv_hashes,
                    &usage.uav_hashes,
                    &usage.sampler_hashes,
                    &usage.cbv_hashes,
                ];
                let matching = groups
                    .iter()
                    .flat_map(|group| group.iter())
                    .filter(|hash| shader_hashes.contains(hash))
                    .count();
                let _ = writeln!(text, "      logical resource usage: SRV={} UAV={} sampler={} CBV={} matching_shader_hashes={matching}",
                    usage.srv_hashes.len(), usage.uav_hashes.len(), usage.sampler_hashes.len(), usage.cbv_hashes.len());
                if let Some(binding) = usage.texture_handle_binding {
                    let _ = writeln!(text, "      texture_handle_binding=0x{binding:08x} | root_constant_dword={} fallback_cbv={}",
                        if binding as u8 == 255 { "absent".into() } else { (binding & 255).to_string() }, binding < 256);
                }
            }
            write_pass_bindings(text, file, effect, pass, shader, roots);
        }
    }
    for (index, sampler) in metadata.sampler_states.iter().enumerate() {
        let descriptor = sampler.descriptor();
        let _ = writeln!(text, "  sampler[{index}] preset={} addressUVW={:?} lod_bias={} min_lod={} max_lod={} border={:?} filter_bits=0x{:04x} comparison_bits=0x{:02x} max_anisotropy={}",
            descriptor.preset_index, descriptor.address_modes, descriptor.mip_lod_bias, descriptor.min_lod,
            descriptor.max_lod, descriptor.border_color, descriptor.filter_bits, descriptor.comparison_bits, descriptor.max_anisotropy);
    }
}

fn write_binding_table(
    text: &mut String,
    index: usize,
    table: &Sgd5BindingTable,
    source: &str,
    shader_hashes: &HashSet<u32>,
    file: &Sgd5File,
) {
    let header = &table.header;
    let matches = table
        .entries
        .iter()
        .filter(|entry| shader_hashes.contains(&entry.name_hash))
        .count();
    let _ = writeln!(text, "  table[{index}] {source} signature=0x{:016x} entries={} SRV={} UAV={} sampler={} CBV={} matching_shader_hashes={matches}",
        table.hash, header.entry_count, header.srv_count, header.uav_count, header.sampler_count, header.cbv_count);
    for entry in table
        .entries
        .iter()
        .filter(|entry| shader_hashes.contains(&entry.name_hash))
    {
        let _ = writeln!(
            text,
            "    {} | name_hash=0x{:08x} binding_type=0x{:04x} table_index={}",
            name(file, entry.name_hash, entry.name.as_deref()),
            entry.name_hash,
            entry.binding_type,
            entry.binding_index
        );
    }
}

pub fn format_shader_metadata(file: &Sgd5File, shader: &Sgd5Shader) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "SGD5 container metadata");
    let _ = writeln!(
        text,
        "Shader: {} #{} {}",
        shader.stage, shader.index, shader.name
    );
    let _ = writeln!(
        text,
        "Group hash: 0x{:016x} | SGA hash: 0x{:016x} | shader-version byte: {}",
        file.group_hash, shader.sga_hash, shader.shader_version
    );
    let _ = writeln!(
        text,
        "Archive: {} bytes -> {} bytes | reflection at 0x{:x}",
        shader.archive.archive_size, shader.archive.decompressed_size, shader.reflection.offset
    );
    let _ = writeln!(
        text,
        "Resource types describe SGD5 metadata; texture/buffer element types are not inferred."
    );
    let _ = writeln!(text, "Per-shader slots encode registers with resource-type bits 7/13; flags bits 6..7 encode register space. Effect material SRV slots index texture handles.");
    let _ = writeln!(text, "Both hashes support parameter lookup. Defaults initialize runtime CBV members; SGPR/VGPR assignments require ISA analysis.");
    let (srvs, uavs, constants) = shader.root_argument_counts();
    let _ = writeln!(text, "Shader root-argument counts: direct SRVs={srvs}, direct UAVs={uavs}, constant dwords={constants}");
    if let Some(selectors) = shader.compute_binding_selectors() {
        let _ = writeln!(
            text,
            "Compute binding selectors space0..3: {selectors:?} (255 absent)"
        );
    }
    text.push('\n');
    if let Some(inputs) = shader.vertex_inputs() {
        write_vertex_inputs(&mut text, &inputs, "Shader vertex-input metadata", "format");
    }
    let mut compiled_root = None;
    match crate::sgd5::decompress_shader(file, shader) {
        Ok(blob) => {
            match crate::dxbc::extract_compact_root_signature(&blob) {
                Ok(root) => {
                    write_root_signature(&mut text, &root);
                    compiled_root = Some(root);
                }
                Err(error) => {
                    let _ = writeln!(text, "Compiled root signature unavailable: {error}");
                }
            }
            match format_dxbc_signatures(&blob) {
                Ok(signatures) => text.push_str(&signatures),
                Err(error) => {
                    let _ = writeln!(text, "DXBC signatures unavailable: {error}");
                }
            }
            match crate::dxbc::extract_fssm_programs(&blob) {
                Ok(programs) => {
                    let _ = writeln!(text, "Native program variants: {}", programs.len());
                    for (index, program) in programs.iter().enumerate() {
                        let _ =
                            writeln!(text, "Variant {index} ({})", program.hardware_stage().tag());
                        text.push_str(&crate::dxbc::format_fssm_metadata(program));
                    }
                }
                Err(error) => {
                    let _ = writeln!(text, "FSSM program metadata unavailable: {error}");
                }
            }
        }
        Err(error) => {
            let _ = writeln!(text, "DXBC signatures unavailable: {error}");
        }
    }
    let _ = writeln!(
        text,
        "\nShader resources ({})",
        shader.reflection.resources.len()
    );
    for resource in &shader.reflection.resources {
        write_resource(&mut text, file, &shader.reflection, resource);
        write_compiled_binding(&mut text, resource, compiled_root.as_ref());
    }
    let mut roots = std::collections::HashMap::new();
    text.push_str(&format_parameter_mapping(
        file,
        shader,
        compiled_root.as_ref(),
    ));
    roots.insert((shader.stage, shader.index), compiled_root);
    let referenced_members: HashSet<_> = shader
        .reflection
        .resources
        .iter()
        .flat_map(|resource| {
            (0..resource.member_count as usize).map(move |index| {
                resource.offset + resource.member_data_offset as usize + index * 24
            })
        })
        .collect();
    for member in shader
        .reflection
        .members
        .iter()
        .filter(|member| !referenced_members.contains(&member.offset))
    {
        write_member(&mut text, file, &shader.reflection, member);
    }
    let shader_hashes: HashSet<_> = shader
        .reflection
        .resources
        .iter()
        .map(|record| record.name_hash)
        .chain(
            shader
                .reflection
                .members
                .iter()
                .map(|record| record.name_hash),
        )
        .collect();
    let shared_matches = file
        .shared_reflection
        .resources
        .iter()
        .filter(|record| shader_hashes.contains(&record.name_hash))
        .count();
    let _ = writeln!(
        text,
        "\nShared reflection: {} resources, {} members; {shared_matches} resource hash matches",
        file.shared_reflection.resources.len(),
        file.shared_reflection.members.len()
    );
    for resource in file
        .shared_reflection
        .resources
        .iter()
        .filter(|record| shader_hashes.contains(&record.name_hash))
    {
        write_resource(&mut text, file, &file.shared_reflection, resource);
    }
    let stage_index = ["VS", "PS", "GS", "DS", "HS", "CS"]
        .iter()
        .position(|stage| *stage == shader.stage);
    let effect_indices =
        stage_index.and_then(|stage| file.effect_shader_map.get(&(stage as u8, shader.index)));
    let _ = writeln!(
        text,
        "\nEffects referencing this shader: {}",
        effect_indices.map_or(0, Vec::len)
    );
    for effect_index in effect_indices.into_iter().flatten() {
        let Some(effect) = file.effects.get(*effect_index) else {
            continue;
        };
        let _ = writeln!(
            text,
            "\nEffect: {} | data_buffer_size={}",
            effect.name, effect.data_buffer_size
        );
        let stages = ["VS", "PS", "GS", "DS", "HS", "CS"];
        let stage_counts = effect
            .stage_indices
            .iter()
            .enumerate()
            .filter(|(_, indices)| !indices.is_empty())
            .map(|(index, indices)| format!("{}={}", stages[index], indices.len()))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(text, "  stage shader references: {stage_counts}");
        if let Some(stage) = stage_index {
            write_effect_passes(
                &mut text,
                effect,
                shader,
                stage,
                &shader_hashes,
                file,
                &mut roots,
            );
        }
        let _ = writeln!(
            text,
            "  reflection: {} resources, {} members, {} auxiliary records; {} default bytes",
            effect.main_reflection.resources.len(),
            effect.main_reflection.members.len(),
            effect.main_reflection.auxiliary_records.len(),
            effect.main_reflection.default_value_data.len()
        );
        let _ = writeln!(
            text,
            "  property reflection (proplst): {} resources, {} members, {} auxiliary records",
            effect.property_reflection.resources.len(),
            effect.property_reflection.members.len(),
            effect.property_reflection.auxiliary_records.len()
        );
        let _ = writeln!(
            text,
            "  effect string pool (includes confirmed technique names): {}",
            effect.strings.join(", ")
        );
        for resource in &effect.main_reflection.resources {
            let target = resource.offset + resource.member_data_offset as usize;
            let member_match = (0..resource.member_count as usize).any(|index| {
                member_at(&effect.main_reflection, target + index * 24)
                    .is_some_and(|member| shader_hashes.contains(&member.name_hash))
            });
            if shader_hashes.contains(&resource.name_hash) || member_match {
                write_resource(&mut text, file, &effect.main_reflection, resource);
            }
        }
        let _ = writeln!(
            text,
            "  Binding table inventory (selected pass mappings shown above):"
        );
        for (index, property) in effect.binding_list.entries.iter().enumerate() {
            match property {
                Sgd5BindingListEntry::Inline(table) => {
                    write_binding_table(&mut text, index, table, "inline", &shader_hashes, file)
                }
                Sgd5BindingListEntry::TableReference { table_hash, .. } => {
                    if let Some(table) = file
                        .binding_tables
                        .iter()
                        .find(|table| table.header.table_hash == *table_hash)
                    {
                        write_binding_table(
                            &mut text,
                            index,
                            table,
                            &format!("shared hash=0x{table_hash:08x}"),
                            &shader_hashes,
                            file,
                        );
                    }
                }
            }
        }
    }
    text
}

pub fn prepend_comments(metadata: &str, prefix: &str, output: String) -> String {
    let mut text = String::with_capacity(metadata.len() + output.len() + 256);
    for line in metadata.lines() {
        let _ = writeln!(text, "{prefix} {line}");
    }
    text.push('\n');
    text.push_str(&output);
    text
}

/// Exact logical parameter/register/root mapping. ISA register allocation is kept separate.
pub fn format_parameter_mapping(
    file: &Sgd5File,
    shader: &Sgd5Shader,
    root: Option<&CompactRootSignature>,
) -> String {
    let mut text = String::from(
        "Parameter mapping: resource identity -> compiled register/space -> compiled shader root location\n",
    );
    for record in &shader.reflection.resources {
        let kind = resource_kind(record);
        let register = record.shader_register();
        let space = record.register_space();
        let location = root.and_then(|root| root.find_binding(kind, register, space));
        let _ = writeln!(
            text,
            "  {} [hash=0x{:08x}, alternate=0x{:08x}] -> {}{register}, space{space} -> {}",
            name(file, record.name_hash, record.name.as_deref()),
            record.name_hash,
            record.secondary_hash,
            kind.register_prefix(),
            location
                .map(format_root_location)
                .unwrap_or_else(|| "root unavailable".into())
        );
        if record.member_count != 0 {
            for index in 0..usize::from(record.member_count) {
                let target = record.offset + usize::from(record.member_data_offset) + index * 24;
                if let Some(member) = member_at(&shader.reflection, target) {
                    let _ = writeln!(
                        text,
                        "    {} {} -> parent byte offset {} ({} bytes)",
                        value_type(member.value_type).0,
                        name(file, member.name_hash, member.name.as_deref()),
                        member.byte_offset,
                        member
                            .byte_size()
                            .map(|size| size.to_string())
                            .unwrap_or_else(|| "unknown".into())
                    );
                }
            }
        }
    }
    text
}

pub fn format_native_parameter_mapping(
    file: &Sgd5File,
    shader: &Sgd5Shader,
    root: &CompactRootSignature,
    native: &crate::dxbc::FssmNativeMetadata,
) -> String {
    let mut text = format!(
        "\nCompiler resource usage ({}, verified against PRIV):\n",
        native.stage.tag()
    );
    for usage in &native.used_resources {
        let Some((kind, register, space, location)) = usage.binding(root) else {
            let _ = writeln!(
                text,
                "  unresolved kind={} register={} {:?} slot={} (raw compiler record preserved)",
                usage.resource_kind_raw,
                usage.lower_bound_register,
                usage.root_type,
                usage.root_slot
            );
            continue;
        };
        let resource = shader.reflection.resources.iter().find(|record| {
            resource_kind(record) == kind
                && record.shader_register() == register
                && record.register_space() == space
        });
        let label = resource
            .map(|record| name(file, record.name_hash, record.name.as_deref()))
            .unwrap_or_else(|| "unnamed compiled resource".into());
        let input = if kind == DescriptorKind::Sampler {
            format!("compiler sampler slot {} (static)", usage.root_slot)
        } else {
            let width = native
                .root_layout
                .iter()
                .find(|p| {
                    p.root_type == usage.root_type
                        && p.root_slot == usage.root_slot
                        && p.base_register == register
                        && p.table_offset_dwords == usage.table_offset_dwords
                })
                .map(|p| p.user_sgpr_count);
            width
                .map(|width| {
                    crate::dxbc::format_user_data(native.stage, usage.first_user_data_dword, width)
                })
                .unwrap_or_else(|| format!("compiler user-data {}", usage.first_user_data_dword))
        };
        let _ = writeln!(text, "  {label} -> {}{register}, space{space} -> {} -> {input}; table bytes={}, descriptor={} dwords, stride={}",
            kind.register_prefix(), format_root_location(location), u64::from(usage.table_offset_dwords) * 4,
            usage.descriptor_dword_count, usage.stride);
    }
    text.push_str("Compiler inputs describe the entry ABI; individual instruction uses still require data-flow analysis.\n");
    text
}

pub fn format_pass_metadata(file: &Sgd5File, effect_index: usize, pass_index: usize) -> String {
    let Some(effect) = file.effects.get(effect_index) else {
        return String::new();
    };
    let Some(pass) = effect.metadata.passes.get(pass_index) else {
        return String::new();
    };
    let mut text = format!(
        "Selected context: effect {} / pass {pass_index}\n",
        effect.name
    );
    let _ = writeln!(
        text,
        "  state_flags=0x{:02x} | material_texture_handles={}",
        pass.state_flags, effect.binding_list.texture_handle_count
    );
    if let Some(usage) = &pass.resource_usage {
        for (label, hashes) in [
            ("SRV", &usage.srv_hashes),
            ("UAV", &usage.uav_hashes),
            ("sampler", &usage.sampler_hashes),
            ("CBV", &usage.cbv_hashes),
        ] {
            let _ = writeln!(
                text,
                "  logical {label} usage: {}",
                hashes
                    .iter()
                    .map(|hash| format!("{} [0x{hash:08x}]", name(file, *hash, None)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Some(binding) = usage.texture_handle_binding {
            let destination = if binding as u8 == 255 {
                "absent".into()
            } else {
                (binding & 255).to_string()
            };
            let _ = writeln!(text, "  material handle-array control=0x{binding:08x}: root-constant dword={destination}, CBV fallback={}", binding < 256);
        }
    }
    let mut roots = std::collections::HashMap::new();
    for stage in 0..6 {
        if let Some(shader) = file.pass_shader(effect, pass, stage) {
            let _ = writeln!(text, "  {}#{} {}:", shader.stage, shader.index, shader.name);
            write_pass_bindings(&mut text, file, effect, pass, shader, &mut roots);
        }
    }
    for record in &effect.main_reflection.resources {
        if resource_kind(record) == DescriptorKind::Srv
            && record.flags & 0xc0 == 0x80
            && record.slot < effect.binding_list.texture_handle_count
        {
            let _ = writeln!(
                text,
                "  material texture {} [0x{:08x}] -> texture_handles[{}] -> descriptor-heap index",
                name(file, record.name_hash, record.name.as_deref()),
                record.name_hash,
                record.slot
            );
        }
    }
    text
}

pub fn format_auxiliary_value(
    file: &Sgd5File,
    table: &Sgd5ReflectionTable,
    record: &crate::sgd5::Sgd5AuxiliaryRecord,
) -> String {
    match record.value_kind() {
        0 => format!("{} (0x{:08x})", record.data, record.data),
        1 => format!("{}", f32::from_bits(record.data)),
        2 => {
            let value = record
                .offset
                .checked_add(record.data as usize)
                .filter(|start| *start >= table.offset + 4 && *start < table.default_values_offset)
                .and_then(|start| file.data.get(start..table.default_values_offset))
                .and_then(|bytes| {
                    bytes
                        .iter()
                        .position(|b| *b == 0)
                        .map(|end| String::from_utf8_lossy(&bytes[..end]).into_owned())
                });
            value
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| format!("unresolved string +0x{:x}", record.data))
        }
        _ => format!("unknown kind: 0x{:08x}", record.data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_family_labels_preserve_uncertain_element_types() {
        assert_eq!(resource_type_name(0x0182), "Texture2D");
        assert_eq!(resource_type_name(0x04b0), "ConstantBuffer");
        assert_eq!(resource_type_name(0x048e), "Buffer subtype 0x0e");
        assert_eq!(resource_type_name(0x2485), "StructuredBuffer");
        assert_eq!(value_type(0x0f).1, None);
    }

    #[test]
    #[ignore = "requires PERCY_SGD5_SAMPLE pointing to the analysis archive"]
    fn archive_roots_match_shader_registers_and_selected_binding_tables() {
        let path = std::path::PathBuf::from(
            std::env::var_os("PERCY_SGD5_SAMPLE").expect("set PERCY_SGD5_SAMPLE"),
        );
        let files = crate::sgd5::load_sgd5(&path).unwrap();
        let mut counts = [0usize; 5];
        for file in &files {
            let mut roots = std::collections::HashMap::new();
            for shader in &file.shaders {
                let blob = crate::sgd5::decompress_shader(file, shader).unwrap();
                let root = crate::dxbc::extract_compact_root_signature(&blob).unwrap();
                if shader.stage == "CS" {
                    assert!(
                        crate::dxbc::extract_fssm(&blob)
                            .unwrap()
                            .compute_metadata()
                            .is_some(),
                        "{}",
                        shader.name
                    );
                }
                for record in &shader.reflection.resources {
                    assert!(
                        root.find_binding(
                            resource_kind(record),
                            record.shader_register(),
                            record.register_space()
                        )
                        .is_some(),
                        "{} hash={:08x}",
                        shader.name,
                        record.name_hash
                    );
                    counts[1] += 1;
                    counts[2] += usize::from(record.resource_type & 0x2080 != 0);
                }
                let word = u64::from_le_bytes(shader.shader_metadata[8..16].try_into().unwrap());
                let constants: u64 = root
                    .parameters
                    .iter()
                    .map(|p| match p {
                        RootParameter::Constants { dword_count, .. } => u64::from(*dword_count),
                        _ => 0,
                    })
                    .sum();
                let direct_count = |kind| {
                    root.parameters.iter().filter(|p| matches!(p, RootParameter::Descriptor { kind: k, .. } if *k == kind)).count() as u64
                };
                assert_eq!(constants, (word >> 58) & 15);
                assert_eq!(direct_count(DescriptorKind::Srv), (word >> 53) & 7);
                assert_eq!(direct_count(DescriptorKind::Uav), (word >> 47) & 7);
                roots.insert((shader.stage, shader.index), root);
                counts[0] += 1;
            }
            for effect in &file.effects {
                for pass in &effect.metadata.passes {
                    let stage = if pass.shader_slots[5] != 0 {
                        Some(5)
                    } else {
                        (0..5).rev().find(|s| pass.shader_slots[*s] != 0)
                    };
                    let Some(stage) = stage else {
                        continue;
                    };
                    let index =
                        effect.stage_indices[stage][usize::from(pass.shader_slots[stage] - 1)];
                    let stage_name = ["VS", "PS", "GS", "DS", "HS", "CS"][stage];
                    let shader = file
                        .shaders
                        .iter()
                        .find(|s| s.stage == stage_name && s.index == index)
                        .unwrap();
                    let root = &roots[&(shader.stage, index)];
                    let selectors: [u8; 4] = if stage == 5 {
                        shader.shader_metadata[88..92].try_into().unwrap()
                    } else {
                        pass.binding_table_indices
                    };
                    for (space, selector) in
                        selectors.into_iter().enumerate().filter(|(_, s)| *s != 255)
                    {
                        let table = resolve_binding_table(file, effect, selector).unwrap();
                        let ranges = root
                            .parameters
                            .iter()
                            .find_map(|p| match p {
                                RootParameter::DescriptorTable { space: s, ranges }
                                    if usize::from(*s) == space =>
                                {
                                    Some(ranges)
                                }
                                _ => None,
                            })
                            .unwrap();
                        assert_eq!(
                            ranges.len(),
                            table.entries.len(),
                            "{} space{space}",
                            effect.name
                        );
                        for entry in &table.entries {
                            let range = &ranges[usize::from(entry.binding_index)];
                            assert_eq!(Some(range.kind), binding_kind(entry.binding_type));
                            counts[4] += 1;
                        }
                        counts[3] += 1;
                    }
                    let usage = pass.resource_usage.as_ref().unwrap();
                    assert_eq!(
                        usage.texture_handle_binding.is_some(),
                        effect.binding_list.texture_handle_count != 0
                    );
                    if let Some(binding) = usage.texture_handle_binding {
                        assert!(matches!(binding, 0x100 | 0x101 | 0x102 | 0x104 | 0x1ff));
                        if binding as u8 != 255 {
                            let total: u32 = root
                                .parameters
                                .iter()
                                .map(|p| match p {
                                    RootParameter::Constants { dword_count, .. } => {
                                        u32::from(*dword_count)
                                    }
                                    _ => 0,
                                })
                                .sum();
                            assert!(
                                (binding & 127) < total,
                                "{} binding={binding:x}",
                                effect.name
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(counts, [26_503, 413_468, 59_560, 76_603, 5_274_062]);
    }

    #[test]
    fn metadata_is_commented_without_creating_hlsl_bindings() {
        let text = prepend_comments(
            "metadata_slot=19\nfloat4[12] byte_offset=304",
            "//",
            "void VSMain() {}".into(),
        );
        assert!(text.starts_with("// metadata_slot=19\n// float4[12] byte_offset=304\n"));
        assert!(!text.contains("register(t19)"));
        assert!(text.ends_with("void VSMain() {}"));
    }

    #[test]
    fn signature_chunks_report_semantics_and_bound_all_offsets() {
        let mut payload = Vec::new();
        for word in [1u32, 8, 0, 40, 0, 0, 3, 3, 0x00000f0f, 0] {
            payload.extend_from_slice(&word.to_le_bytes());
        }
        payload.extend_from_slice(b"POSITION\0");
        let mut data = vec![0; 36];
        data[..4].copy_from_slice(b"DXBC");
        data[28..32].copy_from_slice(&1u32.to_le_bytes());
        data[32..36].copy_from_slice(&36u32.to_le_bytes());
        data.extend_from_slice(b"ISG1");
        data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&payload);
        let size = data.len() as u32;
        data[24..28].copy_from_slice(&size.to_le_bytes());
        let text = format_dxbc_signatures(&data).unwrap();
        assert!(text.contains("POSITION[0] stream=0 signature_register=3 mask=0x0f"));
        assert!(text.contains("component=float32"));
        data[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(format_dxbc_signatures(&data).is_err());
        data[28..32].copy_from_slice(&1u32.to_le_bytes());
        data[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(format_dxbc_signatures(&data).is_err());
    }
}
