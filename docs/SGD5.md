# SGD5 metadata and shader interpretation

SGD5 contains compiled shaders plus an effect database describing how the engine organizes, selects, and initializes them. The supplied two-stream archive has been cross-checked against static analysis of `game_scarlett_master.exe`: the database loader is at `0x142499630`, the effect reader at `0x14247B6C0`, and parameter-interface construction at `0x14247C8C0` (image base `0x140000000`). Compiled registers and selected descriptor layouts are now confirmed. See [the runtime pipeline](SGD5-pipeline.md) for caller data, material storage, root submission, and shader access.

The current [C-style serialized layout](SGD5-layout.md) documents record widths, field order, offset bases, and conditional table forms.

## Browser and shader views

The SGD5 browser follows effect → technique → pass → shader. Search keeps matching ancestors visible and supports effect/technique/shader names, shader indices and SGA hashes, with a stage filter. Each effect also exposes its complete declared variant library; archive shared reflection, shared binding tables, and the complete shader library remain directly accessible. Passes outside techniques and shaderless passes are retained.

Effect inspection shows material reflection, member packing/default bytes, typed `proplst` properties, vertex-input summaries, binding layouts, the effect-owned string pool, and render/sampler state libraries. Pass inspection shows selected stage shaders, active rasterizer/depth/blend states, per-space layouts, and verified parameter bindings. Compute uses the selected CS shader's metadata selectors rather than graphics pass selectors.

Shader selection retains its effect, technique, and pass context. The Parameters tab maps resource identities to compiled registers/spaces and root locations, plus parent-member byte offsets and the selected runtime layout. ASM/HLSL exports include the same metadata and selected pass context. Runtime mappings distinguish descriptor indices, direct roots, constants, static samplers, and material texture handles; they do not assign speculative names to ISA registers. Unsupported FSSM programs can still be inspected through metadata and exported as DXBC.

Navigation, decompressed programs, metadata, and rendered shader text are cached. Disassembly and lifted HLSL consume the complete decoded program through code-end padding, without the former 4,000-instruction preview cap. The parser validates reflection member/auxiliary/string links, known default-value extents, aligned pass usage links, and selected compute layout selectors while retaining unresolved fields.

The shader inspector also enumerates every XDAT/XBCH native variant, displays its actual hardware stage, and defaults to a matching VS/PS/GS/CS variant when available. Compiled metadata includes VGPR/SGPR counts, wave size, LDS, mesh limits, fetch/interpolation/output maps, and pixel export alternatives. Parameters and shader-text exports join compiler resource usage to original `PRIV` bindings and reflection names, with compiler user-data/SGPR locations and structured-buffer stride information. Unresolved joins remain explicit; original instructions are never patched for inspection. See [FSSM.md](FSSM.md) for field layouts and validation.

Pass byte +9 is the stencil reference; flag bits 6/7 supply depth/blend index bit 8. Rasterizer byte +14 is multisample enable, and bits 0/1 of +15 select antialiased lines/conservative rasterization. Depth/stencil +15 enables depth-bounds testing. Sampler inspection decodes addresses, bias/LOD bounds, border color, preset index, anisotropy and retained filter/comparison bits. These fields were checked through the executable's state-expansion path. Runtime effect allocation sizes are also recomputed and validated.

## What the parts provide

| Part | Observed contents | Likely engine role |
| --- | --- | --- |
| Header binding tables | Resource-name hashes, packed binding kinds, indices, group counts, table identities | Reusable binding layouts; effects reference these by hash |
| Shared reflection | Resource descriptions and member layouts reused across shader/effect records | Common logical resource catalog and layout metadata |
| Per-stage shader arrays | Compiled containers, shader hashes, shader-version byte, partly decoded 144-byte metadata, reflection | Shader variants available to the effect database; VS metadata describes vertex inputs |
| Per-shader reflection | Resources and typed/packed member records | Logical resource usage and value layouts for that variant |
| Effect shader index arrays | References into each stage array | The variants available to that effect |
| Effect metadata tables | Vertex-input summary and an additional mask in the 80-byte prefix, render/sampler states, techniques, passes, and per-pass hash lists | Vertex-input metadata, technique/pass selection, pipeline state, shader selection, and logical parameter usage |
| Main effect reflection | Resources, members, auxiliary records, strings, default data | Effect/material parameter descriptions and initialization data |
| Effect property reflection (previously secondary reflection) | One synthetic `proplst` resource with typed auxiliary key/value records in every supplied effect | Effect properties queried for bucket classification, feature selection, and material flags |
| Effect binding list (previously property list) | Inline binding layouts and references to header layouts | Distinct resource layouts, with shared groups and locally stored groups |
| Final effect string pool | Names such as `ClearCube`, `GenerateCube`, `p0`, `p1` | Technique names are directly referenced by packed technique records; remaining strings retain their original spelling |

The effect index arrays collectively reference every shader in both supplied streams: 26,503 shaders and 932 effects, with no unreferenced shader indices. Passes select one-based slots in these arrays; techniques specify contiguous pass ranges.

Expanding an effect's property list does not produce a complete inventory of its reflection hashes. Member variables can belong to a CBV instead of appearing as individual binding entries. Conversely, shared tables contain broader resource sets than any one effect uses.

Reflection and binding tables describe different relationships. Reflection supplies logical resource/member types, names, packing, auxiliary metadata, and default data. A binding table maps resource hashes into an ordered layout with SRV/UAV/sampler/CBV groups. Pass records select layouts for register spaces 0..3, with `0xFF` for absent selections. Compiled PRIV root signatures map each layout's descriptor indices to actual registers. Compute uses the four selectors at shader metadata +88..91.

Effects are plausible engine-facing definitions for rendering or compute work: look up an effect, select a technique/pass/variant, choose its stage shaders and state, populate its resource tables and constant data, then submit work. The archive does not establish whether the runtime calls this an effect, material, pipeline family, or another object.

## How reflection reaches the runtime

The shared, main-effect, and per-shader tables use one serialized reflection format. Shared and main-effect tables use the same resource-registration function, `0x140F9FFA0`; the compute-shader path also passes its per-shader table to that function. This is confirmed by its callers:

- The database loader `0x142499630` processes the header shared table before loading the stage arrays and effects.
- The effect reader `0x14247B6C0` processes the main table at effect `+3208`. It also processes the first-position reflection source at `+3216` when that source is a distinct inline table; a reference to the already-processed archive table is reused.
- Shader initialization `0x140F9F650` processes the shader reflection pointer at shader `+216` (`0xD8`). Its direct loader caller is the compute path at `0x14247CFB0`; equivalent registration for every graphics stage has not been established.

The common function separates resources by category, registers primary/secondary hash lookups and scope information, and creates or merges global resource definitions for resources outside the exposed parameter-interface path. Sampler-category records are skipped in this function. Definition construction at `0x1413F60F0` follows member and auxiliary links and preserves packing, shape, flags, names, and CBV size. Member construction at `0x1413F6E60` copies linked default bytes using the value type's size and the member array count. Those defaults are runtime initialization metadata, not proof of compile-time shader constants.

Sharing that preprocessing path does not make all later consumers interchangeable:

| Reflection role | Confirmed later consumption | Remaining limits |
| --- | --- | --- |
| Header shared / first-position source | Supplies common definitions to the global registries; effects reuse its pointer | All later direct consumers of the retained effect pointer and the policy for putting resources here remain unresolved |
| Main effect | `0x14247C8C0` constructs the exposed SRV/UAV/sampler/CBV/member interface and per-pass parameter usage; material texture slots feed the handle array | Defaults, hash-based value/texture setters, cloning, and serialized material remapping are traced; original interface-property names remain uncertain |
| Main effect, graphics passes | `0x141D65750` retains the main reflection pointer at combined graphics-shader object `+96`, alongside selected stage shaders and binding layouts | Selected binding layouts join to compiled root ranges; some retained fields remain opaque |
| Per-shader | Common registration confirmed on the compute path; retention on stage shader objects and resource-category/scope masks derived from their records | Equivalent registration for every graphics stage remains unconfirmed; compiled registers and root descriptors are verified |
| Effect properties (`proplst`) | Typed property lookups, bucket classification, feature/callback selection, and material/object flags | Most original property names and some downstream behaviors remain unresolved |

The stage readers compute category/scope usage masks from their shader reflection records: SRV, UAV, and CBV set different bits, further shifted by the high two bits of resource `flags`. Graphics-shader construction combines those masks from its selected stages. This is direct runtime consumption of variant-specific reflection, and the same high flag bits directly encode the shader register space. Per-shader register reconstruction is documented below; ISA SGPR assignment is a separate compiler ABI.

The `proplst` table is not passed to the common resource-registration function by the effect reader. Its zero-size synthetic resource is instead the owner of typed properties queried directly by the property accessors. The binding tables remain another structure: they map hashes to layouts using the established registries and are selected by passes or shader metadata.

## Confirmed metadata and limits

- Resource records have byte-sized slot/index, auxiliary count, member count, and flags fields. Material SRV slots index a per-material texture-handle array. Per-shader slots supply register bits 0..7; resource-type bits 7 and 13 supply register bits 8 and 9.
- Member records have a byte-sized value type and auxiliary count, array size, packing offset, default-data offset, and auxiliary-data offset. The second section also describes Buffer element metadata, so it is called `members` rather than `constant_members`.
- Record-relative member and auxiliary links are validated against the supplied archive. Parameter-interface construction carries the adjacent hashes as a 64-bit key (`name_hash | secondary_hash << 32`). The runtime can look up either hash through separate primary/secondary accessors. The original meaning of the secondary identity remains uncertain; both words are preserved.
- CBV byte sizes and member packing support layout reconstruction. Type `0x0F` remains unresolved, and defaults initialize material CBV members rather than establishing compile-time constants.
- `ISG1`/`OSG1` and supported legacy signature chunks supply compiled stage input/output semantics, component types, masks, streams, and signature register indices. These signature registers are not RDNA2 VGPR assignments or texture/buffer binding registers.
- Compiled `PRIV` roots join shader resources and selected binding layouts to exact registers and descriptor offsets. The 144-byte shader metadata supplies root-argument counts and compute layout selectors. Representative ISA accesses complete those joins; universal SGPR assignment remains unproven.
- The byte formerly named `wave_size` is a shader-version/profile code, observed as decimal 50, 60, 64, 65, and 66. These values match shader models 5.0, 6.0, 6.4, 6.5, and 6.6; the reader uses the code to select its shader-loading path. It is now named `shader_version`. Compiled wave size is read separately from bit 5 of FSSM `u32[0xD4]`.

## Effect header and vertex inputs

The formerly opaque 0x50-byte effect prefix contains a 0x48-byte vertex-input metadata block followed by a separate u64 mask. The effect reader at `0x14247B6C0` copies the first 72 bytes to runtime effect `+3232` (`0xCA0`) and the last eight bytes to `+3328` (`0xD00`); intervening runtime fields are not part of the serialized header. The same first 72-byte layout appears at the start of each VS shader's 144-byte metadata (shader object `+72`). This interpretation is specific to VS; it does not assign vertex semantics to PS/CS metadata or the shader block's second half.

| Header offset | Field | Confirmed interpretation |
| --- | --- | --- |
| `+0x00` | `u64 attribute_mask` | Named vertex-input presence, one bit per semantic slot; effect mask is the union of its VS masks |
| `+0x08` | `u64 shader_flags` | System-input flags and encoding/mode/root metadata; effect word is the union of its VS words |
| `+0x10` | `u8 attribute_formats[52]` | Per-semantic format bytes; executable decoding is confirmed on the matching per-VS layout |
| `+0x44` | `u32 reserved_44` | Zero in every supplied effect; preserved rather than skipped |
| `+0x48` | `u64 attribute_mask_48` | Additional mask using the semantic numbering below; selection policy and direct consumer unresolved |

The semantic table at `0x1461ED9D0`, accessed by `0x141043EE0`, establishes the slot numbering:

| Slots | Semantics |
| --- | --- |
| 0..3 | POSITION0..3 |
| 4..7 | NORMAL0..3 |
| 8..11 | TANGENT0..3 |
| 12..15 | BINORMAL0..3 |
| 16..19 | BLENDWEIGHT0..3 |
| 20..23 | BLENDINDICES0..3 |
| 24..27 | COLOR0..3 |
| 28..51 | TEXCOORD0..23 |

Flag bit 3 denotes `SV_VertexID` and bit 1 denotes `SV_InstanceID`. An independent DXBC signature scan checked all 2,236 conventional VS shaders: both flags matched their signature presence in every shader (1,456 VertexID and 508 InstanceID occurrences). All 1,779 conventional named VS input elements matched the corresponding presence bits. These semantic slots are not signature registers, vertex-buffer slots, or ISA VGPRs.

For per-VS formats, flag bit 52 selects compact encoding. When clear, the byte is a direct DXGI_FORMAT value. When set, consumers mask to the low nibble and convert it through `0x14247A600` and the table at `0x1461EDA5A`:

| Compact code | DXGI value | Format |
| ---: | ---: | --- |
| 1 | 41 | R32_FLOAT |
| 2 | 16 | R32G32_FLOAT |
| 3 | 6 | R32G32B32_FLOAT |
| 4 | 2 | R32G32B32A32_FLOAT |
| 5 | 30 | R8G8B8A8_UINT |
| 6 | 28 | R8G8B8A8_UNORM |
| 7 | 24 | R10G10B10A2_UNORM |
| 8 | 38 | R16G16_SINT |
| 9 | 34 | R16G16_FLOAT |
| 10 | 12 | R16G16B16A16_UINT |
| 11 | 10 | R16G16B16A16_FLOAT |
| 12 | 3 | R32G32B32A32_UINT |
| 13 | 17 | R32G32_UINT |

Zero denotes no format. Unsupported compact codes normalize to zero in that helper. The compact byte's high nibble is preserved without assigning a component-mask or register meaning. Remaining flag bits are preserved; some carry the root-argument metadata described in the pipeline notes.

The useful runtime consumers operate on per-VS metadata:

- Pipeline key construction at `0x140B5A5D0` calls `0x1453C46C0` to hash the 52 format slots, normalize compact formats, and account for the selected vertex streams.
- Input-layout construction at `0x14086FA70` filters D3D input-element descriptors against the shader's attribute mask and supplies defaults for missing inputs. `0x142477DC0` checks component compatibility; its `+67 == 0xEF` alternative handles a different packed signature representation.
- Vertex binding at `0x140033CC0` uses the selected VS mask, including a vertex-pulling/direct-buffer route. `0x14078B960` maps semantic slots to engine stream indices through `0x1461E0270`.

Across 932 effects, there are 241 distinct headers. Every first mask and flag word exactly matches the bitwise union of the referenced VS metadata, and every first mask exactly matches the nonzero format bytes. However, a bytewise union reproduces the complete 72-byte block in only 478 effects; even normalization followed by numerical maximum does not explain all format bytes. The format merge rule and the direct runtime use of the retained effect aggregate remain unconfirmed. PercyStar displays effect format conversions as candidates based on the matching VS layout, alongside their raw bytes. The supplied effects always have zero bytes at `+0x1C..1F` and `+0x34..47`; this does not make the unused semantic slots structurally reserved.

The final mask is independently meaningful, but its purpose cannot yet be named confidently:

- `cable`: both masks are `0x31000011`, selecting POSITION0, NORMAL0, COLOR0, TEXCOORD0, and TEXCOORD1.
- `cloth_default`: the aggregate mask is `0x11110011`, while the extra mask is `0x11000011`; the aggregate additionally includes BLENDWEIGHT0 and BLENDINDICES0.
- `alphaResolve`: the aggregate named-input mask is zero, but the extra mask is `0x11000011`; its VS uses VertexID.

The extra mask is neither a simple VS union nor intersection. A source/baseline vertex declaration is a plausible explanation, but is not established by the executable. All supplied extra masks fit the 52 semantic slots. The parser retains the entire word, including any future high bits.

This metadata supplies named input requirements and format context for disassembly/decompilation, and permits comparison with DXBC signatures. It does not by itself provide concrete vertex-buffer offsets/strides or ISA register assignments. PercyStar now decodes the effect prefix once through a typed header and also exposes the matching VS metadata, retaining unknown flags, raw format bytes, the reserved word, and the extra mask.

## Packed resource and binding types

The two u16 fields encode related information differently. The high nibble of a reflection resource's first byte is not one undifferentiated flags field:

| Reflection `resource_type` bits | Interpretation | Executable evidence |
| --- | --- | --- |
| 0..3 | Base resource kind | `0x1413F60F0` passes the low nibble to the reflection builder |
| 4..5 | Category: 0 SRV, 1 UAV, 2 sampler, 3 CBV | `0x14247C8C0`, `0x140F9FFA0`, and `0x141DF6F80` use `(first_byte >> 4) & 3` for interface arrays and hash registries |
| 6 | Resource-array modifier | `0x1413F60F0` extracts it separately; paired array texture layouts validate its interpretation |
| 7 | Shader register bit 8 | `0x1413F60F0` puts it in bit 8 of a slot/flags argument; `0x1413F7960` preserves it in the rebuilt resource record |
| 8..11 | Shape/dimension code | Reader and builder use the high byte's low nibble; observed 0/1/2/3/4 correspond to 1D/2D/cube/3D/buffer-like shapes |
| 13 | Shader register bit 9 (absent in this corpus) | `0x1413F60F0` places high-byte bit 5 in bit 9 of the same slot/flags argument; no supplied resource records set it |

The other upper bits remain unassigned. The reconstruction helper at `0x142476960`, using the table at `0x1461EDA4C`, obtains the category from the base kind and writes bits 4..5 back into the reflection record. Its observed base-kind assignments are SRV `{1,2,5,7,14}`, UAV `{4,6,8,12}`, sampler `{3}`, and CBV `{0}`. All 485,093 supplied resource records agree with this category mapping.

The binding entry's type is instead an 8-bit base kind followed by an 8-bit shape/flags byte:

| Binding `binding_type` bits | Observed consumption |
| --- | --- |
| 0..7 | Base kind; passed intact to `0x1408E88F0` |
| 8..13 | Shape/type argument; passed as `second_byte & 0x3F` |
| 14 | Separate unresolved flag; binding conversion preserves it in packed runtime bit 29 |
| 15 | Array modifier; binding conversion preserves it in packed runtime bit 31 |

`0x1408E8530` converts the supported serialized SRV/UAV/CBV binding groups to packed runtime entries. It resolves hashes through the category-specific registry at `0x1408E8930`, translates kind/shape through `0x1408E88F0`, and packs the low ten bits of `binding_index` into bits 16..25. The translated kind occupies bits 12..15; base kind 14 also sets bit 30. The descriptor index addresses a 32-byte descriptor within the selected root table. The compiled PRIV range at that index supplies the HLSL register; the packed global ID supplies the runtime resource. Neither field directly names an ISA SGPR.

The kind/shape helper returns these internal codes:

| Base kind | Category | Internal code from `0x1408E88F0` |
| ---: | --- | --- |
| 0 | CBV | 11 |
| 1 | SRV | 9 |
| 2 | SRV texture | shape argument |
| 3 | Sampler | 10 |
| 4 | UAV buffer subtype | 13 |
| 5, 7, 14 | SRV buffer subtypes | 8 |
| 6, 8 | UAV buffer subtypes | 12 |
| 12 | UAV texture | shape argument OR 4 |

Individual buffer-subtype HLSL names remain provisional. The two type encodings have the following relationship over every supplied binding/reflection pair that joins by name hash:

```c
base = binding_type & 0xFF;
shape_flags = binding_type >> 8;
normalized_resource_type = base | (category_for_base(base) << 4)
    | ((shape_flags & 0x3F) << 8)
    | ((shape_flags & 0x80) ? 0x0040 : 0);
// Compare type identity after removing the shader register bits (0x2080).
```

There are 124,107 matching pairs with zero type-conversion mismatches; 27 bindings have no same-scope reflection hash match. Examples:

- Binding `0x0403` corresponds to reflection `0x0423`: base kind 3, sampler category 2, shape 4. All 52,323 paired sampler occurrences match.
- Binding `0x0302` corresponds to reflection `0x0302` or `0x0382`: the 3D SRV kind/shape is the same, with shader register bit 8 clear or set.
- Binding `0x8102` corresponds to reflection `0x0142` or `0x01C2`: the array bit moves from binding bit 15 to reflection bit 6; reflection bit 7 carries a register bit independently of array type.
- Bindings `0x0400` and `0x4400` both correspond to reflection `0x0430`; the extra binding bit-14 flag has its own runtime packing path.

The current viewer retains the raw fields and masks reflection bits 7 and 13 for its type-name lookup. It must not discard the entire `0x00F0` nibble: doing so loses the category and array modifier.

## Resource slots and the material texture-handle array

The serialized `slot` is the byte at resource `+2`; resource `+3` is a separate auxiliary-record count. `0x1413F60F0` reads the slot and combines it with separately extracted type bits in a builder argument. `0x1413F7960` writes the argument's low byte back at resource `+2`, while restoring its extra flag bits to the type field. This establishes preservation without establishing a compiled register mapping.

Non-SRV resources also carry nonzero slots. Across all 485,093 resource-record occurrences in shared, shader, main-effect, and effect-property reflection:

| Non-SRV category | Records | Nonzero slot occurrences | Observed slot range |
| --- | ---: | ---: | --- |
| UAV | 16,355 | 16,355 | 1..65 |
| Sampler | 46,742 | 17,461 | 0..39 |
| CBV | 78,521 | 60,347 | 0..253 |

Sampler records occur only in main-effect reflection in the supplied archive. CBVs and UAVs also occur in shared and shader reflection. All 932 synthetic `proplst` CBVs have slot zero. The material texture-handle interpretation below therefore applies to a specific SRV path, not to the field universally.

There is a concrete indexing use in effect parameter-interface construction at `0x14247C8C0`. For resources selected by the interface property key `0x8715D53B` (decimal 2266355003), the SRV path initializes the mapping to `0xFF`. When the effect texture-handle count is nonzero and `(resource.flags & 0xC0) == 0x80`, it copies `resource.slot` and checks that it is below that count. Otherwise it retains `0xFF`. The flags here are the byte at resource `+5`, not the modifiers in `resource_type`.

The resulting map is stored at parameter interface `+8`, indexed by that interface's SRV ordinal. The shape byte at interface `+16` supplies fallback-handle selection. Texture handles are distinct from interface ordinals, global parameter IDs, and binding-table indices:

```c
texture_handle_slot[srv_ordinal] = resource.slot; // eligible material SRV
texture_handles[texture_handle_slot[srv_ordinal]] = resolved_texture_handle;
```

`0x144F966A0` allocates and initially populates a per-material array of `4 * texture_handle_count` bytes. `0x14249A790` refreshes the array, and `0x144F669B0` updates an individual entry using the same mapping. Resolved textures contribute `(texture_descriptor_pointer - pool_base) >> 5`; the fallback helper at `0x144F96880` selects a default handle by shape. The path at `0x144DA8BD0` normally supplies the array's offset within the shared GPU data buffer through a pass-selected root constant. Its fallback can bind the array as the built-in CBV with hash `0x5F759806`. In the cable sample, the shader reads handles from shared SRV hash `0x7B09BD38` at that offset and indexes the descriptor heap. See [the pipeline](SGD5-pipeline.md) for the complete trace.

This also resolves the high byte of the binding-list header: previously called `flags`, it is a **texture-handle count**, stored at runtime effect `+3325` (`0xCFD`). All 932 supplied effects have contiguous active material texture slots. In each effect, the header count equals that active slot extent rounded up to four u32 entries; 559 effects have a nonzero count, with observed counts 4, 8, 12, 16, 20, and 24. All 4,420 interface-selected material SRVs fit the declared count. Another 70 material-scope SRV records have slot `0xFF` and are excluded by the interface property predicate.

For example, `cable` has three mapped textures at slots 0, 1, and 2, with a four-entry handle array. `cloth_default` has one mapped texture at slot 0 and the same four-entry allocation. A slot value of zero is therefore valid, while `0xFF` is an absent mapping in this path.

This is evidence for an indexed/bindless material texture path. It does not establish that every shader/shared-reflection slot has the same role, nor that a slot is an HLSL `tN`, a descriptor-heap index itself, or an RDNA SGPR/VGPR assignment. Preserve the generic slot field until those remaining consumers are resolved.

## Effect properties and the `proplst` name

The third fixed reflection position is an effect-property container. Across all 932 supplied effects it contains one resource with `resource_type = 0x0430`, both hashes equal to `joaat_lower("proplst") = 0xE435592B`, no members, and zero CBV data size. That resource's auxiliary count and record-relative link cover the entire table's auxiliary array. There are 9,365 such records: 9,323 integer-kind values and 42 float-kind values. Its name offset is zero and the remaining string pool is one NUL byte, so the literal resource name is stripped.

The SGD2 parser in `gta5-shadermanager/src/awclib/parser.py` treats `proplst\0` as a section boundary and calls the following binding layouts the property list. Its encoded reference variants in `models.py` establish that the literal is also the preceding resource's name: the 28-byte record has a name offset at `+10` of 29, 37, or 45, exactly `28 + auxiliary_bytes + 1`. The one intervening byte is the string pool's leading NUL; the next eight bytes are `proplst\0`. Thus the literal belongs to that resource's reflection table. It is a convenient parser anchor, but does not establish that the following layouts are properties.

The SGD5 reader at `0x14247B6C0` stores the third table at runtime effect `+3224` (`0xC98`). Consumers directly use `table + 8`, the first resource, rather than looking it up by the `proplst` hash. The property accessors establish the auxiliary-record encoding:

```c
struct auxiliary_property {
    u32 value_kind  : 2;  // bits 0..1
    u32 name_hash30 : 30; // bits 2..31: lowercase JOAAT hash truncated to 30 bits
    union {
        u32 integer_value; // kind 0
        float float_value; // kind 1
        u32 string_offset; // kind 2: relative to this auxiliary record
        u32 raw;           // kind 3: unresolved
    } value;
};
```

`0x144F42240` follows resource `+14` and scans the count at resource `+3`, comparing `packed_key >> 2` with `requested_hash & 0x3FFFFFFF`. `0x144F42210` returns a kind-0 u32 value or the caller's default. `0x1405240C0` reads kind-1 floats; `0x140524090` reads kind-2 strings using `record + value`. Presence checks use the lookup function directly. The property table is queried by key rather than traversed as a list of resource bindings.

Confirmed consumer examples:

- `0x14249A420` passes `effect->property_table + 8` to `0x141F79E90`. That helper reads bucket classification, a mask defaulting to `(1 << bucket) | 0xFF00`, and a Boolean setting. Its fallback bucket key is the exact hash of `drawbucket`; the primary key's original spelling is unresolved.
- `0x1424948F0` filters effects using the same bucket property and a separate bitmask property before invoking its next processing step.
- `0x144F41E90` and `0x145269840` query a fixed array of 24 property keys. A value of one selects the corresponding factory/callback and contributes feature bits to the constructed runtime object. The individual feature names remain unresolved.
- `0x1452ED610` derives object/material flags from property values and property presence. Other readers use presence to select particular material entries or alter rendering behavior.

`effect_properties` is therefore a supported name for the third reflection field; `binding_list` describes the following layout array. The generic reflection format is still shared with shader/resource metadata, but this synthetic zero-size CBV record acts as an annotation/property container rather than establishing a GPU constant-buffer binding. Original property names and every downstream effect remain only partially recovered.

## Effect tables resolved from the executable

The old recursive continuation heuristic is replaced by the reader's explicit sequence:

| Section | Serialized record width | Supplied archive total |
| --- | ---: | ---: |
| Fixed effect prefix | 80 bytes | 932 |
| Rasterizer states | 16 bytes | 64 |
| Depth/stencil states | 24 bytes | 124 |
| Blend states | 68 bytes: 4-byte prefix plus eight 8-byte target descriptors | 398 |
| Sampler states | 36 bytes | 10,650 |
| Techniques | 4 bytes | 25,037 |
| Passes | 24 bytes | 28,798 |
| Pass lookup storage | Counted u32 words | 586,938 words |

Each variable section has a preceding u32 count. State roles are confirmed by their creation/expansion functions: `0x1411BCD00` (rasterizer), `0x1411BC920` (depth/stencil), `0x141819530` (blend), and `0x1412B96B0` (sampler). The sampler descriptor's packed filter fields and several reserved state bytes remain raw.

A technique's packed u32 has a 15-bit string-pool offset, 9-bit first-pass index, and 8-bit pass count. Every name offset and pass range was validated over the supplied archive.

Pass bytes `0..5` select VS/PS/GS/DS/HS/CS slots in the effect's arrays; zero means absent, and other values are one-based. Bytes `6..8` select rasterizer/depth-stencil/blend states when bits `0..2` of byte `10` are set. Bytes `18..21` select binding tables, with `0xFF` for absent tables. Other bytes are preserved without assigning unsupported meanings.

The u16 at pass `+16` points to per-pass resource-usage storage relative to the in-memory pass record. The file offset needs an additional four bytes because the loader omits the lookup array's u32 count when it copies the array into its effect buffer. At that target are four counted hash arrays in SRV, UAV, sampler, and CBV order. The supplied archive has 354,283 SRV, 12,297 UAV, zero sampler, and 81,510 CBV hash occurrences in these lists. When the effect has a nonzero texture-handle count, the next u32 is the handle binding control word. Its low byte selects a flattened root-constant dword (0xFF means absent); values below 0x100 also enable the CBV fallback. Other bits and unused lookup words are preserved.

The following u32 is a reflection-source selector: `0x80000000` reuses the archive's shared reflection, zero denotes no table, and other values introduce an inline reflection table. All 932 supplied effects reuse the shared table. Main and property (`proplst`) reflection follow as separate tables.

The effect binding-list header is a **24-bit entry count and 8-bit texture-handle count**, not two u16 fields. Inline layouts and 32-bit shared-table hash references retain their common binding-table model.

## Viewer and exports

The SGD5 Shader view retains GFX10.3 disassembly, experimental HLSL lifting, and SPIR-V output. Its Metadata view works independently of FSSM extraction and displays:

- DXBC input/output signature elements, when decoding succeeds.
- FSSM version, declared program range, stage-pointer field, embedded compiler digest comparison, and compute thread dimensions/user SGPR count.
- Resource families, exact type codes, compiled registers/spaces, matching root parameters and descriptor byte offsets, both hashes, and CBV sizes.
- Member types, arrays, packing offsets, and raw default-byte previews.
- Shared reflection records with matching shader hashes.
- Referencing effects, effect string pools, and inline/shared binding-table summaries with matching shader hashes and table indices.
- Matching techniques and passes, selected shaders/states, root owner, per-space binding selectors, verified descriptor/register joins, handle binding controls, resource-usage counts, and combined parameter keys.

The old SGD5 path fabricated HLSL declarations such as `Texture2D<float4> DensityMap : register(t19)` from a resource type and metadata slot. This path has been removed. Container metadata is displayed and exported as labeled comments, including confirmed root/register mappings while preserving uncertainty about texture element types and ISA SGPR assignments.

ASM and HLSL text exports include these comments. Metadata can also be exported as a standalone text file. SPIR-V binary generation is unchanged; no unconfirmed resource names or register mappings are introduced into it.

## FSSM program structure

All 26,503 supplied FSSM blobs have magic `FSSM`, version field `0x101`, and a u64 payload size equal to the file length minus `0x10`. Relative offsets use that `0x10` payload base.

The pointer table at file offsets `0x18..0x78` has one populated entry per blob; other entries contain `DEADBEEFDEADBEEF`. The populated entry points to the program descriptor at file offset `0xC0` (relative value `0xB0`). Its first two u64 values are program offset and program size. The program offset is also payload-relative; the size includes compiler no-ops and code-end allocation padding.

The pointer-table field varies with the compiled stage/variant: observed payload-relative fields include `0x18`, `0x20`, `0x28`, `0x30`, `0x48`, and `0x60`. Thus `payload+0x48` is one observed field, not a universal location.

Every supplied program begins with two 12-byte VOP3 `v_nop` instructions. Their ignored fields encode 128 bits: each no-op carries 16 bits in its first word, two data bytes in its source fields, and a 32-bit literal extension. The reconstructed value matches the DXBC `HASH` chunk's 16-byte compiler digest. This compiler hash is distinct from the SGD5 SGA hash and resource/member name hashes.

`BFA00003` is GFX10 `s_inst_prefetch 3`, rather than `s_setprio`. It occurs at program offset `0x18` in 25,873 shaders. In another 630 shaders, `s_swappc_b64 s[4:5], s[4:5]` precedes it, placing prefetch at `0x1C`. Searching for that byte pattern dropped both the no-ops and these real calls. FSSM extraction now follows the declared range and does not depend on this instruction's presence.

Disassembly preserves offsets from the program's beginning and recognizes the no-ops, prefetch, and scalar PC operations. An `s_endpgm` is an instruction, not a container size field: decoding continues after it until code-end padding or the declared range's end. Branch/call structure is still not fully reconstructed by the experimental HLSL/SPIR-V lifters.

The implemented structured FSSM reader supports the observed `0x101` format with one active program. Other versions or multiple active programs are reported as unsupported rather than selected heuristically.

Opcode references: LLVM's [GFX10 VOP1/VOP3 tables](https://github.com/llvm/llvm-project/blob/llvmorg-18.1.6/llvm/lib/Target/AMDGPU/VOP1Instructions.td) and [scalar instruction tables](https://github.com/llvm/llvm-project/blob/llvmorg-18.1.6/llvm/lib/Target/AMDGPU/SOPInstructions.td).

## Improving decompilation later

A useful next step is to connect the compiled resource metadata and effect layout selection to actual descriptor accesses in the ISA. That requires following descriptor table bases, register spaces/ranges, SGPR user data, and bindless indirection rather than equating a name hash or SGD5 index with a machine register.

Once that chain is established, resource types can annotate descriptor loads, member offsets can identify constant-buffer accesses, signatures can describe stage I/O, and effect/variant ownership can organize related shaders. The current viewer exposes the evidence for that work without claiming that the chain is solved.
