# SGD5 serialized layout

This C-style representation describes the serialized SGD5 stream, based on the current parser, executable reader, and supplied archive. It is format pseudocode: arrays may depend on earlier fields, and unions consume only the selected alternative. It is not a compilable C declaration or the engine's in-memory layout.

All integers are little-endian. Records are packed without implicit C alignment. `u8/u16/u32/u64` denote unsigned integers of that width, `i8` is signed, and `f32` is a 32-bit IEEE-754 float. Bitfield positions below are specified explicitly; do not rely on a C compiler's bitfield allocation order.

```c
// ---------- Reflection ----------

struct resource_record {                  // 0x18 bytes
    u16 resource_type;                    // +0x00: kind/category/array/flags/shape; see notes
    u8  slot;                            // +0x02: material SRVs use a handle-array index; see notes
    u8  auxiliary_record_count;           // +0x03
    u8  member_count;                     // +0x04
    u8  flags;                            // +0x05
    u16 member_data_offset;               // +0x06: relative to this record
    u16 name_string_offset;               // +0x08: relative to this record
    u16 reserved_0a;                      // +0x0A
    u16 cbuffer_size;                     // +0x0C: CBV byte size
    u16 auxiliary_data_offset;            // +0x0E: relative to this record
    u32 name_hash;                        // +0x10
    u32 secondary_hash;                   // +0x14: individual meaning unresolved
};

struct member_record {                    // 0x18 bytes
    u8  value_type;                       // +0x00
    u8  auxiliary_record_count;           // +0x01
    u16 array_size;                       // +0x02
    u16 byte_offset;                      // +0x04: offset within parent data layout
    u16 reserved_06;                      // +0x06
    u16 reserved_08;                      // +0x08: not a confirmed name offset
    u16 default_value_offset;             // +0x0A: relative to this record
    u16 auxiliary_data_offset;            // +0x0C: relative to this record
    u16 reserved_0e;                      // +0x0E
    u32 name_hash;                        // +0x10
    u32 secondary_hash;                   // +0x14
};

// For either record:
// parameter_key = (u64)name_hash | ((u64)secondary_hash << 32).
// Members also describe Buffer element metadata, not just CBV constants.

struct auxiliary_record {                 // 0x08 bytes; not a binding_entry
    u32 value_kind  : 2;                  // bits 0..1: 0 integer, 1 float, 2 string, 3 unknown
    u32 name_hash30 : 30;                 // bits 2..31: truncated lowercase JOAAT hash
    union {
        u32 integer_value;
        f32 float_value;
        u32 string_offset;               // kind 2: relative to this auxiliary record
        u32 raw;
    } value;
};

struct reflection_table {
    u16 resource_count;
    u16 member_count;
    u16 auxiliary_record_count;
    u16 size;                            // whole table, including this 8-byte header

    struct resource_record resources[resource_count];
    struct member_record members[member_count];
    struct auxiliary_record auxiliary_records[auxiliary_record_count];

    // Derived lengths, NOT additional serialized fields:
    u8 string_pool[string_pool_bytes];    // NUL-delimited; may be empty
    u8 default_value_data[default_value_bytes]; // raw typed-by-member data; may be empty
};

struct sized_reflection_table {
    u32 table_size;                       // following table only; excludes this u32
    struct reflection_table table;       // table.size == table_size
};

// ---------- Binding layouts ----------

struct binding_entry {                    // 0x08 bytes
    u32 name_hash;
    u16 binding_type;                     // low byte kind; high byte shape/flags; see notes
    u16 binding_index;                    // runtime descriptor index = binding_index & 0x03FF
};

struct binding_table {
    u16 entry_count;                      // +0x00
    u8  unknown_02;                       // +0x02
    u8  srv_count;                        // +0x03
    u8  unknown_04;                       // +0x04
    u8  uav_count;                        // +0x05
    u8  sampler_count;                    // +0x06
    u8  cbv_count;                        // +0x07
    u32 table_hash;                       // +0x08: shared-table lookup identity
    u32 unknown_0c;                       // +0x0C
    struct binding_entry entries[entry_count];
};

struct sized_binding_table {
    u32 table_size;                       // following hash + table; excludes this u32
    u64 hash;
    struct binding_table table;           // table_size == 8 + 16 + 8 * entry_count
};

union binding_list_entry {                // selected by the first u32
    struct {
        u32 marker = 0x80000000;
        u32 table_hash;                   // references a header binding table
    } shared;
    struct sized_binding_table inline_table; // first u32 != 0x80000000
};

struct binding_list {
    u32 entry_count : 24;                 // bits 0..23
    u32 texture_handle_count : 8;         // bits 24..31; padded to four u32 entries
    union binding_list_entry entries[entry_count];
};

// ---------- Shaders ----------

struct shader_archive {
    u32 magic = 0x000DDCE7;
    u32 archive_size;                     // includes this 0x0C-byte header
    u32 decompressed_size;
    u8 deflate_blob[archive_size - 0x0C];  // raw DEFLATE; decompressed compiled container
};

struct shader {
    u8 name_length;                       // includes terminating NUL
    u8 name[name_length];
    u8 shader_version;                    // observed: 50, 60, 64, 65, 66; not wave size
    u32 shader_archive_size;
    struct shader_archive archive;       // archive_size == shader_archive_size
    u64 sga_hash;
    u8 shader_metadata[0x90];             // VS first 0x48: vertex_input_metadata below
                                         // other bytes partly decoded; root signature is DXBC PRIV
    struct sized_reflection_table reflection;
};

struct shader_array {
    u32 count;
    struct shader shaders[count];
};

struct shader_index_array {
    u32 count;
    u32 shader_indices[count];            // zero-based indices in matching file stage array
};

// ---------- Effect states ----------

struct vertex_input_metadata {            // 0x48 bytes; VS metadata and effect prefix
    u64 attribute_mask;                   // +0x00: one bit per semantic slot
    u64 shader_flags;                     // +0x08: bits 1 InstanceID, 3 VertexID, 52 compact formats
    u8 attribute_formats[52];             // +0x10: semantic slots; retain compact high nibble
    u32 reserved_44;                      // +0x44: zero in the supplied effects
};

struct effect_header {                    // 0x50 bytes
    struct vertex_input_metadata vertex_inputs;
    u64 attribute_mask_48;                // +0x48: extra semantic mask; selection policy unknown
};

struct rasterizer_state {                 // 0x10 bytes
    u8 fill_mode;
    u8 cull_mode;
    u8 front_counter_clockwise;
    i8 depth_bias;
    f32 depth_bias_clamp;
    f32 slope_scaled_depth_bias;
    u8 depth_clip_enable;
    u8 unknown_0d;
    u8 multisample_enable;
    u8 flags;                           // bit 0 antialiased lines; bit 1 conservative rasterization
};

struct stencil_face {                     // 0x04 bytes
    u8 fail_op;
    u8 depth_fail_op;
    u8 pass_op;
    u8 comparison_func;
};

struct depth_stencil_state {              // 0x18 bytes
    u8 depth_enable;
    u8 depth_write_mask;
    u8 depth_comparison_func;
    u8 stencil_enable;
    u8 stencil_read_mask;
    u8 stencil_write_mask;
    struct stencil_face front_face;
    struct stencil_face back_face;
    u8 unknown_0e;
    u8 depth_bounds_test_enable;
    u8 unknown_tail[8];
};

struct render_target_blend_state {        // 0x08 bytes
    u8 blend_enable;
    u8 source_blend;
    u8 destination_blend;
    u8 blend_op;
    u8 source_blend_alpha;
    u8 destination_blend_alpha;
    u8 blend_op_alpha;
    u8 write_mask;
};

struct blend_state {                     // 0x44 bytes
    u8 alpha_to_coverage_enable;
    u8 independent_blend_enable;
    u16 unknown_02;
    struct render_target_blend_state render_targets[8];
};

struct sampler_state {                   // 0x24 bytes
    u8 preset_index;
    u8 address_u;
    u8 address_v;
    u8 address_w;
    f32 mip_lod_bias;
    u8 comparison_bits;                 // low five bits consumed as comparison; retain upper bits
    u8 max_anisotropy;
    u16 filter_bits;                    // packed encoding retained
    f32 border_color[4];
    f32 min_lod;
    f32 max_lod;
};

// ---------- Techniques, passes, and usage ----------

struct technique {                       // one packed u32
    u32 name_string_offset : 15;         // bits 0..14; relative to effect string pool
    u32 first_pass         : 9;          // bits 15..23; zero-based
    u32 pass_count         : 8;          // bits 24..31; contiguous pass range
};

struct pass {                            // 0x18 bytes
    u8 shader_slots[6];                  // +0x00: VS, PS, GS, DS, HS, CS
                                         // zero absent; otherwise one-based effect-array slot
    u8 rasterizer_state_index;           // +0x06: zero-based when enabled
    u8 depth_stencil_state_index_low;    // +0x07: state_flags bit 6 supplies index bit 8
    u8 blend_state_index_low;            // +0x08: state_flags bit 7 supplies index bit 8
    u8 stencil_reference;               // +0x09: applied when state_flags bit 3 is set
    u8 state_flags;                      // +0x0A: bits 0/1/2 enable rasterizer/depth/blend
                                         // bit 3 enables stencil reference; bits 4/5 remain unresolved
    u8 unknown_0b[5];                    // +0x0B
    u16 usage_data_offset;               // +0x10: see address rule below; zero means absent
    u8 binding_table_indices[4];         // +0x12: binding-list indices for spaces0..3; 0xFF absent
    u16 unknown_16;                      // +0x16
};

struct hash_array {
    u32 count;
    u32 hashes[count];
};

// A view into pass_lookup_words, NOT a second sequential block after each pass.
struct pass_resource_usage {
    struct hash_array srvs;
    struct hash_array uavs;
    struct hash_array samplers;
    struct hash_array cbvs;
    u32 texture_handle_binding[effect.binding_list.texture_handle_count != 0];
    // Low byte: flattened root-constant dword, 0xFF absent. Value <0x100 enables CBV fallback.
    // Other bits and remaining lookup words are preserved.
};

struct effect_metadata {
    struct effect_header header;        // no recursive continuation header

    u32 rasterizer_state_count;
    struct rasterizer_state rasterizer_states[rasterizer_state_count];

    u32 depth_stencil_state_count;
    struct depth_stencil_state depth_stencil_states[depth_stencil_state_count];

    u32 blend_state_count;
    struct blend_state blend_states[blend_state_count];

    u32 sampler_state_count;
    struct sampler_state sampler_states[sampler_state_count];

    u32 technique_count;
    struct technique techniques[technique_count];

    u32 pass_count;
    struct pass passes[pass_count];

    u32 pass_lookup_word_count;
    u32 pass_lookup_words[pass_lookup_word_count];
};

union reflection_source {                // selected by the first u32
    u32 selector;                       // 0: absent; 0x80000000: archive shared reflection
    struct sized_reflection_table inline_table; // otherwise selector IS table_size
};

struct effect {
    u8 name_length;                      // includes terminating NUL
    u8 name[name_length];
    u32 data_buffer_size;                // engine allocation size, not serialized effect size

    struct shader_index_array VS;
    struct shader_index_array PS;
    struct shader_index_array GS;
    struct shader_index_array DS;
    struct shader_index_array HS;
    struct shader_index_array CS;

    struct effect_metadata metadata;
    union reflection_source shared_reflection_source;
    struct sized_reflection_table main_reflection;
    struct sized_reflection_table effect_properties; // previously secondary_reflection; proplst
    struct binding_list bindings;        // previously called the property list

    u32 string_pool_size;
    u8 string_pool[string_pool_size];    // NUL-delimited technique names and other strings
};

// ---------- Complete stream ----------

struct sgd5_file {
    u8 magic[4] = {'S', 'G', 'D', '5'};
    u64 group_hash;

    u32 binding_table_count;
    struct sized_binding_table binding_tables[binding_table_count];
    struct sized_reflection_table shared_reflection;

    struct shader_array VS;
    struct shader_array PS;
    struct shader_array GS;
    struct shader_array DS;
    struct shader_array HS;
    struct shader_array CS;

    u32 effect_count;
    struct effect effects[effect_count];

    u32 trailer_flags;                  // observed as 0 or 1; semantics unresolved
    u8 trailer_reserved[6];             // observed zero; 10 trailer bytes total
};
```

Reflection member, name, default-value, and auxiliary offsets are relative to the beginning of the record that contains them. A nonzero member link addresses `member_count` consecutive 24-byte records; an auxiliary link addresses the indicated count of 8-byte records. The member's `byte_offset` instead describes packing within its parent data layout. Zero optional links mean absent data.

There are no serialized string/default-data lengths inside a reflection table. The current parser separates its trailing bytes at the earliest nonzero member default-value target; without such a target, it treats the entire tail as string-pool bytes. The tail's total length is `size - 8 - 24 * resource_count - 24 * member_count - 8 * auxiliary_record_count`. Default data should remain raw bytes until interpreted using the member type and layout; it is not uniformly a float array. Member names can be resolved by hash from available string pools rather than assigning `reserved_08` a name-offset meaning.

For a pass with a nonzero usage link, the serialized target is `pass_file_offset + usage_data_offset + 4`. The extra four bytes account for the lookup-array count omitted from the engine's copied in-memory buffer. The four counted hash lists are views into the stored lookup words, which must still be retained in full.

The effect's `data_buffer_size` is checked against the loader's runtime allocation: six shader pointer arrays (including their terminators), 8 bytes per technique, 32 bytes per pass, all lookup words, copied reflection tables, an aligned binding-pointer array plus inline binding tables, and the string pool. Render/sampler states are interned separately. Serialized depth/blend index bytes are combined with their flag high bits before lookup; the executable then remaps local state indices to global interned state IDs.

Native shader variants, compiler root/usage tables, and stage-specific metadata are documented in [FSSM.md](FSSM.md). The SGD5 stage library and actual compiled hardware stage are distinct; many VS-library entries are mesh shaders, and 195 VS containers carry both VS and ES variants.

Vertex semantic slots 0..27 are four indices each of POSITION, NORMAL, TANGENT, BINORMAL, BLENDWEIGHT, BLENDINDICES, and COLOR, in that order. Slots 28..51 are TEXCOORD0..23. These are semantic slots, not D3D vertex-buffer input slots or signature registers. With `shader_flags` bit 52 clear, per-VS format bytes are DXGI_FORMAT values. With it set, low-nibble codes 1..13 map to DXGI values `{41,16,6,2,30,28,24,38,34,12,10,3,17}`; the high nibble remains unresolved. Other flag bits are retained, including previously decoded root-argument fields.

Across all 932 supplied effects, `attribute_mask` and `shader_flags` equal the respective bitwise unions of their referenced VS metadata, and the attribute mask equals the presence of nonzero format bytes. The effect format merge rule is unconfirmed: the entire first 0x48 bytes cannot be reconstructed by a bytewise union. Applying the VS format decoding to the matching effect layout is provisional until its direct consumer is traced. The final `attribute_mask_48` uses the same semantic numbering but is neither a simple VS union nor intersection; retain it independently. See [SGD5.md](SGD5.md) for consumers, examples, and evidence.

The two supplied streams use shared reflection for every effect. Inline/absent reflection-source alternatives are supported by the executable reader but were not observed there. The trailer representation is the current parser's grouping of the ten observed bytes; its runtime interpretation remains unresolved. It replaces the earlier assumed two-byte `eof_flag = 0x100`.

Resource slots and binding indices belong to distinct numbering schemes. Material SRV slots have the handle-array use described below; per-shader registers combine slot with resource-type register bits. The four pass binding selectors correspond to register spaces0..3; compute uses shader metadata selectors instead. See [SGD5.md](SGD5.md) for evidence, runtime roles, and remaining limits.

`resource_type` bits 0..3 are the base kind, bits 4..5 select SRV/UAV/sampler/CBV (0/1/2/3), bit 6 is an array modifier, and bit 7 supplies shader register bit 8. Bits 8..11 carry the shape code; bit 13 supplies shader register bit 9 but is not set in the supplied records. `binding_type` instead uses the whole low byte for the base kind, bits 8..13 for the shape argument, bit 14 for a separate unresolved binding flag, and bit 15 for the array modifier. See the packed-type section in [SGD5.md](SGD5.md) for the executable consumption and conversion formula.

In every supplied effect, `effect_properties` contains one zero-size CBV-kind resource named by the hash of `proplst`, no members, and all of that table's auxiliary property records. Both resource hashes are `0xE435592B`; its name offset is zero. The executable queries those auxiliary records for effect properties. In the older SGD2 reference variants, the trailing literal `proplst\0` is addressed by this resource's name field, so it belongs to the preceding reflection table rather than naming the following binding layouts.

For interface-selected material SRVs with `(flags & 0xC0) == 0x80`, `slot` indexes a per-material u32 texture-handle array. The binding-list header's high byte is that array's padded entry count, previously called `flags`. The executable allocates four bytes per entry, writes resolved texture handles through the slot mapping, and supplies the array to a constant-data binding path. `0xFF` denotes an absent mapping there; zero is valid. For per-shader records, register = slot | ((resource_type & 0x0080) << 1) | ((resource_type & 0x2000) >> 4), and space = flags >> 6. These register semantics must not be applied to effect material slots.
