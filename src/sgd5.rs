use flate2::read::DeflateDecoder;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"SGD5";
const COMPRESSED_MAGIC: u32 = 0x000D_DCE7;
const TABLE_REFERENCE: u32 = 0x8000_0000;
const REFLECTION_RECORD_SIZE: usize = 24;
const MAX_SHADER_BYTES: u32 = 64 << 20;
pub const STAGES: [(&str, &str); 6] = [
    ("VS", "vs"),
    ("PS", "ps"),
    ("GS", "gs"),
    ("DS", "ds"),
    ("HS", "hs"),
    ("CS", "cs"),
];

#[derive(Clone, Debug, Default)]
pub struct Sgd5ShaderArchive {
    pub offset: usize,
    pub archive_size: u32,
    pub decompressed_size: u32,
}

#[derive(Clone, Debug)]
pub struct Sgd5Shader {
    pub stage: &'static str,
    pub index: u32,
    pub name: String,
    /// Serialized shader-version/profile code, not the compiled wave size.
    pub shader_version: u8,
    pub archive: Sgd5ShaderArchive,
    pub sga_hash: u64,
    /// Engine shader metadata. Compiled root signatures are stored in DXBC PRIV.
    pub shader_metadata: [u8; 0x90],
    pub reflection: Sgd5ReflectionTable,
}

impl Sgd5Shader {
    pub fn root_argument_counts(&self) -> (u8, u8, u8) {
        let word = u64::from_le_bytes(self.shader_metadata[8..16].try_into().unwrap());
        (
            ((word >> 53) & 7) as u8,
            ((word >> 47) & 7) as u8,
            ((word >> 58) & 15) as u8,
        )
    }

    pub fn compute_binding_selectors(&self) -> Option<[u8; 4]> {
        (self.stage == "CS").then(|| self.shader_metadata[88..92].try_into().unwrap())
    }

    pub fn vertex_inputs(&self) -> Option<Sgd5VertexInputMetadata> {
        (self.stage == "VS")
            .then(|| Sgd5VertexInputMetadata::from_bytes(&self.shader_metadata[..0x48]))
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5VertexInputMetadata {
    /// One bit per input slot; slots 0..27 cover seven semantics with four indices each.
    pub attribute_mask: u64,
    /// Includes system inputs, format encoding, shader mode and root-argument fields.
    pub shader_flags: u64,
    pub attribute_formats: [u8; 52],
    pub reserved_44: u32,
}

impl Sgd5VertexInputMetadata {
    fn from_bytes(data: &[u8]) -> Self {
        Self {
            attribute_mask: u64::from_le_bytes(data[..8].try_into().unwrap()),
            shader_flags: u64::from_le_bytes(data[8..16].try_into().unwrap()),
            attribute_formats: data[16..68].try_into().unwrap(),
            reserved_44: u32::from_le_bytes(data[68..72].try_into().unwrap()),
        }
    }

    pub fn semantic(slot: usize) -> Option<(&'static str, u8)> {
        const SEMANTICS: [&str; 7] = [
            "POSITION",
            "NORMAL",
            "TANGENT",
            "BINORMAL",
            "BLENDWEIGHT",
            "BLENDINDICES",
            "COLOR",
        ];
        match slot {
            0..=27 => Some((SEMANTICS[slot / 4], (slot % 4) as u8)),
            28..=51 => Some(("TEXCOORD", (slot - 28) as u8)),
            _ => None,
        }
    }

    pub fn uses_vertex_id(&self) -> bool {
        self.shader_flags & 8 != 0
    }
    pub fn uses_instance_id(&self) -> bool {
        self.shader_flags & 2 != 0
    }
    pub fn compact_formats(&self) -> bool {
        self.shader_flags & (1 << 52) != 0
    }

    /// Normalizes the executable's compact format codes to DXGI_FORMAT values.
    /// The compact byte's high nibble is retained without assigning it a meaning.
    pub fn dxgi_format(&self, slot: usize) -> Option<u8> {
        let raw = *self.attribute_formats.get(slot)?;
        if !self.compact_formats() {
            return (raw != 0).then_some(raw);
        }
        const FORMATS: [u8; 13] = [41, 16, 6, 2, 30, 28, 24, 38, 34, 12, 10, 3, 17];
        usize::from(raw & 15)
            .checked_sub(1)
            .and_then(|index| FORMATS.get(index).copied())
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5EffectHeader {
    /// Aggregate VS masks/flags. Format interpretation follows the matching VS layout;
    /// its serialized merge rule and direct runtime consumer remain unconfirmed.
    pub vertex_inputs: Sgd5VertexInputMetadata,
    /// An additional mask in the same semantic-slot numbering; its selection policy is unconfirmed.
    pub attribute_mask_48: u64,
}

#[derive(Clone, Debug)]
pub struct Sgd5BindingEntry {
    pub name_hash: u32,
    pub binding_type: u16,
    pub binding_index: u16,
    pub name: Option<String>,
}

impl Sgd5BindingEntry {
    /// The runtime binding converter retains the low ten bits as descriptor index.
    pub fn descriptor_index(&self) -> usize {
        usize::from(self.binding_index & 0x03ff)
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5BindingTableHeader {
    pub entry_count: u16,
    pub unknown0: u8,
    pub srv_count: u8,
    pub unknown1: u8,
    pub uav_count: u8,
    pub sampler_count: u8,
    pub cbv_count: u8,
    pub table_hash: u32,
    pub unknown2: u32,
}

#[derive(Clone, Debug)]
pub struct Sgd5BindingTable {
    pub offset: usize,
    pub size: u32,
    pub hash: u64,
    pub header: Sgd5BindingTableHeader,
    pub entries: Vec<Sgd5BindingEntry>,
}

#[derive(Clone, Debug)]
pub struct Sgd5ResourceRecord {
    pub offset: usize,
    pub resource_type: u16,
    /// Shader register low byte in per-shader reflection; material SRV handle index in effect reflection.
    pub slot: u8,
    pub auxiliary_record_count: u8,
    pub member_count: u8,
    pub flags: u8,
    pub member_data_offset: u16,
    pub name_string_offset: u16,
    pub reserved_0a: u16,
    /// CBV data size; zero for records that do not carry a constant-buffer layout.
    pub cbuffer_size: u16,
    pub auxiliary_data_offset: u16,
    pub name_hash: u32,
    pub secondary_hash: u32,
    pub name: Option<String>,
}

impl Sgd5ResourceRecord {
    /// Only use for per-shader reflection; effect slots have different semantics.
    pub fn shader_register(&self) -> u32 {
        u32::from(self.slot)
            | (u32::from(self.resource_type & 0x0080) << 1)
            | (u32::from(self.resource_type & 0x2000) >> 4)
    }

    pub fn register_space(&self) -> u8 {
        self.flags >> 6
    }

    /// The effect runtime carries this pair as one 64-bit parameter identity.
    pub fn parameter_key(&self) -> u64 {
        u64::from(self.name_hash) | (u64::from(self.secondary_hash) << 32)
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5MemberRecord {
    pub offset: usize,
    pub value_type: u8,
    pub auxiliary_record_count: u8,
    pub array_size: u16,
    pub byte_offset: u16,
    pub reserved_06: u16,
    pub reserved_08: u16,
    pub default_value_offset: u16,
    pub auxiliary_data_offset: u16,
    pub reserved_0e: u16,
    pub name_hash: u32,
    pub secondary_hash: u32,
    pub name: Option<String>,
}

impl Sgd5MemberRecord {
    pub fn byte_size(&self) -> Option<usize> {
        let width = match self.value_type {
            0 | 1 | 5 | 9 => 4,
            2 | 6 | 10 => 8,
            3 | 7 | 11 => 12,
            4 | 8 | 12 => 16,
            13 => 48,
            14 => 64,
            _ => return None,
        };
        Some(width * usize::from(self.array_size.max(1)))
    }

    pub fn parameter_key(&self) -> u64 {
        u64::from(self.name_hash) | (u64::from(self.secondary_hash) << 32)
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5AuxiliaryRecord {
    pub offset: usize,
    pub hash: u32,
    pub data: u32,
}

impl Sgd5AuxiliaryRecord {
    pub fn value_kind(&self) -> u8 {
        (self.hash & 3) as u8
    }
    pub fn name_hash30(&self) -> u32 {
        self.hash >> 2
    }
}

#[derive(Clone, Debug, Default)]
pub struct Sgd5ReflectionTable {
    pub offset: usize,
    pub size: u32,
    pub resource_count: u16,
    pub member_count: u16,
    pub auxiliary_record_count: u16,
    pub resources: Vec<Sgd5ResourceRecord>,
    pub members: Vec<Sgd5MemberRecord>,
    pub auxiliary_records: Vec<Sgd5AuxiliaryRecord>,
    pub string_pool: Vec<u8>,
    pub strings: Vec<String>,
    pub default_values_offset: usize,
    pub default_value_data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum Sgd5BindingListEntry {
    TableReference { offset: usize, table_hash: u32 },
    Inline(Sgd5BindingTable),
}

#[derive(Clone, Debug)]
pub struct Sgd5BindingList {
    pub offset: usize,
    /// Number of u32 material texture handles, padded to a multiple of four.
    pub texture_handle_count: u8,
    pub entries: Vec<Sgd5BindingListEntry>,
}

#[derive(Clone, Debug)]
pub struct Sgd5RasterizerState {
    pub offset: usize,
    pub fill_mode: u8,
    pub cull_mode: u8,
    pub front_counter_clockwise: u8,
    pub depth_bias: i8,
    pub depth_bias_clamp: f32,
    pub slope_scaled_depth_bias: f32,
    pub depth_clip_enable: u8,
    pub unknown_0d: u8,
    pub multisample_enable: u8,
    pub flags: u8,
}

impl Sgd5RasterizerState {
    pub fn antialiased_line_enable(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn conservative_rasterization(&self) -> bool {
        self.flags & 2 != 0
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5StencilFace {
    pub fail_op: u8,
    pub depth_fail_op: u8,
    pub pass_op: u8,
    pub comparison_func: u8,
}

#[derive(Clone, Debug)]
pub struct Sgd5DepthStencilState {
    pub offset: usize,
    pub depth_enable: u8,
    pub depth_write_mask: u8,
    pub depth_comparison_func: u8,
    pub stencil_enable: u8,
    pub stencil_read_mask: u8,
    pub stencil_write_mask: u8,
    pub front_face: Sgd5StencilFace,
    pub back_face: Sgd5StencilFace,
    pub unknown_0e: u8,
    pub depth_bounds_test_enable: u8,
    pub unknown_tail: [u8; 8],
}

#[derive(Clone, Debug)]
pub struct Sgd5RenderTargetBlendState {
    pub blend_enable: u8,
    pub source_blend: u8,
    pub destination_blend: u8,
    pub blend_op: u8,
    pub source_blend_alpha: u8,
    pub destination_blend_alpha: u8,
    pub blend_op_alpha: u8,
    pub write_mask: u8,
}

#[derive(Clone, Debug)]
pub struct Sgd5BlendState {
    pub offset: usize,
    pub alpha_to_coverage_enable: u8,
    pub independent_blend_enable: u8,
    pub unknown_02: u16,
    pub render_targets: [Sgd5RenderTargetBlendState; 8],
}

#[derive(Clone, Debug)]
pub struct Sgd5SamplerState {
    pub offset: usize,
    /// Serialized sampler descriptor; filter bits remain available without guessing their meaning.
    pub data: [u8; 36],
}

#[derive(Clone, Debug)]
pub struct Sgd5SamplerDescriptor {
    pub preset_index: u8,
    pub address_modes: [u8; 3],
    pub mip_lod_bias: f32,
    pub comparison_bits: u8,
    pub max_anisotropy: u8,
    pub filter_bits: u16,
    pub border_color: [f32; 4],
    pub min_lod: f32,
    pub max_lod: f32,
}

impl Sgd5SamplerState {
    pub fn descriptor(&self) -> Sgd5SamplerDescriptor {
        let f = |at| f32::from_le_bytes(self.data[at..at + 4].try_into().unwrap());
        Sgd5SamplerDescriptor {
            preset_index: self.data[0],
            address_modes: self.data[1..4].try_into().unwrap(),
            mip_lod_bias: f(4),
            comparison_bits: self.data[8],
            max_anisotropy: self.data[9],
            filter_bits: u16::from_le_bytes(self.data[10..12].try_into().unwrap()),
            border_color: std::array::from_fn(|i| f(12 + i * 4)),
            min_lod: f(28),
            max_lod: f(32),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5Technique {
    pub offset: usize,
    pub name_string_offset: u16,
    pub first_pass: u16,
    pub pass_count: u8,
    pub name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Sgd5PassResourceUsage {
    pub offset: usize,
    pub srv_hashes: Vec<u32>,
    pub uav_hashes: Vec<u32>,
    pub sampler_hashes: Vec<u32>,
    pub cbv_hashes: Vec<u32>,
    /// Present when the effect has a texture-handle array. Low byte selects a
    /// flattened root-constant dword; 0xFF has no root-constant destination.
    pub texture_handle_binding: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Sgd5Pass {
    pub offset: usize,
    /// Zero means absent; other values are one-based slots in the effect's stage arrays.
    pub shader_slots: [u8; 6],
    pub rasterizer_state_index: u8,
    /// Nine-bit indices include the high bits carried by state_flags bits 6/7.
    pub depth_stencil_state_index: u16,
    pub blend_state_index: u16,
    pub stencil_reference: u8,
    pub state_flags: u8,
    pub unknown_0b: [u8; 5],
    /// Relative to the in-memory pass record; add four for the on-disk lookup-size prefix.
    pub usage_data_offset: u16,
    /// Binding-list selectors for register spaces 0..3; 0xFF means absent.
    pub binding_table_indices: [u8; 4],
    pub unknown_16: u16,
    pub resource_usage: Option<Sgd5PassResourceUsage>,
}

impl Sgd5Pass {
    pub fn selected_stencil_reference(&self) -> Option<u8> {
        (self.state_flags & 8 != 0).then_some(self.stencil_reference)
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5EffectMetadata {
    pub offset: usize,
    pub header: Sgd5EffectHeader,
    pub rasterizer_states: Vec<Sgd5RasterizerState>,
    pub depth_stencil_states: Vec<Sgd5DepthStencilState>,
    pub blend_states: Vec<Sgd5BlendState>,
    pub sampler_states: Vec<Sgd5SamplerState>,
    pub techniques: Vec<Sgd5Technique>,
    pub passes: Vec<Sgd5Pass>,
    /// Preserved because the per-pass lists do not yet explain every trailing word.
    pub pass_lookup_words: Vec<u32>,
}

#[derive(Clone, Debug)]
pub enum Sgd5ReflectionSource {
    Shared,
    Inline(Sgd5ReflectionTable),
    Absent,
}

#[derive(Clone, Debug)]
pub struct Sgd5Effect {
    pub name: String,
    pub data_buffer_size: u32,
    pub stage_indices: [Vec<u32>; 6],
    pub payload_offset: usize,
    pub payload_end: usize,
    pub metadata: Sgd5EffectMetadata,
    pub shared_reflection_source: Sgd5ReflectionSource,
    pub main_reflection: Sgd5ReflectionTable,
    pub property_reflection: Sgd5ReflectionTable,
    pub binding_list: Sgd5BindingList,
    /// Effect-owned pool addressed by technique records, independent of the binding list.
    pub string_pool: Vec<u8>,
    pub strings: Vec<String>,
}

impl Sgd5Effect {
    /// Bytes in the reader's copied/relocated data buffer. Interned render
    /// states and the fixed runtime effect object are allocated separately.
    pub fn runtime_data_size(&self) -> Option<u32> {
        let mut size = 0usize;
        for indices in &self.stage_indices {
            size = size.checked_add(indices.len().checked_add(1)?.checked_mul(8)?)?;
        }
        size = size.checked_add(self.metadata.techniques.len().checked_mul(8)?)?;
        size = size.checked_add(self.metadata.passes.len().checked_mul(32)?)?;
        size = size.checked_add(self.metadata.pass_lookup_words.len().checked_mul(4)?)?;
        if let Sgd5ReflectionSource::Inline(table) = &self.shared_reflection_source {
            size = size.checked_add(table.size as usize)?;
        }
        size = size.checked_add(self.main_reflection.size as usize)?;
        size = size.checked_add(self.property_reflection.size as usize)?;
        if !self.binding_list.entries.is_empty() {
            size = size.checked_add(7)? & !7;
            size = size.checked_add(self.binding_list.entries.len().checked_mul(8)?)?;
        }
        for entry in &self.binding_list.entries {
            if let Sgd5BindingListEntry::Inline(table) = entry {
                size = size.checked_add(table.size as usize)?;
            }
        }
        u32::try_from(size.checked_add(self.string_pool.len())?).ok()
    }
}

#[derive(Clone, Debug)]
pub struct Sgd5File {
    pub data: Vec<u8>,
    pub group_hash: u64,
    pub binding_tables: Vec<Sgd5BindingTable>,
    pub shared_reflection: Sgd5ReflectionTable,
    pub shaders: Vec<Sgd5Shader>,
    pub declared_effect_count: u32,
    pub effect_count: u32,
    pub trailer_flags: u32,
    pub trailer_reserved: [u8; 6],
    pub effects: Vec<Sgd5Effect>,
    pub hash_names: HashMap<u32, String>,
    pub effect_shader_map: HashMap<(u8, u32), Vec<usize>>,
}

impl Sgd5File {
    pub fn shader_position(&self, stage: &str, index: u32) -> Option<usize> {
        let stage = STAGES.iter().position(|s| s.0 == stage)?;
        self.shaders
            .binary_search_by_key(&(stage, index), |shader| {
                (
                    STAGES.iter().position(|s| s.0 == shader.stage).unwrap_or(6),
                    shader.index,
                )
            })
            .ok()
    }

    pub fn shader(&self, stage: &str, index: u32) -> Option<&Sgd5Shader> {
        self.shaders.get(self.shader_position(stage, index)?)
    }

    pub fn pass_shader(
        &self,
        effect: &Sgd5Effect,
        pass: &Sgd5Pass,
        stage: usize,
    ) -> Option<&Sgd5Shader> {
        let slot = pass.shader_slots.get(stage)?.checked_sub(1)?;
        let index = *effect.stage_indices.get(stage)?.get(usize::from(slot))?;
        self.shader(STAGES.get(stage)?.0, index)
    }

    pub fn binding_table<'a>(
        &'a self,
        effect: &'a Sgd5Effect,
        selector: u8,
    ) -> Option<&'a Sgd5BindingTable> {
        self.binding_table_at(effect, usize::from(selector))
    }

    pub fn binding_table_at<'a>(
        &'a self,
        effect: &'a Sgd5Effect,
        index: usize,
    ) -> Option<&'a Sgd5BindingTable> {
        match effect.binding_list.entries.get(index)? {
            Sgd5BindingListEntry::Inline(table) => Some(table),
            Sgd5BindingListEntry::TableReference { table_hash, .. } => self
                .binding_tables
                .iter()
                .find(|t| t.header.table_hash == *table_hash),
        }
    }

    pub fn resolve_name(&self, hash: u32) -> Option<&str> {
        self.hash_names.get(&hash).map(String::as_str)
    }

    pub fn effect_names_for_shader(&self, stage: &str, index: u32) -> Vec<&str> {
        let Some(stage_index) = STAGES.iter().position(|(name, _)| *name == stage) else {
            return Vec::new();
        };
        self.effect_shader_map
            .get(&(stage_index as u8, index))
            .map(|effect_indices| {
                effect_indices
                    .iter()
                    .filter_map(|effect_index| self.effects.get(*effect_index))
                    .map(|effect| effect.name.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug)]
pub struct Sgd5Extraction {
    pub files: Vec<Sgd5File>,
    pub extracted_count: usize,
    pub manifests: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Sgd5Error(String);

impl std::fmt::Display for Sgd5Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Sgd5Error {}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            end: data.len(),
        }
    }

    fn remaining(&self) -> usize {
        self.end - self.position
    }

    fn take(&mut self, size: usize) -> Result<&'a [u8], Sgd5Error> {
        let end = self
            .position
            .checked_add(size)
            .filter(|end| *end <= self.end)
            .ok_or_else(|| {
                Sgd5Error(format!(
                    "truncated data at 0x{:x}: need {size} bytes",
                    self.position
                ))
            })?;
        let bytes = &self.data[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Sgd5Error> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    fn u8(&mut self) -> Result<u8, Sgd5Error> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, Sgd5Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, Sgd5Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn peek_u32(&self) -> Result<u32, Sgd5Error> {
        let end = self
            .position
            .checked_add(4)
            .filter(|end| *end <= self.end)
            .ok_or_else(|| Sgd5Error(format!("truncated u32 at 0x{:x}", self.position)))?;
        Ok(u32::from_le_bytes(
            self.data[self.position..end].try_into().unwrap(),
        ))
    }

    fn u64(&mut self) -> Result<u64, Sgd5Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn section(&mut self, size: usize) -> Result<Self, Sgd5Error> {
        let position = self.position;
        self.take(size)?;
        Ok(Self {
            data: self.data,
            position,
            end: self.position,
        })
    }

    fn require_records(&self, count: usize, width: usize) -> Result<usize, Sgd5Error> {
        count
            .checked_mul(width)
            .filter(|size| *size <= self.remaining())
            .ok_or_else(|| {
                Sgd5Error(format!(
                    "truncated {count} records at 0x{:x}",
                    self.position
                ))
            })
    }

    fn name(&mut self, label: &str) -> Result<String, Sgd5Error> {
        let offset = self.position;
        let length = self.u8()? as usize;
        let bytes = self.take(length)?;
        if bytes.last() != Some(&0) || length < 2 || bytes[..length - 1].contains(&0) {
            return Err(Sgd5Error(format!("invalid {label} name at 0x{offset:x}")));
        }
        String::from_utf8(bytes[..length - 1].to_vec())
            .map_err(|_| Sgd5Error(format!("invalid UTF-8 {label} name at 0x{offset:x}")))
    }
}

pub fn joaat_lower(name: &str) -> u32 {
    let mut hash = 0u32;
    for byte in name.as_bytes().iter().map(|byte| byte.to_ascii_lowercase()) {
        hash = hash.wrapping_add(byte as u32);
        hash = hash.wrapping_add(hash << 10);
        hash ^= hash >> 6;
    }
    hash = hash.wrapping_add(hash << 3);
    hash ^= hash >> 11;
    hash = hash.wrapping_add(hash << 15);
    hash
}

fn parse_nul_pool(pool: &[u8]) -> Result<Vec<String>, Sgd5Error> {
    if pool.is_empty() {
        return Ok(Vec::new());
    }
    if pool.first() != Some(&0) || pool.last() != Some(&0) {
        return Err(Sgd5Error("string pool is not NUL-delimited".into()));
    }
    Ok(pool[1..]
        .split(|byte| *byte == 0)
        .filter(|token| !token.is_empty())
        .map(|token| String::from_utf8_lossy(token).into_owned())
        .collect())
}

fn parse_resource_record(reader: &mut Reader<'_>) -> Result<Sgd5ResourceRecord, Sgd5Error> {
    Ok(Sgd5ResourceRecord {
        offset: reader.position,
        resource_type: reader.u16()?,
        slot: reader.u8()?,
        auxiliary_record_count: reader.u8()?,
        member_count: reader.u8()?,
        flags: reader.u8()?,
        member_data_offset: reader.u16()?,
        name_string_offset: reader.u16()?,
        reserved_0a: reader.u16()?,
        cbuffer_size: reader.u16()?,
        auxiliary_data_offset: reader.u16()?,
        name_hash: reader.u32()?,
        secondary_hash: reader.u32()?,
        name: None,
    })
}

fn parse_member_record(reader: &mut Reader<'_>) -> Result<Sgd5MemberRecord, Sgd5Error> {
    Ok(Sgd5MemberRecord {
        offset: reader.position,
        value_type: reader.u8()?,
        auxiliary_record_count: reader.u8()?,
        array_size: reader.u16()?,
        byte_offset: reader.u16()?,
        reserved_06: reader.u16()?,
        reserved_08: reader.u16()?,
        default_value_offset: reader.u16()?,
        auxiliary_data_offset: reader.u16()?,
        reserved_0e: reader.u16()?,
        name_hash: reader.u32()?,
        secondary_hash: reader.u32()?,
        name: None,
    })
}

fn reflection_name(
    data: &[u8],
    offset: usize,
    relative_offset: u16,
    hash: u32,
    pool_start: usize,
    pool_end: usize,
) -> Option<String> {
    if relative_offset == 0 {
        return None;
    }
    let target = offset.checked_add(relative_offset as usize)?;
    if !(pool_start..pool_end).contains(&target) {
        return None;
    }
    let bytes = data.get(target..pool_end)?;
    let end = bytes.iter().position(|byte| *byte == 0)?;
    let name = std::str::from_utf8(&bytes[..end]).ok()?;
    (!name.is_empty() && joaat_lower(name) == hash).then(|| name.to_string())
}

fn resolve_reflection_names(table: &mut Sgd5ReflectionTable, names: &HashMap<u32, String>) {
    for record in &mut table.resources {
        if record.name.is_none() {
            record.name = names.get(&record.name_hash).cloned();
        }
    }
    for record in &mut table.members {
        if record.name.is_none() {
            record.name = names.get(&record.name_hash).cloned();
        }
    }
}

fn parse_reflection_table(reader: &mut Reader<'_>) -> Result<Sgd5ReflectionTable, Sgd5Error> {
    let offset = reader.position;
    let size = reader.u32()?;
    let mut section = reader.section(size as usize)?;
    let resource_count = section.u16()?;
    let member_count = section.u16()?;
    let auxiliary_record_count = section.u16()?;
    if section.u16()? as u32 != size {
        return Err(Sgd5Error(format!(
            "reflection table at 0x{offset:x}: size copy mismatch"
        )));
    }
    section.require_records(
        resource_count as usize + member_count as usize,
        REFLECTION_RECORD_SIZE,
    )?;
    let mut resources = Vec::with_capacity(resource_count as usize);
    for _ in 0..resource_count {
        resources.push(parse_resource_record(&mut section)?);
    }
    let mut members = Vec::with_capacity(member_count as usize);
    for _ in 0..member_count {
        members.push(parse_member_record(&mut section)?);
    }
    section.require_records(auxiliary_record_count as usize, 8)?;
    let mut auxiliary_records = Vec::with_capacity(auxiliary_record_count as usize);
    for _ in 0..auxiliary_record_count {
        auxiliary_records.push(Sgd5AuxiliaryRecord {
            offset: section.position,
            hash: section.u32()?,
            data: section.u32()?,
        });
    }

    let pool_start = section.position;
    let mut default_values_offset = section.end;
    let value_links = members
        .iter()
        .map(|record| (record.offset, record.default_value_offset));
    for (record_offset, relative_offset) in value_links {
        if relative_offset == 0 {
            continue;
        }
        let target = record_offset
            .checked_add(relative_offset as usize)
            .filter(|target| *target >= pool_start && *target <= section.end.saturating_sub(4))
            .ok_or_else(|| {
                Sgd5Error(format!(
                    "invalid default-value offset at 0x{record_offset:x}"
                ))
            })?;
        default_values_offset = default_values_offset.min(target);
    }
    // Default data is untyped; its boundary follows the actual value links, not the auxiliary count.
    let string_pool = section.take(default_values_offset - pool_start)?.to_vec();
    let strings = parse_nul_pool(&string_pool)?;
    let default_value_data = section.take(section.remaining())?.to_vec();
    for record in &mut resources {
        record.name = reflection_name(
            section.data,
            record.offset,
            record.name_string_offset,
            record.name_hash,
            pool_start,
            default_values_offset,
        );
    }
    let mut table = Sgd5ReflectionTable {
        offset,
        size,
        resource_count,
        member_count,
        auxiliary_record_count,
        resources,
        members,
        auxiliary_records,
        string_pool,
        strings,
        default_values_offset,
        default_value_data,
    };
    let names = table
        .strings
        .iter()
        .map(|name| (joaat_lower(name), name.clone()))
        .collect();
    resolve_reflection_names(&mut table, &names);
    validate_reflection_links(&table, section.data)?;
    Ok(table)
}

fn validate_reflection_links(table: &Sgd5ReflectionTable, data: &[u8]) -> Result<(), Sgd5Error> {
    let resource_start = table.offset + 12;
    let member_start = resource_start + table.resources.len() * 24;
    let auxiliary_start = member_start + table.members.len() * 24;
    let pool_start = auxiliary_start + table.auxiliary_records.len() * 8;
    let table_end = table.offset + 4 + table.size as usize;
    let link =
        |offset: usize, relative: u16, count: u8, start: usize, length: usize, width: usize| {
            if count == 0 {
                return Ok(());
            }
            let valid = relative != 0
                && offset
                    .checked_add(usize::from(relative))
                    .is_some_and(|target| {
                        target >= start
                            && (target - start) % width == 0
                            && target
                                .checked_add(usize::from(count) * width)
                                .is_some_and(|end| end <= start + length * width)
                    });
            if valid {
                Ok(())
            } else {
                Err(Sgd5Error(format!(
                    "invalid reflection record link at 0x{offset:x}"
                )))
            }
        };
    for record in &table.resources {
        link(
            record.offset,
            record.member_data_offset,
            record.member_count,
            member_start,
            table.members.len(),
            24,
        )?;
        link(
            record.offset,
            record.auxiliary_data_offset,
            record.auxiliary_record_count,
            auxiliary_start,
            table.auxiliary_records.len(),
            8,
        )?;
        if record.name_string_offset != 0 {
            let target = record.offset + usize::from(record.name_string_offset);
            if target < pool_start
                || target >= table.default_values_offset
                || !data[target..table.default_values_offset].contains(&0)
            {
                return Err(Sgd5Error(format!(
                    "invalid resource name link at 0x{:x}",
                    record.offset
                )));
            }
        }
    }
    for member in &table.members {
        link(
            member.offset,
            member.auxiliary_data_offset,
            member.auxiliary_record_count,
            auxiliary_start,
            table.auxiliary_records.len(),
            8,
        )?;
        if member.default_value_offset != 0 {
            let target = member.offset + usize::from(member.default_value_offset);
            if member
                .byte_size()
                .is_some_and(|size| target.checked_add(size).is_none_or(|end| end > table_end))
            {
                return Err(Sgd5Error(format!(
                    "truncated member default data at 0x{:x}",
                    member.offset
                )));
            }
        }
    }
    for record in &table.auxiliary_records {
        if record.value_kind() == 2 {
            let target = record.offset.checked_add(record.data as usize);
            if !target.is_some_and(|target| {
                target >= pool_start
                    && target < table.default_values_offset
                    && data[target..table.default_values_offset].contains(&0)
            }) {
                return Err(Sgd5Error(format!(
                    "invalid auxiliary string link at 0x{:x}",
                    record.offset
                )));
            }
        }
    }
    Ok(())
}

fn parse_binding_table(reader: &mut Reader<'_>) -> Result<Sgd5BindingTable, Sgd5Error> {
    let offset = reader.position;
    let size = reader.u32()?;
    let mut section = reader.section(size as usize)?;
    let hash = section.u64()?;
    let header = Sgd5BindingTableHeader {
        entry_count: section.u16()?,
        unknown0: section.u8()?,
        srv_count: section.u8()?,
        unknown1: section.u8()?,
        uav_count: section.u8()?,
        sampler_count: section.u8()?,
        cbv_count: section.u8()?,
        table_hash: section.u32()?,
        unknown2: section.u32()?,
    };
    let entries_size = section.require_records(header.entry_count as usize, 8)?;
    if section.remaining() != entries_size {
        return Err(Sgd5Error(format!(
            "binding table at 0x{offset:x}: size does not match entry count"
        )));
    }
    let mut entries = Vec::with_capacity(header.entry_count as usize);
    for _ in 0..header.entry_count {
        entries.push(Sgd5BindingEntry {
            name_hash: section.u32()?,
            binding_type: section.u16()?,
            binding_index: section.u16()?,
            name: None,
        });
    }
    Ok(Sgd5BindingTable {
        offset,
        size,
        hash,
        header,
        entries,
    })
}

fn parse_shader_archive(
    reader: &mut Reader<'_>,
    expected_size: u32,
) -> Result<Sgd5ShaderArchive, Sgd5Error> {
    let offset = reader.position;
    let mut section = reader.section(expected_size as usize)?;
    if section.u32()? != COMPRESSED_MAGIC {
        return Err(Sgd5Error(format!(
            "invalid compressed-container magic at 0x{offset:x}"
        )));
    }
    let archive_size = section.u32()?;
    let decompressed_size = section.u32()?;
    if decompressed_size > MAX_SHADER_BYTES {
        return Err(Sgd5Error(format!(
            "shader archive at 0x{offset:x}: decompressed size exceeds 64 MiB"
        )));
    }
    if archive_size != expected_size {
        return Err(Sgd5Error(format!(
            "shader archive at 0x{offset:x}: size copy mismatch"
        )));
    }
    Ok(Sgd5ShaderArchive {
        offset,
        archive_size,
        decompressed_size,
    })
}

fn parse_shader(
    reader: &mut Reader<'_>,
    stage: &'static str,
    index: u32,
) -> Result<Sgd5Shader, Sgd5Error> {
    let name = reader.name("shader")?;
    let shader_version = reader.u8()?;
    let archive_size = reader.u32()?;
    let archive = parse_shader_archive(reader, archive_size)?;
    let sga_hash = reader.u64()?;
    let shader_metadata = reader.array()?;
    let reflection = parse_reflection_table(reader)?;
    Ok(Sgd5Shader {
        stage,
        index,
        name,
        shader_version,
        archive,
        sga_hash,
        shader_metadata,
        reflection,
    })
}

fn read_counted_records<'a, T>(
    reader: &mut Reader<'a>,
    width: usize,
    mut parse: impl FnMut(&mut Reader<'a>) -> Result<T, Sgd5Error>,
) -> Result<Vec<T>, Sgd5Error> {
    let count = reader.u32()? as usize;
    reader.require_records(count, width)?;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        records.push(parse(reader)?);
    }
    Ok(records)
}

fn parse_rasterizer_state(reader: &mut Reader<'_>) -> Result<Sgd5RasterizerState, Sgd5Error> {
    let offset = reader.position;
    let data: [u8; 16] = reader.array()?;
    Ok(Sgd5RasterizerState {
        offset,
        fill_mode: data[0],
        cull_mode: data[1],
        front_counter_clockwise: data[2],
        depth_bias: data[3] as i8,
        depth_bias_clamp: f32::from_le_bytes(data[4..8].try_into().unwrap()),
        slope_scaled_depth_bias: f32::from_le_bytes(data[8..12].try_into().unwrap()),
        depth_clip_enable: data[12],
        unknown_0d: data[13],
        multisample_enable: data[14],
        flags: data[15],
    })
}

fn parse_depth_stencil_state(reader: &mut Reader<'_>) -> Result<Sgd5DepthStencilState, Sgd5Error> {
    let offset = reader.position;
    let data: [u8; 24] = reader.array()?;
    let face = |start| Sgd5StencilFace {
        fail_op: data[start],
        depth_fail_op: data[start + 1],
        pass_op: data[start + 2],
        comparison_func: data[start + 3],
    };
    Ok(Sgd5DepthStencilState {
        offset,
        depth_enable: data[0],
        depth_write_mask: data[1],
        depth_comparison_func: data[2],
        stencil_enable: data[3],
        stencil_read_mask: data[4],
        stencil_write_mask: data[5],
        front_face: face(6),
        back_face: face(10),
        unknown_0e: data[14],
        depth_bounds_test_enable: data[15],
        unknown_tail: data[16..].try_into().unwrap(),
    })
}

fn parse_blend_state(reader: &mut Reader<'_>) -> Result<Sgd5BlendState, Sgd5Error> {
    let offset = reader.position;
    let data: [u8; 68] = reader.array()?;
    Ok(Sgd5BlendState {
        offset,
        alpha_to_coverage_enable: data[0],
        independent_blend_enable: data[1],
        unknown_02: u16::from_le_bytes(data[2..4].try_into().unwrap()),
        render_targets: std::array::from_fn(|index| {
            let start = 4 + index * 8;
            Sgd5RenderTargetBlendState {
                blend_enable: data[start],
                source_blend: data[start + 1],
                destination_blend: data[start + 2],
                blend_op: data[start + 3],
                source_blend_alpha: data[start + 4],
                destination_blend_alpha: data[start + 5],
                blend_op_alpha: data[start + 6],
                write_mask: data[start + 7],
            }
        }),
    })
}

fn parse_pass(reader: &mut Reader<'_>) -> Result<Sgd5Pass, Sgd5Error> {
    let offset = reader.position;
    let data: [u8; 24] = reader.array()?;
    Ok(Sgd5Pass {
        offset,
        shader_slots: data[..6].try_into().unwrap(),
        rasterizer_state_index: data[6],
        depth_stencil_state_index: u16::from(data[7]) | (u16::from(data[10] & 0x40) << 2),
        blend_state_index: u16::from(data[8]) | (u16::from(data[10] & 0x80) << 1),
        stencil_reference: data[9],
        state_flags: data[10],
        unknown_0b: data[11..16].try_into().unwrap(),
        usage_data_offset: u16::from_le_bytes(data[16..18].try_into().unwrap()),
        binding_table_indices: data[18..22].try_into().unwrap(),
        unknown_16: u16::from_le_bytes(data[22..24].try_into().unwrap()),
        resource_usage: None,
    })
}

fn parse_effect_metadata(reader: &mut Reader<'_>) -> Result<Sgd5EffectMetadata, Sgd5Error> {
    let offset = reader.position;
    let header = Sgd5EffectHeader {
        vertex_inputs: Sgd5VertexInputMetadata::from_bytes(reader.take(0x48)?),
        attribute_mask_48: reader.u64()?,
    };
    let rasterizer_states = read_counted_records(reader, 16, parse_rasterizer_state)?;
    let depth_stencil_states = read_counted_records(reader, 24, parse_depth_stencil_state)?;
    let blend_states = read_counted_records(reader, 68, parse_blend_state)?;
    let sampler_states = read_counted_records(reader, 36, |reader| {
        Ok(Sgd5SamplerState {
            offset: reader.position,
            data: reader.array()?,
        })
    })?;
    let techniques = read_counted_records(reader, 4, |reader| {
        let offset = reader.position;
        let packed = reader.u32()?;
        Ok(Sgd5Technique {
            offset,
            name_string_offset: (packed & 0x7fff) as u16,
            first_pass: ((packed >> 15) & 0x1ff) as u16,
            pass_count: (packed >> 24) as u8,
            name: None,
        })
    })?;
    let mut passes = read_counted_records(reader, 24, parse_pass)?;
    let count = reader.u32()? as usize;
    reader.require_records(count, 4)?;
    let usage_start = reader.position;
    let mut pass_lookup_words = Vec::with_capacity(count);
    for _ in 0..count {
        pass_lookup_words.push(reader.u32()?);
    }
    let usage_end = reader.position;
    for pass in &mut passes {
        if pass.usage_data_offset == 0 {
            continue;
        }
        // The loader omits the lookup table's u32 count from its in-memory buffer.
        let target = pass
            .offset
            .checked_add(pass.usage_data_offset as usize)
            .and_then(|offset| offset.checked_add(4))
            .filter(|offset| {
                *offset >= usage_start && *offset < usage_end && (*offset - usage_start) % 4 == 0
            })
            .ok_or_else(|| {
                Sgd5Error(format!("invalid pass usage offset at 0x{:x}", pass.offset))
            })?;
        let mut usage = Reader {
            data: reader.data,
            position: target,
            end: usage_end,
        };
        pass.resource_usage = Some(Sgd5PassResourceUsage {
            offset: target,
            srv_hashes: read_counted_records(&mut usage, 4, |reader| reader.u32())?,
            uav_hashes: read_counted_records(&mut usage, 4, |reader| reader.u32())?,
            sampler_hashes: read_counted_records(&mut usage, 4, |reader| reader.u32())?,
            cbv_hashes: read_counted_records(&mut usage, 4, |reader| reader.u32())?,
            texture_handle_binding: None,
        });
    }
    Ok(Sgd5EffectMetadata {
        offset,
        header,
        rasterizer_states,
        depth_stencil_states,
        blend_states,
        sampler_states,
        techniques,
        passes,
        pass_lookup_words,
    })
}

fn parse_reflection_source(reader: &mut Reader<'_>) -> Result<Sgd5ReflectionSource, Sgd5Error> {
    match reader.peek_u32()? {
        TABLE_REFERENCE => {
            reader.u32()?;
            Ok(Sgd5ReflectionSource::Shared)
        }
        0 => {
            reader.u32()?;
            Ok(Sgd5ReflectionSource::Absent)
        }
        _ => Ok(Sgd5ReflectionSource::Inline(parse_reflection_table(
            reader,
        )?)),
    }
}

fn parse_binding_list(reader: &mut Reader<'_>) -> Result<Sgd5BindingList, Sgd5Error> {
    let offset = reader.position;
    let packed_count = reader.u32()?;
    let count = (packed_count & 0x00ff_ffff) as usize;
    let texture_handle_count = (packed_count >> 24) as u8;
    reader.require_records(count, 8)?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let entry_offset = reader.position;
        let marker = reader.peek_u32()?;
        if marker == TABLE_REFERENCE {
            reader.u32()?;
            entries.push(Sgd5BindingListEntry::TableReference {
                offset: entry_offset,
                table_hash: reader.u32()?,
            });
        } else {
            entries.push(Sgd5BindingListEntry::Inline(parse_binding_table(reader)?));
        }
    }
    Ok(Sgd5BindingList {
        offset,
        texture_handle_count,
        entries,
    })
}

fn parse_effect(reader: &mut Reader<'_>, stage_counts: &[u32; 6]) -> Result<Sgd5Effect, Sgd5Error> {
    let name = reader.name("effect")?;
    let data_buffer_size = reader.u32()?;
    let mut stage_indices: [Vec<u32>; 6] = std::array::from_fn(|_| Vec::new());
    for (stage_index, indices) in stage_indices.iter_mut().enumerate() {
        let count = reader.u32()? as usize;
        reader.require_records(count, 4)?;
        indices.reserve(count);
        for _ in 0..count {
            let index = reader.u32()?;
            if index >= stage_counts[stage_index] {
                return Err(Sgd5Error(format!(
                    "effect {name}: invalid {} shader index {index}",
                    STAGES[stage_index].0
                )));
            }
            indices.push(index);
        }
    }
    let payload_offset = reader.position;
    let mut metadata = parse_effect_metadata(reader)?;
    let lookup_end = reader.position;
    let shared_reflection_source = parse_reflection_source(reader)?;
    let main_reflection = parse_reflection_table(reader)?;
    let property_reflection = parse_reflection_table(reader)?;
    let binding_list = parse_binding_list(reader)?;
    let strings_size = reader.u32()? as usize;
    let string_pool = reader.take(strings_size)?.to_vec();
    let strings = parse_nul_pool(&string_pool)?;
    if binding_list.texture_handle_count != 0 {
        for pass in &mut metadata.passes {
            if let Some(usage) = &mut pass.resource_usage {
                let words = usage.srv_hashes.len()
                    + usage.uav_hashes.len()
                    + usage.sampler_hashes.len()
                    + usage.cbv_hashes.len();
                let position = usage.offset + 16 + 4 * words;
                let mut tail = Reader {
                    data: reader.data,
                    position,
                    end: lookup_end,
                };
                usage.texture_handle_binding = Some(tail.u32()?);
            }
        }
    }
    for technique in &mut metadata.techniques {
        let pool = string_pool
            .get(technique.name_string_offset as usize..)
            .ok_or_else(|| Sgd5Error(format!("effect {name}: invalid technique name offset")))?;
        let end = pool
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| Sgd5Error(format!("effect {name}: unterminated technique name")))?;
        technique.name = Some(
            std::str::from_utf8(&pool[..end])
                .map_err(|_| Sgd5Error(format!("effect {name}: invalid technique name")))?
                .to_string(),
        );
        if technique.first_pass as usize + technique.pass_count as usize > metadata.passes.len() {
            return Err(Sgd5Error(format!(
                "effect {name}: technique pass range exceeds pass table"
            )));
        }
    }
    for pass in &metadata.passes {
        for (stage, slot) in pass.shader_slots.iter().enumerate() {
            if *slot as usize > stage_indices[stage].len() {
                return Err(Sgd5Error(format!(
                    "effect {name}: pass references an invalid shader slot"
                )));
            }
        }
        for slot in pass.binding_table_indices {
            if slot != 0xff && slot as usize >= binding_list.entries.len() {
                return Err(Sgd5Error(format!(
                    "effect {name}: pass references an invalid binding table"
                )));
            }
        }
        for (flag, index, count) in [
            (
                1,
                u16::from(pass.rasterizer_state_index),
                metadata.rasterizer_states.len(),
            ),
            (
                2,
                pass.depth_stencil_state_index,
                metadata.depth_stencil_states.len(),
            ),
            (4, pass.blend_state_index, metadata.blend_states.len()),
        ] {
            if pass.state_flags & flag != 0 && index as usize >= count {
                return Err(Sgd5Error(format!(
                    "effect {name}: pass references an invalid render state"
                )));
            }
        }
    }
    let effect = Sgd5Effect {
        name,
        data_buffer_size,
        stage_indices,
        payload_offset,
        payload_end: reader.position,
        metadata,
        shared_reflection_source,
        main_reflection,
        property_reflection,
        binding_list,
        string_pool,
        strings,
    };
    if effect.runtime_data_size() != Some(data_buffer_size) {
        return Err(Sgd5Error(format!(
            "effect {}: runtime allocation size {data_buffer_size} does not match parsed layout",
            effect.name
        )));
    }
    Ok(effect)
}

fn build_hash_names(
    shared_reflection: &Sgd5ReflectionTable,
    shaders: &[Sgd5Shader],
    effects: &[Sgd5Effect],
) -> HashMap<u32, String> {
    let tables = std::iter::once(shared_reflection)
        .chain(shaders.iter().map(|shader| &shader.reflection))
        .chain(
            effects
                .iter()
                .flat_map(|effect| [&effect.main_reflection, &effect.property_reflection]),
        )
        .chain(
            effects
                .iter()
                .filter_map(|effect| match &effect.shared_reflection_source {
                    Sgd5ReflectionSource::Inline(table) => Some(table),
                    _ => None,
                }),
        );
    let strings = tables
        .flat_map(|table| table.strings.iter())
        .chain(effects.iter().flat_map(|effect| effect.strings.iter()));
    strings
        .map(|name| (joaat_lower(name), name.clone()))
        .collect()
}

fn resolve_binding_names(table: &mut Sgd5BindingTable, names: &HashMap<u32, String>) {
    for binding in &mut table.entries {
        binding.name = names.get(&binding.name_hash).cloned();
    }
}

fn build_effect_shader_map(effects: &[Sgd5Effect]) -> HashMap<(u8, u32), Vec<usize>> {
    let mut map = HashMap::new();
    for (effect_index, effect) in effects.iter().enumerate() {
        for (stage_index, indices) in effect.stage_indices.iter().enumerate() {
            for shader_index in indices {
                let effect_indices = map
                    .entry((stage_index as u8, *shader_index))
                    .or_insert_with(Vec::new);
                if effect_indices.last() != Some(&effect_index) {
                    effect_indices.push(effect_index);
                }
            }
        }
    }
    map
}

pub fn load_sgd5(path: &Path) -> Result<Vec<Sgd5File>, Sgd5Error> {
    let data =
        fs::read(path).map_err(|error| Sgd5Error(format!("read {}: {error}", path.display())))?;
    parse_sgd5_streams(path, &data)
}

fn parse_sgd5_data(path: &Path, data: &[u8]) -> Result<Sgd5File, Sgd5Error> {
    let mut reader = Reader::new(data);
    if reader.take(4)? != MAGIC {
        return Err(Sgd5Error(format!(
            "{} is not an SGD5 shader-group file",
            path.display()
        )));
    }
    let group_hash = reader.u64()?;
    let binding_table_count = reader.u32()? as usize;
    reader.require_records(binding_table_count, 28)?;
    let mut binding_tables = Vec::with_capacity(binding_table_count.min(256));
    for _ in 0..binding_table_count {
        binding_tables.push(parse_binding_table(&mut reader)?);
    }
    let mut shared_reflection = parse_reflection_table(&mut reader)?;
    let mut shaders = Vec::new();
    let mut stage_counts = [0u32; 6];
    for (stage_index, (stage, _)) in STAGES.iter().enumerate() {
        let count = reader.u32()?;
        reader.require_records(count as usize, 1)?;
        stage_counts[stage_index] = count;
        for index in 0..count {
            let offset = reader.position;
            shaders.push(parse_shader(&mut reader, stage, index).map_err(|error| {
                Sgd5Error(format!("{stage} shader {index} at 0x{offset:x}: {error}"))
            })?);
        }
    }
    let declared_effect_count = reader.u32()?;
    reader.require_records(declared_effect_count as usize, 1)?;
    let mut effects = Vec::with_capacity(declared_effect_count.min(1_024) as usize);
    for index in 0..declared_effect_count {
        let offset = reader.position;
        effects.push(
            parse_effect(&mut reader, &stage_counts)
                .map_err(|error| Sgd5Error(format!("effect {index} at 0x{offset:x}: {error}")))?,
        );
    }
    let trailer_flags = reader.u32()?;
    let trailer_reserved = reader.array()?;
    if !matches!(trailer_flags, 0 | 1) || trailer_reserved != [0; 6] {
        return Err(Sgd5Error(format!(
            "invalid SGD5 stream trailer at 0x{:x}",
            reader.position - 10
        )));
    }

    let hash_names = build_hash_names(&shared_reflection, &shaders, &effects);
    let compute_selectors: HashMap<_, _> = shaders
        .iter()
        .filter_map(|shader| {
            shader
                .compute_binding_selectors()
                .map(|selectors| (shader.index, selectors))
        })
        .collect();
    for effect in &effects {
        for pass in &effect.metadata.passes {
            if let Some(slot) = pass.shader_slots[5].checked_sub(1) {
                let index = effect.stage_indices[5][usize::from(slot)];
                for selector in compute_selectors[&index] {
                    if selector != 255 && usize::from(selector) >= effect.binding_list.entries.len()
                    {
                        return Err(Sgd5Error(format!(
                            "effect {}: compute shader {index} references an invalid binding table",
                            effect.name
                        )));
                    }
                }
            }
        }
    }
    resolve_reflection_names(&mut shared_reflection, &hash_names);
    let mut table_hashes = HashMap::new();
    for (index, table) in binding_tables.iter_mut().enumerate() {
        if table_hashes
            .insert(table.header.table_hash, index)
            .is_some()
        {
            return Err(Sgd5Error(format!(
                "duplicate binding table hash {:08x}",
                table.header.table_hash
            )));
        }
        resolve_binding_names(table, &hash_names);
    }
    for effect in &mut effects {
        if let Sgd5ReflectionSource::Inline(table) = &mut effect.shared_reflection_source {
            resolve_reflection_names(table, &hash_names);
        }
        resolve_reflection_names(&mut effect.main_reflection, &hash_names);
        resolve_reflection_names(&mut effect.property_reflection, &hash_names);
        for property in &mut effect.binding_list.entries {
            match property {
                Sgd5BindingListEntry::Inline(table) => resolve_binding_names(table, &hash_names),
                Sgd5BindingListEntry::TableReference { table_hash, .. } => {
                    if !table_hashes.contains_key(table_hash) {
                        return Err(Sgd5Error(format!(
                            "effect {}: unknown binding table hash {table_hash:08x}",
                            effect.name
                        )));
                    }
                }
            }
        }
    }
    for shader in &mut shaders {
        resolve_reflection_names(&mut shader.reflection, &hash_names);
    }
    let effect_shader_map = build_effect_shader_map(&effects);
    Ok(Sgd5File {
        data: data[..reader.position].to_vec(),
        group_hash,
        binding_tables,
        shared_reflection,
        shaders,
        declared_effect_count,
        effect_count: effects.len() as u32,
        trailer_flags,
        trailer_reserved,
        effects,
        hash_names,
        effect_shader_map,
    })
}

fn parse_sgd5_streams(path: &Path, data: &[u8]) -> Result<Vec<Sgd5File>, Sgd5Error> {
    let mut files = Vec::new();
    let mut offset = 0;
    while let Some(found) = data[offset..]
        .windows(MAGIC.len())
        .position(|window| window == MAGIC)
    {
        let start = offset + found;
        let file = parse_sgd5_data(path, &data[start..]).map_err(|error| {
            Sgd5Error(format!(
                "SGD5 stream {} at 0x{start:x}: {error}",
                files.len()
            ))
        })?;
        offset = start + file.data.len();
        files.push(file);
    }
    if files.is_empty() {
        return Err(Sgd5Error("no valid SGD5 data".into()));
    }
    Ok(files)
}

pub fn decompress_shader(file: &Sgd5File, shader: &Sgd5Shader) -> Result<Vec<u8>, Sgd5Error> {
    let archive = &shader.archive;
    if archive.decompressed_size > MAX_SHADER_BYTES {
        return Err(Sgd5Error(format!(
            "{}: decompressed size exceeds 64 MiB",
            shader.name
        )));
    }
    let mut reader = Reader::new(&file.data);
    reader.take(archive.offset)?;
    let mut section = reader.section(archive.archive_size as usize)?;
    section.take(12)?;
    let compressed = section.take(section.remaining())?;
    let mut decoder = DeflateDecoder::new(compressed);
    let mut output = Vec::new();
    decoder
        .by_ref()
        .take(archive.decompressed_size as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|error| Sgd5Error(format!("{}: DEFLATE error: {error}", shader.name)))?;
    if output.len() != archive.decompressed_size as usize {
        return Err(Sgd5Error(format!(
            "{}: decompressed {} bytes, expected {}",
            shader.name,
            output.len(),
            archive.decompressed_size
        )));
    }
    if output.len() < 32 || &output[..4] != b"DXBC" {
        return Err(Sgd5Error(format!(
            "{}: decompressed payload is not DXBC",
            shader.name
        )));
    }
    Ok(output)
}

fn safe_name(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
            output.push(character);
        } else {
            output.push('_');
        }
    }
    let trimmed = output.trim_matches('_');
    if trimmed.is_empty() {
        "shader".to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_sgd5_file(
    file: &Sgd5File,
    output_dir: &Path,
    limit: Option<usize>,
    manifest_name: &str,
) -> Result<usize, Sgd5Error> {
    fs::create_dir_all(output_dir)
        .map_err(|error| Sgd5Error(format!("create {}: {error}", output_dir.display())))?;

    let selected = limit.map_or(&file.shaders[..], |limit| {
        &file.shaders[..limit.min(file.shaders.len())]
    });

    let mut manifest = String::from(
        "index,stage,name,shader_version,archive_size,decompressed_size,sga_hash,resources,members,auxiliary_records,filename\n",
    );
    for shader in selected {
        let blob = decompress_shader(file, shader)?;
        let filename = format!(
            "{:05}_{}_{}.dxbc",
            shader.index,
            shader.stage.to_lowercase(),
            safe_name(&shader.name)
        );
        fs::write(output_dir.join(&filename), &blob)
            .map_err(|error| Sgd5Error(format!("write {filename}: {error}")))?;
        let _ = writeln!(
            manifest,
            "{},{},{},{},{},{},0x{:016x},{},{},{},{}",
            shader.index,
            shader.stage,
            shader.name,
            shader.shader_version,
            shader.archive.archive_size,
            shader.archive.decompressed_size,
            shader.sga_hash,
            shader.reflection.resource_count,
            shader.reflection.member_count,
            shader.reflection.auxiliary_record_count,
            filename
        );
    }
    fs::write(output_dir.join(manifest_name), manifest)
        .map_err(|error| Sgd5Error(format!("write {manifest_name}: {error}")))?;

    Ok(selected.len())
}

pub fn extract_sgd5(
    input: &Path,
    output_dir: &Path,
    limit: usize,
) -> Result<Sgd5Extraction, Sgd5Error> {
    let data =
        fs::read(input).map_err(|error| Sgd5Error(format!("read {}: {error}", input.display())))?;
    let files = parse_sgd5_streams(input, &data)?;
    fs::create_dir_all(output_dir)
        .map_err(|error| Sgd5Error(format!("create {}: {error}", output_dir.display())))?;

    let mut remaining = if limit == 0 { None } else { Some(limit) };
    let mut extracted_count = 0usize;
    let mut manifests = Vec::new();
    for (stream_index, file) in files.iter().enumerate() {
        let single_output = if files.len() == 1 {
            output_dir.to_path_buf()
        } else {
            output_dir.join(format!("sgd5_{stream_index:03}"))
        };
        let manifest_name = "sgd5_index.csv";
        let extracted = extract_sgd5_file(file, &single_output, remaining, manifest_name)?;
        if let Some(remaining) = remaining.as_mut() {
            *remaining = remaining.saturating_sub(extracted);
        }
        extracted_count += extracted;
        manifests.push(single_output.join(manifest_name));
    }

    Ok(Sgd5Extraction {
        files,
        extracted_count,
        manifests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_state_indices_include_high_bits_and_stencil_reference_is_conditional() {
        let mut data = [0u8; 24];
        data[7] = 3;
        data[8] = 9;
        data[9] = 131;
        data[10] = 0xce;
        let pass = parse_pass(&mut Reader::new(&data)).unwrap();
        assert_eq!(pass.depth_stencil_state_index, 259);
        assert_eq!(pass.blend_state_index, 265);
        assert_eq!(pass.selected_stencil_reference(), Some(131));
        data[10] &= !8;
        assert_eq!(
            parse_pass(&mut Reader::new(&data))
                .unwrap()
                .selected_stencil_reference(),
            None
        );
    }

    #[test]
    fn state_fields_follow_executable_expansion_offsets() {
        let mut rasterizer = [0u8; 16];
        rasterizer[14] = 1;
        rasterizer[15] = 3;
        let state = parse_rasterizer_state(&mut Reader::new(&rasterizer)).unwrap();
        assert_eq!(state.multisample_enable, 1);
        assert!(state.antialiased_line_enable() && state.conservative_rasterization());
        let mut depth = [0u8; 24];
        depth[15] = 1;
        assert_eq!(
            parse_depth_stencil_state(&mut Reader::new(&depth))
                .unwrap()
                .depth_bounds_test_enable,
            1
        );
        let mut sampler = Sgd5SamplerState {
            offset: 0,
            data: [0; 36],
        };
        sampler.data[1..4].copy_from_slice(&[1, 2, 3]);
        sampler.data[4..8].copy_from_slice(&(-0.5f32).to_le_bytes());
        sampler.data[10..12].copy_from_slice(&0x55u16.to_le_bytes());
        sampler.data[28..32].copy_from_slice(&2f32.to_le_bytes());
        sampler.data[32..36].copy_from_slice(&8f32.to_le_bytes());
        let desc = sampler.descriptor();
        assert_eq!(desc.address_modes, [1, 2, 3]);
        assert_eq!(
            (
                desc.mip_lod_bias,
                desc.filter_bits,
                desc.min_lod,
                desc.max_lod
            ),
            (-0.5, 0x55, 2.0, 8.0)
        );
    }

    fn empty_reflection() -> Vec<u8> {
        let mut data = 9u32.to_le_bytes().to_vec();
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 9, 0, 0]);
        data
    }

    fn empty_stream() -> Vec<u8> {
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend(empty_reflection());
        data.extend_from_slice(&[0; 28]);
        data.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        data
    }

    fn binding_table(count: u16) -> Vec<u8> {
        let size = 24 + count as u32 * 8;
        let mut data = size.to_le_bytes().to_vec();
        data.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        data.extend_from_slice(&count.to_le_bytes());
        data.extend_from_slice(&[0; 6]);
        data.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for index in 0..count {
            data.extend_from_slice(&(index as u32).to_le_bytes());
            data.extend_from_slice(&0x0102u16.to_le_bytes());
            data.extend_from_slice(&index.to_le_bytes());
        }
        data
    }

    fn resource(words: [u16; 8], hash: u32, secondary_hash: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for word in words {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data.extend_from_slice(&hash.to_le_bytes());
        data.extend_from_slice(&secondary_hash.to_le_bytes());
        data
    }

    #[test]
    fn reflection_preserves_sections_offsets_and_untyped_defaults() {
        let pool = b"\0Buffer\0Member\0";
        let size = 8 + 48 + 16 + pool.len() + 8;
        let mut data = vec![0; 7];
        data.extend_from_slice(&(size as u32).to_le_bytes());
        data.extend_from_slice(&[1, 0, 1, 0, 2, 0]);
        data.extend_from_slice(&(size as u16).to_le_bytes());
        data.extend(resource(
            [0x0430, 0x0105, 0x4001, 24, 65, 0xffff, 16, 48],
            joaat_lower("Buffer"),
            0x8765_4321,
        ));
        data.extend(resource(
            [0x0109, 0, 12, 0x55aa, 0xffff, 55, 32, 0xbeef],
            joaat_lower("Member"),
            0x1234_5678,
        ));
        data.extend_from_slice(&0x1c57_54ecu32.to_le_bytes());
        data.extend_from_slice(&0x050fu32.to_le_bytes());
        data.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(pool);
        data.extend_from_slice(&1.25f32.to_le_bytes());
        data.extend_from_slice(&0xdead_beefu32.to_le_bytes());
        data.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        let mut reader = Reader::new(&data);
        reader.take(7).unwrap();
        let table = parse_reflection_table(&mut reader).unwrap();
        assert_eq!(table.offset, 7);
        assert_eq!(table.resources[0].name.as_deref(), Some("Buffer"));
        assert_eq!(table.resources[0].slot, 5);
        assert_eq!(table.resources[0].auxiliary_record_count, 1);
        assert_eq!(table.resources[0].member_count, 1);
        assert_eq!(table.resources[0].flags, 0x40);
        assert_eq!(table.resources[0].reserved_0a, 0xffff);
        assert_eq!(table.resources[0].cbuffer_size, 16);
        assert_eq!(table.resources[0].auxiliary_data_offset, 48);
        assert_eq!(table.resources[0].secondary_hash, 0x8765_4321);
        assert_eq!(table.members[0].name.as_deref(), Some("Member"));
        assert_eq!(table.members[0].byte_offset, 12);
        assert_eq!(table.members[0].value_type, 9);
        assert_eq!(table.members[0].auxiliary_record_count, 1);
        assert_eq!(table.members[0].reserved_06, 0x55aa);
        assert_eq!(table.members[0].reserved_08, 0xffff);
        assert_eq!(table.members[0].reserved_0e, 0xbeef);
        assert_eq!(table.members[0].auxiliary_data_offset, 32);
        assert_eq!(table.auxiliary_records[0].data, 0x050f);
        assert_eq!(table.default_value_data.len(), 8);
        assert_eq!(table.default_value_data[4..], 0xdead_beefu32.to_le_bytes());
        assert_eq!(table.string_pool, pool);
        assert_eq!(reader.u32().unwrap(), 0x1122_3344);
        // A record link into a neighbouring section must be rejected at parse time.
        for (field, value) in [(7 + 4 + 8 + 6, 25u16), (7 + 4 + 8 + 14, 49)] {
            let mut invalid = data.clone();
            invalid[field..field + 2].copy_from_slice(&value.to_le_bytes());
            let mut reader = Reader::new(&invalid);
            reader.take(7).unwrap();
            assert!(parse_reflection_table(&mut reader).is_err());
        }
        let mut invalid = data.clone();
        // A float4 default cannot fit in the eight-byte tail.
        invalid[7 + 4 + 8 + 24] = 12;
        let mut reader = Reader::new(&invalid);
        reader.take(7).unwrap();
        assert!(parse_reflection_table(&mut reader).is_err());
    }

    #[test]
    fn binding_tables_keep_u16_counts_and_binding_lists_use_u24_counts() {
        let inline = binding_table(257);
        let table = parse_binding_table(&mut Reader::new(&inline)).unwrap();
        assert_eq!(table.hash, 0x1122_3344_5566_7788);
        assert_eq!(table.header.table_hash, 0xaabb_ccdd);
        assert_eq!(table.entries.len(), 257);
        assert_eq!(table.entries[256].binding_index, 256);
        assert_eq!(table.entries[0].binding_type, 0x0102);
        let mut data = vec![2, 0, 0, 0x34];
        data.extend_from_slice(&TABLE_REFERENCE.to_le_bytes());
        data.extend_from_slice(&table.header.table_hash.to_le_bytes());
        data.extend(inline);
        data.extend_from_slice(&0u32.to_le_bytes());
        let list = parse_binding_list(&mut Reader::new(&data)).unwrap();
        assert_eq!(list.texture_handle_count, 0x34);
        assert!(matches!(
            list.entries[0],
            Sgd5BindingListEntry::TableReference {
                table_hash: 0xaabb_ccdd,
                ..
            }
        ));
        let Sgd5BindingListEntry::Inline(table) = &list.entries[1] else {
            panic!("missing inline table")
        };
        assert_eq!(table.entries.len(), 257);
    }

    #[test]
    fn binding_list_count_does_not_drop_bit_16() {
        let count = 0x10000u32;
        let mut data = (count | 0xa500_0000).to_le_bytes().to_vec();
        for _ in 0..count {
            data.extend_from_slice(&TABLE_REFERENCE.to_le_bytes());
            data.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        }
        data.extend_from_slice(&0u32.to_le_bytes());
        let list = parse_binding_list(&mut Reader::new(&data)).unwrap();
        assert_eq!(list.texture_handle_count, 0xa5);
        assert_eq!(list.entries.len(), count as usize);
    }

    #[test]
    fn table_reads_cannot_cross_their_declared_extent() {
        let mut data = empty_reflection();
        data[4..6].copy_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&[0; 64]);
        assert!(parse_reflection_table(&mut Reader::new(&data)).is_err());
        let mut table = binding_table(1);
        table[..4].copy_from_slice(&24u32.to_le_bytes());
        assert!(parse_binding_table(&mut Reader::new(&table)).is_err());
        let mut reader = Reader::new(&data);
        assert!(reader.take(usize::MAX).is_err());
        let mut short = empty_reflection();
        short[10..12].copy_from_slice(&8u16.to_le_bytes());
        assert!(parse_reflection_table(&mut Reader::new(&short)).is_err());
    }

    #[test]
    fn shader_archive_checks_both_sizes_and_header_extent() {
        let mut data = COMPRESSED_MAGIC.to_le_bytes().to_vec();
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&32u32.to_le_bytes());
        data.extend_from_slice(b"SGD5");
        let archive = parse_shader_archive(&mut Reader::new(&data), 16).unwrap();
        assert_eq!(archive.decompressed_size, 32);
        assert!(parse_shader_archive(&mut Reader::new(&data), 8).is_err());
        data[4..8].copy_from_slice(&15u32.to_le_bytes());
        assert!(parse_shader_archive(&mut Reader::new(&data), 16).is_err());
        data[4..8].copy_from_slice(&16u32.to_le_bytes());
        data[8..12].copy_from_slice(&(MAX_SHADER_BYTES + 1).to_le_bytes());
        assert!(parse_shader_archive(&mut Reader::new(&data), 16).is_err());
    }

    #[test]
    fn binding_list_truncation_never_reads_past_the_input() {
        let mut data = vec![2, 0, 0, 0];
        data.extend(binding_table(1));
        data.extend_from_slice(&TABLE_REFERENCE.to_le_bytes());
        data.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
        for length in 0..data.len() {
            assert!(parse_binding_list(&mut Reader::new(&data[..length])).is_err());
        }
        assert!(parse_binding_list(&mut Reader::new(&data)).is_ok());
        // The effect's pool-size word belongs to the enclosing effect, not this list.
        let list_end = data.len();
        data.extend_from_slice(&7u32.to_le_bytes());
        let mut reader = Reader::new(&data);
        parse_binding_list(&mut reader).unwrap();
        assert_eq!(reader.position, list_end);
        assert_eq!(reader.u32().unwrap(), 7);
    }

    #[test]
    fn effect_metadata_reads_counted_states_and_pass_relative_usage() {
        let mut data = vec![0; 0x50];
        for width in [16, 24, 68, 36] {
            data.extend_from_slice(&1u32.to_le_bytes());
            let start = data.len();
            data.resize(start + width, 0);
            if width == 16 {
                data[start] = 3;
                data[start + 3] = 0xfe;
            }
            if width == 68 {
                data[start + 4 + 7] = 0xf;
            }
        }
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&0x0100_0001u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        let pass_start = data.len();
        data.extend_from_slice(&[0; 24]);
        data[pass_start + 16..pass_start + 18].copy_from_slice(&24u16.to_le_bytes());
        data.extend_from_slice(&5u32.to_le_bytes());
        for word in [1u32, 0x1234_5678, 0, 0, 0] {
            data.extend_from_slice(&word.to_le_bytes());
        }
        let end = data.len();
        data.extend_from_slice(&TABLE_REFERENCE.to_le_bytes());
        let mut reader = Reader::new(&data);
        let metadata = parse_effect_metadata(&mut reader).unwrap();
        assert_eq!(reader.position, end);
        assert_eq!(reader.u32().unwrap(), TABLE_REFERENCE);
        assert_eq!(metadata.rasterizer_states[0].fill_mode, 3);
        assert_eq!(metadata.rasterizer_states[0].depth_bias, -2);
        assert_eq!(metadata.depth_stencil_states.len(), 1);
        assert_eq!(metadata.blend_states[0].render_targets[0].write_mask, 0xf);
        assert_eq!(metadata.sampler_states.len(), 1);
        assert_eq!(metadata.techniques[0].pass_count, 1);
        assert_eq!(
            metadata.passes[0]
                .resource_usage
                .as_ref()
                .unwrap()
                .srv_hashes,
            [0x1234_5678]
        );
        data[pass_start + 16..pass_start + 18].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(parse_effect_metadata(&mut Reader::new(&data)).is_err());
    }

    #[test]
    fn effect_header_decodes_vertex_inputs_and_preserves_unknown_fields() {
        let mask = (1u64 << 0) | (1 << 4) | (1 << 24) | (1 << 33);
        let flags = (1u64 << 52) | 8 | 2;
        let mut data = vec![0; 0x50 + 7 * 4];
        data[..8].copy_from_slice(&mask.to_le_bytes());
        data[8..16].copy_from_slice(&flags.to_le_bytes());
        for (slot, value) in [(0, 0xa3), (4, 0x91), (24, 0xf6), (33, 0x72)] {
            data[16 + slot] = value;
        }
        data[68..72].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        let extra_mask = (1u64 << 0) | (1 << 28) | (1 << 60);
        data[72..80].copy_from_slice(&extra_mask.to_le_bytes());
        let metadata = parse_effect_metadata(&mut Reader::new(&data)).unwrap();
        let inputs = &metadata.header.vertex_inputs;
        assert_eq!(inputs.attribute_mask, mask);
        assert!(inputs.uses_vertex_id() && inputs.uses_instance_id());
        assert_eq!(inputs.dxgi_format(0), Some(6));
        assert_eq!(inputs.dxgi_format(4), Some(41));
        assert_eq!(inputs.dxgi_format(24), Some(28));
        assert_eq!(inputs.dxgi_format(33), Some(16));
        assert_eq!(inputs.attribute_formats[24], 0xf6);
        assert_eq!(inputs.reserved_44, 0x1122_3344);
        assert_eq!(metadata.header.attribute_mask_48, extra_mask);
        assert_eq!(Sgd5VertexInputMetadata::semantic(33), Some(("TEXCOORD", 5)));
        assert_eq!(
            Sgd5VertexInputMetadata::semantic(51),
            Some(("TEXCOORD", 23))
        );
        assert_eq!(Sgd5VertexInputMetadata::semantic(52), None);
        let mut direct = inputs.clone();
        direct.shader_flags = 0;
        direct.attribute_formats[0] = 6;
        direct.attribute_formats[33] = 16;
        assert_eq!(direct.dxgi_format(0), Some(6));
        assert_eq!(direct.dxgi_format(33), Some(16));
        assert_eq!(direct.dxgi_format(52), None);
    }

    #[test]
    fn reflection_sources_support_shared_inline_and_absent_tables() {
        let data = TABLE_REFERENCE.to_le_bytes();
        assert!(matches!(
            parse_reflection_source(&mut Reader::new(&data)).unwrap(),
            Sgd5ReflectionSource::Shared
        ));
        let data = 0u32.to_le_bytes();
        assert!(matches!(
            parse_reflection_source(&mut Reader::new(&data)).unwrap(),
            Sgd5ReflectionSource::Absent
        ));
        let data = empty_reflection();
        assert!(matches!(
            parse_reflection_source(&mut Reader::new(&data)).unwrap(),
            Sgd5ReflectionSource::Inline(_)
        ));
    }

    #[test]
    fn stream_scanning_skips_magic_inside_shader_payloads() {
        let mut stream = empty_stream();
        stream.truncate(29);
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&[3, b'V', b'S', 0, 32]);
        stream.extend_from_slice(&16u32.to_le_bytes());
        stream.extend_from_slice(&COMPRESSED_MAGIC.to_le_bytes());
        stream.extend_from_slice(&16u32.to_le_bytes());
        stream.extend_from_slice(&32u32.to_le_bytes());
        stream.extend_from_slice(MAGIC);
        stream.extend_from_slice(&0u64.to_le_bytes());
        stream.extend_from_slice(&[0; 0x90]);
        stream.extend(empty_reflection());
        stream.extend_from_slice(&[0; 24]);
        stream.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let expected_size = stream.len();
        stream.extend_from_slice(&[0; 7]);
        stream.extend(empty_stream());
        let files = parse_sgd5_streams(Path::new("synthetic"), &stream).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].data.len(), expected_size);
        assert_eq!(files[0].shaders.len(), 1);
        assert_eq!(files[0].shaders[0].shader_metadata, [0; 0x90]);
    }

    #[test]
    fn truncated_streams_return_errors_and_effect_counts_are_enforced() {
        let data = empty_stream();
        for length in 0..data.len() {
            assert!(parse_sgd5_data(Path::new("synthetic"), &data[..length]).is_err());
        }
        assert!(parse_sgd5_data(Path::new("synthetic"), &data).is_ok());
        let mut invalid = data.clone();
        invalid[53..57].copy_from_slice(&1u32.to_le_bytes());
        assert!(parse_sgd5_data(Path::new("synthetic"), &invalid).is_err());
    }

    #[test]
    #[ignore = "requires PERCY_SGD5_SAMPLE pointing to the analysis archive"]
    fn sample_archive_uses_common_tables_and_decompresses_each_stage() {
        let path =
            PathBuf::from(std::env::var_os("PERCY_SGD5_SAMPLE").expect("set PERCY_SGD5_SAMPLE"));
        let files = load_sgd5(&path).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            files.iter().map(|file| file.shaders.len()).sum::<usize>(),
            26_503
        );
        assert_eq!(
            files.iter().map(|file| file.effects.len()).sum::<usize>(),
            932
        );
        let mut reference_count = 0;
        let mut auxiliary_count = 0;
        let mut table_count = 0;
        let mut member_link_count = 0;
        let mut engine_counts = [0usize; 9];
        for file in &files {
            let tables = std::iter::once(&file.shared_reflection)
                .chain(file.shaders.iter().map(|shader| &shader.reflection))
                .chain(
                    file.effects
                        .iter()
                        .flat_map(|effect| [&effect.main_reflection, &effect.property_reflection]),
                );
            for table in tables {
                table_count += 1;
                let mut linked_auxiliary_count = 0;
                for resource in &table.resources {
                    assert_eq!(resource.reserved_0a, 0);
                    assert_eq!(resource.member_count == 0, resource.member_data_offset == 0);
                    if resource.member_count != 0 {
                        member_link_count += 1;
                        for index in 0..resource.member_count as usize {
                            let target =
                                resource.offset + resource.member_data_offset as usize + index * 24;
                            assert!(table.members.iter().any(|member| member.offset == target));
                        }
                    }
                    assert_eq!(
                        resource.auxiliary_record_count == 0,
                        resource.auxiliary_data_offset == 0
                    );
                    for index in 0..resource.auxiliary_record_count as usize {
                        let target =
                            resource.offset + resource.auxiliary_data_offset as usize + index * 8;
                        assert!(table
                            .auxiliary_records
                            .iter()
                            .any(|record| record.offset == target));
                    }
                    linked_auxiliary_count += resource.auxiliary_record_count as usize;
                }
                for member in &table.members {
                    assert_eq!(member.reserved_06, 0);
                    assert_eq!(member.reserved_08, 0);
                    assert_eq!(member.reserved_0e, 0);
                    assert_eq!(
                        member.auxiliary_record_count == 0,
                        member.auxiliary_data_offset == 0
                    );
                    for index in 0..member.auxiliary_record_count as usize {
                        let target =
                            member.offset + member.auxiliary_data_offset as usize + index * 8;
                        assert!(table
                            .auxiliary_records
                            .iter()
                            .any(|record| record.offset == target));
                    }
                    linked_auxiliary_count += member.auxiliary_record_count as usize;
                }
                assert_eq!(linked_auxiliary_count, table.auxiliary_records.len());
            }
            assert_eq!(file.declared_effect_count, file.effects.len() as u32);
            assert!(file.shared_reflection.default_value_data.is_empty());
            for stage in STAGES {
                if let Some(shader) = file.shaders.iter().find(|shader| shader.stage == stage.0) {
                    assert_eq!(&decompress_shader(file, shader).unwrap()[..4], b"DXBC");
                    assert!(shader.reflection.default_value_data.is_empty());
                    let metadata = crate::sgd5_metadata::format_shader_metadata(file, shader);
                    assert!(metadata.contains("DXBC input/output signatures"));
                    assert!(!metadata.contains("DXBC signatures unavailable"));
                    assert!(metadata.contains("Binding table inventory"));
                }
            }
            for effect in &file.effects {
                let inputs = &effect.metadata.header.vertex_inputs;
                let mut mask_union = 0;
                let mut flag_union = 0;
                for index in &effect.stage_indices[0] {
                    let shader = file
                        .shaders
                        .iter()
                        .find(|shader| shader.stage == "VS" && shader.index == *index)
                        .unwrap();
                    let input = shader.vertex_inputs().unwrap();
                    mask_union |= input.attribute_mask;
                    flag_union |= input.shader_flags;
                }
                assert_eq!(inputs.attribute_mask, mask_union, "{}", effect.name);
                assert_eq!(inputs.shader_flags, flag_union, "{}", effect.name);
                let format_mask = inputs
                    .attribute_formats
                    .iter()
                    .enumerate()
                    .fold(0u64, |mask, (slot, value)| {
                        mask | (u64::from(*value != 0) << slot)
                    });
                assert_eq!(inputs.attribute_mask, format_mask, "{}", effect.name);
                assert_eq!(inputs.reserved_44, 0);
                assert_eq!(effect.metadata.header.attribute_mask_48 >> 52, 0);
                assert!(matches!(
                    effect.shared_reflection_source,
                    Sgd5ReflectionSource::Shared
                ));
                for (sum, value) in engine_counts[..6].iter_mut().zip([
                    effect.metadata.rasterizer_states.len(),
                    effect.metadata.depth_stencil_states.len(),
                    effect.metadata.blend_states.len(),
                    effect.metadata.sampler_states.len(),
                    effect.metadata.techniques.len(),
                    effect.metadata.passes.len(),
                ]) {
                    *sum += value;
                }
                for pass in &effect.metadata.passes {
                    let usage = pass.resource_usage.as_ref().unwrap();
                    engine_counts[6] += usage.srv_hashes.len();
                    engine_counts[7] += usage.uav_hashes.len();
                    engine_counts[8] += usage.cbv_hashes.len();
                    assert!(usage.sampler_hashes.is_empty());
                }
                assert_eq!(effect.property_reflection.resource_count, 1);
                assert_eq!(
                    effect.property_reflection.resources[0].name_hash,
                    0xe435_592b
                );
                assert!(effect.property_reflection.default_value_data.is_empty());
                auxiliary_count += effect.main_reflection.auxiliary_records.len();
                reference_count += effect
                    .binding_list
                    .entries
                    .iter()
                    .filter(|entry| matches!(entry, Sgd5BindingListEntry::TableReference { .. }))
                    .count();
            }
        }
        assert_eq!(reference_count, 3_247);
        assert_eq!(auxiliary_count, 5_605);
        assert_eq!(table_count, 28_369);
        assert_eq!(member_link_count, 94_397);
        assert_eq!(
            engine_counts,
            [64, 124, 398, 10_650, 25_037, 28_798, 354_283, 12_297, 81_510]
        );
    }
}
