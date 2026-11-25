# Native compiler metadata for SGD5 exploration

SGD5's deflate-packed shader archive contains a DXBC container. A DXIL part
indexes native programs through XDAT at part-data +0x18. Each XDAT entry is
`u32 kind, offset, size`; kind 0x0D contains XBCH version 3 with `u64 packed_size`,
`u64 unpacked_size`, then a zlib stream at XBCH +0x18. Offset/size bounds and
the decompressed size are validated. This replaces probing fixed zlib offsets.

The supplied archive has 26,503 shader containers and 26,698 native programs.
195 VS containers have both VS and ES variants; XDAT order is preserved rather
than assumed to put VS first. The explorer selects a matching stage when one
exists, and makes every variant available for inspection/export. VS libraries
also contain 9,282 mesh programs. The compiled hardware stage is displayed
separately from the SGD5 library stage.

FSSM version 0x101 starts with magic, version, and u64 payload size. All embedded
pointers are relative to payload base file +0x10. The payload has six graphics
slots, with pointer fields at +0x08/+0x10/+0x18/+0x20/+0x28/+0x30 for
LS/HS/ES/GS/VS/PS; mesh and compute use +0x48 and +0x60. Empty pointers use
`0xDEADBEEFDEADBEEF`. Other slot positions remain unclassified. The current
corpus contains one active stage in each FSSM blob; multiple XDAT entries
account for its native variants.

## Active stage descriptor

| Offset | Field |
| --- | --- |
| +0x00 | u64 program offset |
| +0x08 | u64 program byte size |
| +0x14 | u32 flags: bit 5 wave32, otherwise wave64; retain other bits |
| +0x28 | u64 root-layout table offset |
| +0x30 | u64 used-resource table offset |
| +0x38 | u32 root-layout count |
| +0x3C | u32 used-resource count |
| +0x4C | u32 LDS byte size |
| +0x50 | u32 register word: VGPR count bits 0..9, SGPR count bits 20..27 |

Wave flags are read relative to the descriptor, not absolute file +0xD4.
The latter happened to work because all supplied descriptors begin at +0xC0.
Remaining descriptor fields are not assigned speculative names.

```c
struct native_root_layout_entry { // 20 bytes
    u32 root_type;
    u32 table_offset_dwords;
    u32 user_sgpr_count;
    u32 base_register;
    u8 first_user_data_dword;
    u8 root_slot;
    u16 reserved;
};

struct native_resource_usage_entry { // 20 bytes
    u8 resource_kind; // SRV=0, sampler=1, CBV=2, UAV=3
    u8 root_type;
    u16 reserved;
    u32 table_offset_dwords;
    u32 lower_bound_register;
    u32 stride;
    u8 descriptor_dword_count;
    u8 first_user_data_dword;
    u8 root_slot;
    u8 reserved_tail;
};
```

FSSM's resource-kind order differs from SGD5 reflection's SRV/UAV/sampler/CBV
category order. Root types are constants 0, direct descriptor 1, table 3,
immediate sampler 4, resource heap 5, sampler heap 6. Unknown enum values and
reserved fields are retained. Sampler slots and heap slots are separate from
ordinary `PRIV` root-parameter indices; ambiguous sampler joins stay unresolved.

A table usage entry joins its root slot and `table_offset_dwords / 8` to a
`PRIV` descriptor range, checking both category and register. That range supplies
the original register space, which then joins to SGD5 reflection/hash identity.
Descriptor placement is 32 bytes even when a shader consumes four dwords.
The compiler's stride is additional buffer-layout context, not a replacement
for constant-member byte offsets or texture-handle indices.

The inspector shows the compiler's user-data extent and corresponding SGPRs.
VS/PS/CS use the user-data indices directly; LS/HS/ES/GS/MS add eight system
SGPRs. For example, cable PS user-data 8 is s8, while cable MS user-data 8
is s16. A compute user-data index >=16 denotes argument-memory context rather
than an inline SGPR assignment; no such compute layout occurs in this corpus.
These are entry ABI annotations. Register copies and dynamic descriptor uses
still require ISA data-flow analysis.

## Stage extensions

Offsets here begin at the end of the 0x54-byte descriptor.

- VS inputs: 32 five-byte records at +0x24, count +0xC4; usage, signature row,
  channel mask and fetch VGPR. Outputs: 32 six-byte records at +0xC8, count
  +0x188; signature row, export parameter and fp16/packed flags.
- PS inputs: 32 eight-byte records at +0x24, count +0x124; interpolation
  attributes, signature rows, masks, second packed row and flat/linear/fp16
  flags. SPI_PS_INPUT_ADDR is +0x134, DB_SHADER_CONTROL +0x138.
- PS export patch width is +0x148. Eight 404-byte slots start at +0x14C,
  each holding a code offset and ten format alternatives, each occupying ten
  dwords. Only the declared width is meaningful. Patch extents are validated
  against the program. The parser retains all alternatives and displays the
  full 32_ABGR sequence without modifying the original code.
- CS COMPUTE_PGM_RSRC2 is +0x24 and u32 group dimensions are +0x28/+0x2C/+0x30.
  The explorer decodes enabled group-ID SGPRs, thread-ID VGPR count and the LDS
  allocation from RSRC2. Descriptor-relative offsets remain +0x78 and
  +0x7C/+0x80/+0x84, respectively.
- MS uses the VS output table, with parameter bit 7 marking per-primitive
  outputs. Eight register words at +0x18C are retained. At +0x448 are u16 group
  lanes, maximum vertices, maximum primitives, u8 user-SGPR count and topology.
- Merged-stage extension layouts remain unparsed; common descriptor and root
  metadata remain available.

## Evidence and checks

The interpretations were cross-checked with VIctory's format notes and decoded
independently from the supplied archive. The Rust regression verifies all
26,698 native programs, 5,214,262 layout records, 445,625 usage records, and
385,337 table-resource joins against shader reflection and `PRIV`. All 2,689
compute group dimensions match independent `PSV0` fields. Both native variants
also match the DXBC compiler fingerprint.

Effect state improvements were additionally verified in the executable:
pass state consumption `0x144DA8830`, rasterizer expansion `0x1411BCD00`,
depth/stencil expansion `0x1411BC920`, and sampler creation `0x1412B96B0`.
Pass index high bits, rasterizer multisampling/flags and depth bounds are zero
in this archive; executable consumption establishes their meaning. Active
stencil references include 3, 131, 4, 19 and 27. Sampler filter encodings
observed are 0x00, 0x14, 0x15 and 0x55; raw packed bits stay exposed.

Run the opt-in native regression with `PERCY_SGD5_SAMPLE` set to the archive:

```powershell
cargo test archive_native_variants_metadata_and_priv_joins -- --ignored
cargo test real_archive_shader_views_keep_pass_parameter_context -- --ignored
```

These checks validate metadata and explorer navigation. They do not establish
shader translation equivalence or complete ISA semantics.
