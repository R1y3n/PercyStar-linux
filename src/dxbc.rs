use flate2::read::ZlibDecoder;
#[path = "dxbc/compact_root.rs"]
mod compact_root;
pub use compact_root::{
    extract_compact_root_signature, CompactRootSignature, DescriptorKind, RootBindingLocation,
    RootParameter,
};
#[path = "dxbc/fssm.rs"]
mod fssm;
pub use fssm::{format_native_metadata, format_user_data, FssmNativeMetadata, NativeStage};
#[cfg(test)]
use fssm::{NativeRootType, NativeStageIo};
#[cfg(windows)]
use std::ffi::c_void;
use std::fmt::Write as _;
use std::io::Read;
#[cfg(windows)]
use std::sync::OnceLock;

const DXBC_MAGIC: &[u8; 4] = b"DXBC";
const FSSM_MAGIC: &[u8; 4] = b"FSSM";
const FSSM_PAYLOAD_BASE: usize = 0x10;
const FSSM_EMPTY_STAGE: u64 = 0xDEAD_BEEF_DEAD_BEEF;
const MAX_FSSM_BYTES: usize = 64 << 20;

#[cfg(windows)]
const CLSID_DXC_COMPILER: Guid = Guid::from_values(
    0x73e22d93,
    0xe6ce,
    0x47f3,
    [0xb5, 0xbf, 0xf0, 0x66, 0x4f, 0x39, 0xc1, 0xb0],
);

#[cfg(windows)]
const IID_IDXC_COMPILER3: Guid = Guid::from_values(
    0x228b4687,
    0x5a6a,
    0x4730,
    [0x90, 0x0c, 0x97, 0x02, 0xb2, 0x20, 0x3f, 0x54],
);

#[cfg(windows)]
const IID_IDXC_RESULT: Guid = Guid::from_values(
    0x58346cda,
    0xdde7,
    0x4497,
    [0x94, 0x61, 0x6f, 0x87, 0xaf, 0x5e, 0x06, 0x59],
);

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
impl Guid {
    const fn from_values(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }
}

#[cfg(windows)]
#[repr(C)]
struct DxcBuffer {
    ptr: *const c_void,
    size: usize,
    encoding: u32,
}

pub fn disassemble(data: &[u8]) -> String {
    if let Ok(programs) = extract_fssm_programs(data) {
        return programs
            .iter()
            .enumerate()
            .map(|(index, program)| {
                format_fssm_disasm(program, &format!("DXBC / FSSM variant {index}"))
            })
            .collect::<Vec<_>>()
            .join("\n");
    }

    match disassemble_with_dxc(data) {
        Ok(text) => text,
        Err(error) => format!("{error}\n\n{}", inspect(data)),
    }
}

#[cfg(test)]
pub fn disassemble_with_source(data: &[u8], source: &str) -> String {
    match extract_fssm(data) {
        Ok(fssm) => format_fssm_disasm(&fssm, source),
        Err(_) => disassemble(data),
    }
}

#[derive(Clone, Debug)]
pub struct FssmShader {
    pub data: Vec<u8>,
    pub version: u32,
    pub payload_size: u64,
    pub stage_pointer_offset: usize,
    pub stage_descriptor_offset: usize,
    pub code_offset: usize,
    pub code_size: usize,
    pub compiler_hash: Option<[u8; 16]>,
    pub dxbc_hash_match: Option<bool>,
}

#[derive(Clone, Copy, Debug)]
pub struct FssmComputeMetadata {
    pub rsrc2: u32,
    pub user_sgpr_count: u8,
    pub num_threads: [u16; 3],
}

impl FssmComputeMetadata {
    pub fn group_id_sgprs(&self) -> [Option<u8>; 3] {
        let mut next = self.user_sgpr_count;
        std::array::from_fn(|axis| {
            if self.rsrc2 & (1 << (7 + axis)) == 0 {
                return None;
            }
            let result = next;
            next += 1;
            Some(result)
        })
    }
    pub fn thread_id_vgpr_count(&self) -> u8 {
        ((self.rsrc2 >> 11) & 3) as u8 + 1
    }
    pub fn lds_allocation_bytes(&self) -> u32 {
        ((self.rsrc2 >> 15) & 0x1ff) * 512
    }
}

impl FssmShader {
    pub fn compute_metadata(&self) -> Option<FssmComputeMetadata> {
        if self.stage_pointer_offset != 0x70 {
            return None;
        }
        // This FSSM layout has the Scarlett compute state at descriptor+0x70.
        // The shorter retail layout used by some tools puts it at +0x30.
        let descriptor_end = self.stage_descriptor_offset.checked_add(0x88)?;
        if descriptor_end > self.code_offset {
            return None;
        }
        let rsrc2 = read_u32(&self.data, self.stage_descriptor_offset + 0x78).ok()?;
        let mut num_threads = [0u16; 3];
        for (index, value) in num_threads.iter_mut().enumerate() {
            let count =
                read_u32(&self.data, self.stage_descriptor_offset + 0x7c + index * 4).ok()?;
            if !(1..=1024).contains(&count) {
                return None;
            }
            *value = count as u16;
        }
        if num_threads
            .iter()
            .map(|value| u32::from(*value))
            .product::<u32>()
            > 1024
        {
            return None;
        }
        Some(FssmComputeMetadata {
            rsrc2,
            user_sgpr_count: ((rsrc2 >> 1) & 31) as u8,
            num_threads,
        })
    }
    pub fn compiled_wave_size(&self) -> Option<u8> {
        // The flags are in metadata, so never interpret program bytes at this offset.
        let offset = self.stage_descriptor_offset.checked_add(0x14)?;
        if offset.checked_add(4)? > self.code_offset {
            return None;
        }
        let flags = read_u32(&self.data, offset).ok()?;
        Some(if flags & 0x20 != 0 { 32 } else { 64 })
    }

    pub fn code(&self) -> &[u8] {
        self.code_offset
            .checked_add(self.code_size)
            .and_then(|end| self.data.get(self.code_offset..end))
            .unwrap_or_default()
    }
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("u32 offset overflow")?;
    let bytes = data.get(offset..end).ok_or("truncated u32")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64, String> {
    let end = offset.checked_add(8).ok_or("u64 offset overflow")?;
    let bytes = data.get(offset..end).ok_or("truncated u64")?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn payload_offset(relative: u64) -> Result<usize, String> {
    usize::try_from(relative)
        .ok()
        .and_then(|offset| offset.checked_add(FSSM_PAYLOAD_BASE))
        .ok_or_else(|| "FSSM relative offset overflow".into())
}

fn compiler_hash(program: &[u8]) -> Option<[u8; 16]> {
    let mut hash = [0; 16];
    for index in 0..2 {
        let offset = index * 12;
        let word0 = read_u32(program, offset).ok()?;
        let word1 = read_u32(program, offset + 4).ok()?;
        let word2 = read_u32(program, offset + 8).ok()?;
        if word0 & 0xFFFF_0000 != 0xD580_0000 || word1 & 0xFFFE_0100 != 0x03FE_0100 {
            return None;
        }
        let packed = (word0 & 0xffff) | ((word1 & 0xff) << 16) | (((word1 >> 9) & 0xff) << 24);
        hash[index * 8..index * 8 + 4].copy_from_slice(&packed.to_le_bytes());
        hash[index * 8 + 4..index * 8 + 8].copy_from_slice(&word2.to_le_bytes());
    }
    Some(hash)
}

pub fn parse_fssm(data: Vec<u8>) -> Result<FssmShader, String> {
    if data.get(..4) != Some(FSSM_MAGIC) {
        return Err("not an FSSM container".into());
    }
    if data.len() > MAX_FSSM_BYTES {
        return Err("FSSM exceeds 64 MiB".into());
    }
    let version = read_u32(&data, 4)?;
    if version != 0x101 {
        return Err(format!("unsupported FSSM version 0x{version:x}"));
    }
    let payload_size = read_u64(&data, 8)?;
    if payload_offset(payload_size)? != data.len() {
        return Err("FSSM payload size mismatch".into());
    }
    data.get(..0x78)
        .ok_or("truncated FSSM stage-pointer table")?;
    let mut active = None;
    for offset in (0x18..0x78).step_by(8) {
        let relative = read_u64(&data, offset)?;
        if relative == FSSM_EMPTY_STAGE || relative == 0 {
            continue;
        }
        if active.is_some() {
            return Err("multiple active FSSM programs are not supported".into());
        }
        active = Some((offset, payload_offset(relative)?));
    }
    let (stage_pointer_offset, stage_descriptor_offset) =
        active.ok_or("FSSM has no active program")?;
    if stage_descriptor_offset < 0x78 {
        return Err("FSSM stage descriptor overlaps its pointer table".into());
    }
    let code_offset = payload_offset(read_u64(&data, stage_descriptor_offset)?)?;
    let size_offset = stage_descriptor_offset
        .checked_add(8)
        .ok_or("stage descriptor overflow")?;
    let code_size =
        usize::try_from(read_u64(&data, size_offset)?).map_err(|_| "FSSM program size overflow")?;
    let code_end = code_offset
        .checked_add(code_size)
        .ok_or("FSSM program extent overflow")?;
    if code_size == 0 || code_size % 4 != 0 || code_offset < 0x78 || code_offset % 4 != 0 {
        return Err("invalid FSSM program alignment or size".into());
    }
    let descriptor_end = stage_descriptor_offset
        .checked_add(16)
        .ok_or("stage descriptor overflow")?;
    if code_offset < descriptor_end && code_end > stage_descriptor_offset {
        return Err("FSSM program overlaps its descriptor".into());
    }
    let program = data
        .get(code_offset..code_end)
        .ok_or("FSSM program exceeds its payload")?;
    let compiler_hash = compiler_hash(program);
    Ok(FssmShader {
        data,
        version,
        payload_size,
        stage_pointer_offset,
        stage_descriptor_offset,
        code_offset,
        code_size,
        compiler_hash,
        dxbc_hash_match: None,
    })
}

/// Compatibility helper for callers that explicitly want the first variant.
pub fn extract_fssm(data: &[u8]) -> Result<FssmShader, String> {
    extract_fssm_programs(data)?
        .into_iter()
        .next()
        .ok_or_else(|| "no native FSSM programs".into())
}

/// Enumerate native XDAT entries in order, including alternate VS/ES programs.
pub fn extract_fssm_programs(data: &[u8]) -> Result<Vec<FssmShader>, String> {
    if data.get(..4) == Some(FSSM_MAGIC) {
        return Ok(vec![parse_fssm(data.to_vec())?]);
    }
    let payload = find_dxil_payload(data)?;
    if payload.get(0x18..0x1c) != Some(b"XDAT") {
        return Err("DXIL payload has no XDAT native index".into());
    }
    let count = read_u32(payload, 0x1c)? as usize;
    let end = count
        .checked_mul(12)
        .and_then(|n| n.checked_add(0x24))
        .ok_or("XDAT table size overflow")?;
    payload.get(0x24..end).ok_or("truncated XDAT table")?;
    let mut programs = Vec::new();
    for index in 0..count {
        let entry = 0x24 + index * 12;
        if read_u32(payload, entry)? != 0x0d {
            continue;
        }
        let offset = read_u32(payload, entry + 4)? as usize;
        let size = read_u32(payload, entry + 8)? as usize;
        let blob = payload
            .get(offset..offset.checked_add(size).ok_or("XDAT blob overflow")?)
            .ok_or("XDAT blob outside DXIL payload")?;
        if blob.get(..4) != Some(b"XBCH") || read_u32(blob, 4)? != 3 {
            return Err("unsupported XBCH native header".into());
        }
        let packed_size =
            usize::try_from(read_u64(blob, 8)?).map_err(|_| "XBCH packed size overflow")?;
        let unpacked_size =
            usize::try_from(read_u64(blob, 16)?).map_err(|_| "XBCH unpacked size overflow")?;
        if unpacked_size > MAX_FSSM_BYTES {
            return Err("XBCH exceeds 64 MiB".into());
        }
        let packed_end = packed_size
            .checked_add(0x18)
            .ok_or("XBCH stream extent overflow")?;
        let stream = blob.get(0x18..packed_end).ok_or("truncated XBCH stream")?;
        let mut decompressed = Vec::new();
        ZlibDecoder::new(stream)
            .take(unpacked_size as u64 + 1)
            .read_to_end(&mut decompressed)
            .map_err(|error| format!("XBCH inflate failed: {error}"))?;
        if decompressed.len() != unpacked_size {
            return Err("XBCH unpacked size mismatch".into());
        }
        let mut shader = parse_fssm(decompressed)?;
        if let (Some(hash), Ok(Some(chunk))) =
            (shader.compiler_hash, find_dxbc_chunk(data, b"HASH"))
        {
            if let Some(digest) = chunk.get(4..20) {
                shader.dxbc_hash_match = Some(digest == hash);
            }
        }
        programs.push(shader);
    }
    if programs.is_empty() {
        return Err("XDAT has no native FSSM entries".into());
    }
    Ok(programs)
}

#[cfg(windows)]
fn disassemble_with_dxc(data: &[u8]) -> Result<String, String> {
    if data.len() < 32 || &data[..4] != DXBC_MAGIC {
        return Err("not a DXBC container".into());
    }

    let create_instance = cached_dxc_create_instance()?;
    let mut compiler = std::ptr::null_mut::<c_void>();
    unsafe {
        let status = create_instance(
            &CLSID_DXC_COMPILER as *const Guid as *const c_void,
            &IID_IDXC_COMPILER3 as *const Guid as *const c_void,
            &mut compiler,
        );
        if status != 0 {
            return Err(format!("DxcCreateInstance failed: 0x{status:08x}"));
        }
    }

    let input = DxcBuffer {
        ptr: data.as_ptr() as *const c_void,
        size: data.len(),
        encoding: 0,
    };
    let mut result = std::ptr::null_mut::<c_void>();
    unsafe {
        let disassemble = com_call::<
            unsafe extern "system" fn(
                *mut c_void,
                *const DxcBuffer,
                *const Guid,
                *mut *mut c_void,
            ) -> i32,
        >(compiler, 4);
        let status = disassemble(compiler, &input, &IID_IDXC_RESULT, &mut result);
        release(compiler);
        if status != 0 {
            return Err(format!("IDxcCompiler3::Disassemble failed: 0x{status:08x}"));
        }
    }

    let mut operation_status = 0i32;
    let mut blob = std::ptr::null_mut::<c_void>();
    unsafe {
        com_call::<unsafe extern "system" fn(*mut c_void, *mut i32) -> i32>(result, 3)(
            result,
            &mut operation_status,
        );
        com_call::<unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32>(result, 4)(
            result, &mut blob,
        );
        release(result);
    }

    if operation_status != 0 {
        if !blob.is_null() {
            unsafe { release(blob) };
        }
        return Err(format!("DXC disassembly failed: 0x{operation_status:08x}"));
    }
    if blob.is_null() {
        return Err("DXC returned no disassembly blob".into());
    }

    unsafe {
        let get_pointer =
            com_call::<unsafe extern "system" fn(*mut c_void) -> *mut c_void>(blob, 3);
        let get_size = com_call::<unsafe extern "system" fn(*mut c_void) -> usize>(blob, 4);
        let pointer = get_pointer(blob);
        let size = get_size(blob);
        let bytes = std::slice::from_raw_parts(pointer as *const u8, size);
        let text = String::from_utf8_lossy(bytes).into_owned();
        release(blob);
        Ok(text)
    }
}

#[cfg(not(windows))]
fn disassemble_with_dxc(_data: &[u8]) -> Result<String, String> {
    Err("DXC disassembly is only available on Windows with dxcompiler.dll".into())
}

#[cfg(windows)]
unsafe fn com_call<F>(instance: *mut c_void, slot: usize) -> F {
    debug_assert!(!instance.is_null());
    let vtable = *(instance as *const *const *mut c_void);
    let function = *vtable.add(slot);
    std::mem::transmute_copy(&function)
}

#[cfg(windows)]
unsafe fn release(instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let release = com_call::<unsafe extern "system" fn(*mut c_void) -> u32>(instance, 2);
    release(instance);
}

#[cfg(windows)]
type DxcCreateInstanceFn =
    unsafe extern "system" fn(*const c_void, *const c_void, *mut *mut c_void) -> i32;

#[cfg(windows)]
fn cached_dxc_create_instance() -> Result<DxcCreateInstanceFn, String> {
    static DXC_CREATE: OnceLock<Result<DxcCreateInstanceFn, String>> = OnceLock::new();
    DXC_CREATE
        .get_or_init(|| {
            let library = load_dxc_library()?;
            get_dxc_create_instance(library)
        })
        .clone()
}

#[cfg(windows)]
fn load_dxc_library() -> Result<*mut c_void, String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    }
    // a bare name would also search the working directory and path where a dump folder can plant a dll
    const SEARCH_DLL_LOAD_DIR: u32 = 0x100;
    const SEARCH_APPLICATION_DIR: u32 = 0x200;
    const SEARCH_SYSTEM32: u32 = 0x800;
    let load = |name: &std::ffi::OsStr, flags: u32| {
        let wide: Vec<u16> = name.encode_wide().chain(std::iter::once(0)).collect();
        unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), flags) }
    };
    // explicit full path or the folder of the executable and system32
    let explicit = std::env::var_os("DXCOMPILER_DLL")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file());
    let library = match explicit {
        Some(path) => load(path.as_os_str(), SEARCH_DLL_LOAD_DIR | SEARCH_SYSTEM32),
        None => load(
            std::ffi::OsStr::new("dxcompiler.dll"),
            SEARCH_APPLICATION_DIR | SEARCH_SYSTEM32,
        ),
    };
    if library.is_null() {
        return Err("dxcompiler.dll was not found next to PercyStar.exe (or set DXCOMPILER_DLL to its full path)".into());
    }
    Ok(library)
}

#[cfg(windows)]
fn get_dxc_create_instance(
    library: *mut c_void,
) -> Result<unsafe extern "system" fn(*const c_void, *const c_void, *mut *mut c_void) -> i32, String>
{
    #[link(name = "kernel32")]
    extern "system" {
        fn GetProcAddress(library: isize, name: *const u8) -> *const c_void;
    }

    let name = c"DxcCreateInstance";
    let address = unsafe { GetProcAddress(library as isize, name.as_ptr() as *const u8) };
    if address.is_null() {
        return Err("dxcompiler.dll does not export DxcCreateInstance".into());
    }
    Ok(unsafe { std::mem::transmute(address) })
}

fn find_dxbc_chunk<'a>(data: &'a [u8], wanted: &[u8; 4]) -> Result<Option<&'a [u8]>, String> {
    if data.get(..4) != Some(DXBC_MAGIC) {
        return Err("not a DXBC container".into());
    }
    let size = read_u32(data, 0x18)? as usize;
    let data = data
        .get(..size)
        .filter(|data| data.len() >= 0x20)
        .ok_or("invalid DXBC extent")?;
    let count = read_u32(data, 0x1c)? as usize;
    let table_end = count
        .checked_mul(4)
        .and_then(|size| size.checked_add(0x20))
        .filter(|end| *end <= data.len())
        .ok_or("invalid DXBC chunk count")?;
    for table_offset in (0x20..table_end).step_by(4) {
        let chunk_offset = read_u32(data, table_offset)? as usize;
        let header_end = chunk_offset
            .checked_add(8)
            .ok_or("DXBC chunk offset overflow")?;
        let header = data
            .get(chunk_offset..header_end)
            .ok_or("truncated DXBC chunk header")?;
        if &header[..4] != wanted {
            continue;
        }
        let size = read_u32(header, 4)? as usize;
        let end = header_end
            .checked_add(size)
            .ok_or("DXBC chunk size overflow")?;
        return data
            .get(header_end..end)
            .map(Some)
            .ok_or_else(|| "truncated DXBC chunk".into());
    }
    Ok(None)
}

fn find_dxil_payload(data: &[u8]) -> Result<&[u8], String> {
    find_dxbc_chunk(data, b"DXIL")?.ok_or_else(|| "DXBC container has no valid DXIL chunk".into())
}

pub fn format_fssm_metadata(fssm: &FssmShader) -> String {
    let mut text = format!("FSSM program metadata\n  version_field=0x{:08x} payload_bytes={}\n  stage_pointer_field=payload+0x{:x} descriptor_file_offset=0x{:x}\n  declared_program_file_offset=0x{:x} program_bytes={} (includes compiler no-ops and allocation padding)\n",
        fssm.version, fssm.payload_size, fssm.stage_pointer_offset.saturating_sub(FSSM_PAYLOAD_BASE),
        fssm.stage_descriptor_offset, fssm.code_offset, fssm.code_size);
    if let Some(wave_size) = fssm.compiled_wave_size() {
        let _ = writeln!(
            text,
            "  compiled_wave_size={wave_size} (stage descriptor+0x14)"
        );
    }
    if let Some(compute) = fssm.compute_metadata() {
        let _ = writeln!(
            text,
            "  numthreads={:?} compute_rsrc2=0x{:08x} user_sgprs={}",
            compute.num_threads, compute.rsrc2, compute.user_sgpr_count
        );
    }
    if let Some(hash) = fssm.compiler_hash {
        let hash = hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let matches = match fssm.dxbc_hash_match {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unavailable",
        };
        let _ = writeln!(
            text,
            "  compiler_hash_embedded_in_noops={hash} DXBC_HASH_match={matches}"
        );
    }
    match fssm.native_metadata() {
        Ok(metadata) => text.push_str(&format_native_metadata(&metadata)),
        Err(error) => {
            let _ = writeln!(text, "  Native metadata unavailable: {error}");
        }
    }
    text
}

fn format_fssm_disasm(fssm: &FssmShader, source: &str) -> String {
    let instructions = crate::rdna2::decode_gfx103(fssm.code(), usize::MAX);
    let mut output = String::with_capacity(instructions.len() * 48 + 512);
    let _ = writeln!(
        output,
        "; =============================================================================\n\
         ; PercyStar AMD RDNA2 (GFX10.3) Disassembly\n\
         ; Source: {source}\n\
         ; FSSM: {} bytes | Declared program: 0x{:X} + {} bytes | {} insns\n\
         ; =============================================================================\n",
        fssm.data.len(),
        fssm.code_offset,
        fssm.code_size,
        instructions.len()
    );
    for line in format_fssm_metadata(fssm).lines() {
        let _ = writeln!(output, "; {line}");
    }

    for instruction in &instructions {
        let mut words = String::new();
        for word in 0..instruction.nwords {
            let _ = write!(words, "{:08x} ", instruction.words[word]);
        }
        let _ = writeln!(
            output,
            "  +{:04x}: {:<28} {:<26} {}",
            instruction.offset, words, instruction.mnemonic, instruction.operands
        );
    }
    output
}

pub fn inspect(data: &[u8]) -> String {
    if data.len() < 0x24 || &data[..4] != DXBC_MAGIC {
        return "Not a DXBC container".into();
    }

    let read_u32 = |offset: usize| {
        u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    };
    let chunk_count = read_u32(0x1c) as usize;
    let mut output = String::new();
    let _ = writeln!(
        output,
        "DXBC container: {} bytes, {} chunks",
        data.len(),
        chunk_count
    );
    for chunk_index in 0..chunk_count {
        let table_offset = 0x20 + chunk_index * 4;
        if table_offset + 4 > data.len() {
            let _ = writeln!(output, "  truncated chunk table at 0x{table_offset:x}");
            break;
        }
        let offset = read_u32(table_offset) as usize;
        if offset + 8 > data.len() {
            let _ = writeln!(output, "  invalid chunk offset 0x{offset:x}");
            continue;
        }
        let tag = &data[offset..offset + 4];
        let size = read_u32(offset + 4) as usize;
        let end = offset + 8 + size;
        let state = if end <= data.len() { "ok" } else { "truncated" };
        let _ = writeln!(
            output,
            "  {} at 0x{:08x}, {} bytes ({state})",
            String::from_utf8_lossy(tag),
            offset,
            size
        );
        if tag == b"DXIL" && end <= data.len() {
            let content = &data[offset + 8..end];
            if content.windows(4).any(|window| window == b"NOBC") {
                let _ = writeln!(
                    output,
                    "    NOBC wrapped shader: DXIL payload uses NOBC (no LLVM bitcode)"
                );
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write as _;

    fn synthetic_fssm(program: &[u32]) -> Vec<u8> {
        let mut data = vec![0; 0xd0];
        data[..4].copy_from_slice(FSSM_MAGIC);
        data[4..8].copy_from_slice(&0x101u32.to_le_bytes());
        for offset in (0x18..0x78).step_by(8) {
            data[offset..offset + 8].copy_from_slice(&FSSM_EMPTY_STAGE.to_le_bytes());
        }
        data[0x58..0x60].copy_from_slice(&0xb0u64.to_le_bytes());
        data[0xc0..0xc8].copy_from_slice(&0xc0u64.to_le_bytes());
        data[0xc8..0xd0].copy_from_slice(&(program.len() as u64 * 4).to_le_bytes());
        for word in program {
            data.extend_from_slice(&word.to_le_bytes());
        }
        let size = data.len() as u64 - 16;
        data[8..16].copy_from_slice(&size.to_le_bytes());
        data
    }

    fn fingerprint_program() -> Vec<u32> {
        vec![
            0xd5802957, 0x03ffe3e0, 0x666b5907, 0xd580ccb6, 0x03fecf20, 0xdfe4d789, 0xbe842104,
            0xbfa00003, 0xbf810000, 0xbf9f0000,
        ]
    }

    fn indexed_dxbc(blobs: &[Vec<u8>]) -> Vec<u8> {
        let mut payload = vec![0; 0x24 + blobs.len() * 12];
        payload[0x18..0x1c].copy_from_slice(b"XDAT");
        payload[0x1c..0x20].copy_from_slice(&(blobs.len() as u32).to_le_bytes());
        for (index, blob) in blobs.iter().enumerate() {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(blob).unwrap();
            let packed = encoder.finish().unwrap();
            let offset = payload.len() as u32;
            let entry = 0x24 + index * 12;
            payload[entry..entry + 4].copy_from_slice(&13u32.to_le_bytes());
            payload[entry + 4..entry + 8].copy_from_slice(&offset.to_le_bytes());
            payload[entry + 8..entry + 12]
                .copy_from_slice(&(packed.len() as u32 + 24).to_le_bytes());
            payload.extend_from_slice(b"XBCH");
            payload.extend_from_slice(&3u32.to_le_bytes());
            payload.extend_from_slice(&(packed.len() as u64).to_le_bytes());
            payload.extend_from_slice(&(blob.len() as u64).to_le_bytes());
            payload.extend_from_slice(&packed);
        }
        let mut data = vec![0; 0x24];
        data[..4].copy_from_slice(b"DXBC");
        data[0x1c..0x20].copy_from_slice(&1u32.to_le_bytes());
        data[0x20..0x24].copy_from_slice(&0x24u32.to_le_bytes());
        data.extend_from_slice(b"DXIL");
        data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend(payload);
        let size = data.len() as u32;
        data[0x18..0x1c].copy_from_slice(&size.to_le_bytes());
        data
    }

    #[test]
    fn indexed_extraction_keeps_variants_and_rejects_corrupt_declared_streams() {
        let first = synthetic_fssm(&[0xbf810000]);
        let mut second = synthetic_fssm(&[0xbf800001, 0xbf810000]);
        second[0x58..0x60].copy_from_slice(&FSSM_EMPTY_STAGE.to_le_bytes());
        second[0x28..0x30].copy_from_slice(&0xb0u64.to_le_bytes());
        let data = indexed_dxbc(&[first, second]);
        let programs = extract_fssm_programs(&data).unwrap();
        assert_eq!(programs.len(), 2);
        assert_eq!(programs[1].hardware_stage(), NativeStage::Es);
        assert_eq!(programs[1].code_size, 8);
        assert!(disassemble(&data).contains("variant 1"));
        let mut truncated = data.clone();
        // First entry's native blob points outside the declared DXIL part.
        truncated[0x2c + 0x28..0x2c + 0x2c].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(extract_fssm_programs(&truncated).is_err());
        let mut invalid_size = data;
        let blob_at = 0x2c + 0x24 + 24;
        invalid_size[blob_at + 16..blob_at + 24].copy_from_slice(&1u64.to_le_bytes());
        assert!(extract_fssm_programs(&invalid_size).is_err());
    }

    #[test]
    fn inspects_dxbc_chunk_table() {
        let mut data = vec![0u8; 0x24];
        data[..4].copy_from_slice(DXBC_MAGIC);
        data[0x1c..0x20].copy_from_slice(&1u32.to_le_bytes());
        data[0x20..0x24].copy_from_slice(&0x24u32.to_le_bytes());
        data.extend_from_slice(b"TEST");
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&[1, 2, 3, 4]);

        let text = inspect(&data);
        assert!(text.contains("1 chunks"));
        assert!(text.contains("TEST"));
    }

    #[test]
    fn extracts_and_disassembles_fssm_from_dxil() {
        let fssm = synthetic_fssm(&fingerprint_program());
        let hash = parse_fssm(fssm.clone()).unwrap().compiler_hash.unwrap();

        let mut payload = vec![0u8; 0x40];
        payload[0x18..0x1c].copy_from_slice(b"XDAT");
        payload[0x1c..0x20].copy_from_slice(&1u32.to_le_bytes());
        payload[0x24..0x28].copy_from_slice(&13u32.to_le_bytes());
        payload[0x28..0x2c].copy_from_slice(&0x40u32.to_le_bytes());
        let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
        compressor.write_all(&fssm).unwrap();
        let compressed = compressor.finish().unwrap();
        payload[0x2c..0x30].copy_from_slice(&(compressed.len() as u32 + 24).to_le_bytes());
        payload.extend_from_slice(b"XBCH");
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
        payload.extend_from_slice(&(fssm.len() as u64).to_le_bytes());
        payload.extend_from_slice(&compressed);

        let mut data = vec![0u8; 0x28];
        data[..4].copy_from_slice(DXBC_MAGIC);
        data[0x1c..0x20].copy_from_slice(&2u32.to_le_bytes());
        data[0x20..0x24].copy_from_slice(&0x28u32.to_le_bytes());
        data.extend_from_slice(b"DXIL");
        data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&payload);
        let hash_offset = data.len() as u32;
        data[0x24..0x28].copy_from_slice(&hash_offset.to_le_bytes());
        data.extend_from_slice(b"HASH");
        data.extend_from_slice(&20u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&hash);
        let total = data.len() as u32;
        data[0x18..0x1c].copy_from_slice(&total.to_le_bytes());

        let extracted = extract_fssm(&data).unwrap();
        assert_eq!(extracted.data, fssm);
        assert_eq!(extracted.code_offset, 0xd0);
        assert_eq!(extracted.code_size, 40);
        assert_eq!(extracted.stage_pointer_offset, 0x58);
        assert_eq!(extracted.stage_descriptor_offset, 0xc0);
        assert_eq!(extracted.dxbc_hash_match, Some(true));

        let text = disassemble_with_source(&data, "synthetic SGD5 shader");
        assert!(text.contains("v_nop_e64"));
        assert!(text.contains("s_inst_prefetch"));
        assert!(text.contains("s_swappc_b64"));
        assert!(text.contains("s_endpgm"));
        assert!(text.contains("synthetic SGD5 shader"));
        *data.last_mut().unwrap() ^= 1;
        assert_eq!(extract_fssm(&data).unwrap().dxbc_hash_match, Some(false));
    }

    #[test]
    fn declared_program_does_not_depend_on_instruction_markers() {
        let data = synthetic_fssm(&[0xbf810000, 0xbf800000, 0xbf810000, 0xbf9f0000]);
        let shader = parse_fssm(data).unwrap();
        assert_eq!(shader.code_offset, 0xd0);
        assert_eq!(shader.code_size, 16);
        assert_eq!(shader.compiler_hash, None);
        assert_eq!(shader.compiled_wave_size(), None);
        let instructions = crate::rdna2::decode_gfx103(shader.code(), 10);
        assert_eq!(instructions.len(), 4);
        assert_eq!(instructions[2].mnemonic, "s_endpgm");
    }

    #[test]
    fn compiled_wave_size_comes_from_metadata_before_the_program() {
        let mut data = synthetic_fssm(&fingerprint_program());
        data.splice(0xd0..0xd0, [0; 32]);
        data[0xc0..0xc8].copy_from_slice(&0xd0u64.to_le_bytes());
        let size = data.len() as u64 - 16;
        data[8..16].copy_from_slice(&size.to_le_bytes());
        data[0xd4] = 0x20;
        assert_eq!(
            parse_fssm(data.clone()).unwrap().compiled_wave_size(),
            Some(32)
        );
        data[0xd4] = 0;
        assert_eq!(parse_fssm(data).unwrap().compiled_wave_size(), Some(64));
    }

    #[test]
    fn compute_metadata_reads_descriptor_fields_and_bounds_thread_counts() {
        let mut data = synthetic_fssm(&[0xbf810000]);
        data.splice(0xd0..0xd0, [0; 128]);
        data[0x58..0x60].copy_from_slice(&FSSM_EMPTY_STAGE.to_le_bytes());
        data[0x70..0x78].copy_from_slice(&0xb0u64.to_le_bytes());
        data[0xc0..0xc8].copy_from_slice(&0x140u64.to_le_bytes());
        data[0x138..0x13c].copy_from_slice(&(13u32 << 1).to_le_bytes());
        for (offset, value) in [(0x13c, 8u32), (0x140, 8), (0x144, 1)] {
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        let size = data.len() as u64 - 16;
        data[8..16].copy_from_slice(&size.to_le_bytes());
        let mut shader = parse_fssm(data).unwrap();
        let metadata = shader.compute_metadata().unwrap();
        assert_eq!(metadata.user_sgpr_count, 13);
        assert_eq!(metadata.num_threads, [8, 8, 1]);
        shader.data[0x13c..0x140].copy_from_slice(&1024u32.to_le_bytes());
        assert!(shader.compute_metadata().is_none());
        shader.data[0x13c..0x140].copy_from_slice(&0u32.to_le_bytes());
        assert!(shader.compute_metadata().is_none());
        shader.data.truncate(0x140);
        assert!(shader.compute_metadata().is_none());
        shader.stage_pointer_offset = 0x40;
        assert!(shader.compute_metadata().is_none());
    }

    #[test]
    fn invalid_fssm_sizes_and_relative_offsets_are_rejected() {
        let data = synthetic_fssm(&fingerprint_program());
        for length in 0..data.len() {
            assert!(parse_fssm(data[..length].to_vec()).is_err());
        }
        let mut invalid = data.clone();
        invalid[0x58..0x60].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse_fssm(invalid).is_err());
        let mut invalid = data.clone();
        invalid[0xc8..0xd0].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse_fssm(invalid).is_err());
        let mut invalid = data.clone();
        invalid[0x20..0x28].copy_from_slice(&0xb0u64.to_le_bytes());
        assert!(parse_fssm(invalid).unwrap_err().contains("multiple active"));
    }

    #[test]
    #[ignore = "requires PERCY_SGD5_SAMPLE pointing to the analysis archive"]
    fn archive_native_variants_metadata_and_priv_joins() {
        let path = std::env::var_os("PERCY_SGD5_SAMPLE").expect("set PERCY_SGD5_SAMPLE");
        let files = crate::sgd5::load_sgd5(std::path::Path::new(&path)).unwrap();
        let (mut shaders, mut programs, mut alternate, mut used, mut layouts) = (0, 0, 0, 0, 0);
        let (mut table_joins, mut compute_programs) = (0, 0);
        for file in &files {
            for shader in &file.shaders {
                let blob = crate::sgd5::decompress_shader(file, shader).unwrap();
                let root = extract_compact_root_signature(&blob).unwrap();
                let variants = extract_fssm_programs(&blob).unwrap();
                shaders += 1;
                alternate += usize::from(variants.len() == 2);
                for program in &variants {
                    let native = program.native_metadata().unwrap_or_else(|error| {
                        panic!(
                            "{} {}: {error}",
                            shader.name,
                            program.hardware_stage().tag()
                        )
                    });
                    assert_eq!(program.dxbc_hash_match, Some(true), "{}", shader.name);
                    programs += 1;
                    layouts += native.root_layout.len();
                    for resource in &native.used_resources {
                        if resource.root_type == NativeRootType::Table {
                            table_joins += 1;
                            let (kind, register, space, _) =
                                resource.binding(&root).unwrap_or_else(|| {
                                    panic!("{}: unresolved {resource:?}", shader.name)
                                });
                            assert!(
                                shader.reflection.resources.iter().any(|record| {
                                    crate::sgd5_metadata::resource_kind(record) == kind
                                        && record.shader_register() == register
                                        && record.register_space() == space
                                }),
                                "{}: missing reflected {}{register} space{space}",
                                shader.name,
                                kind.register_prefix()
                            );
                        }
                        used += 1;
                    }
                    if let NativeStageIo::Compute(compute) = native.io {
                        compute_programs += 1;
                        let psv = find_dxbc_chunk(&blob, b"PSV0").unwrap().unwrap();
                        for (axis, dimension) in compute.num_threads.iter().enumerate() {
                            assert_eq!(
                                u32::from(*dimension),
                                read_u32(psv, 40 + axis * 4).unwrap()
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(
            (shaders, programs, alternate, layouts, used),
            (26_503, 26_698, 195, 5_214_262, 445_625)
        );
        assert_eq!((table_joins, compute_programs), (385_337, 2_689));
    }

    #[test]
    #[ignore = "requires PERCY_SGD5_SAMPLE pointing to the analysis archive"]
    fn all_sample_fssm_programs_use_declared_ranges_and_match_compiler_hashes() {
        let path = std::env::var_os("PERCY_SGD5_SAMPLE").expect("set PERCY_SGD5_SAMPLE");
        let files = crate::sgd5::load_sgd5(std::path::Path::new(&path)).unwrap();
        let mut count = 0;
        let mut leading_calls = 0;
        for file in &files {
            for shader in &file.shaders {
                let blob = crate::sgd5::decompress_shader(file, shader).unwrap();
                let program = extract_fssm(&blob).unwrap();
                assert_eq!(program.version, 0x101);
                assert_eq!(program.payload_size as usize, program.data.len() - 16);
                assert_eq!(program.stage_descriptor_offset, 0xc0);
                assert_eq!(program.dxbc_hash_match, Some(true));
                assert_eq!(program.code_size % 64, 0);
                let instructions = crate::rdna2::decode_gfx103(program.code(), 3);
                assert_eq!(instructions[0].mnemonic, "v_nop_e64");
                assert_eq!(instructions[0].offset, 0);
                assert_eq!(instructions[1].offset, 12);
                assert_eq!(instructions[1].mnemonic, "v_nop_e64");
                assert_eq!(instructions[2].offset, 24);
                leading_calls += usize::from(instructions[2].mnemonic == "s_swappc_b64");
                count += 1;
            }
        }
        assert_eq!(count, 26_503);
        assert_eq!(leading_calls, 630);
    }
}
