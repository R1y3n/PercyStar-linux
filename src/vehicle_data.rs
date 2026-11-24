//! Vehicle physics components from the PSIN metadata file in `data.rpf`.
//!
//! The file holds one name table whose entries carry a name hash, a parent hash and a list of
//! pointers to component records. A component record is one row of a struct typed instance.
//! An entry only lists the components it defines itself, everything else comes from the parent
//! chain (`cheetah2` -> ... -> `car` -> ... -> `vehicle`) where the nearest record of a struct wins.

use crate::names;
use crate::psin::joaat;
use eframe::egui::{self, Color32, RichText};
use flate2::{Decompress, FlushDecompress, Status};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

// where the file sits in the 1.0.2.1 xbox data.rpf tried first so the scan is usually skipped
pub const OFFSET_HINT: u64 = 0x1c35_b000;

const NAME_TABLE: u32 = 0x9077_9a14;
const NAME_FIELD: u32 = 0x93db_5464;
const PARENT_FIELD: u32 = 0x49a0_1a86;
const COMPONENTS_FIELD: u32 = 0x41c8_dec7;
// field name of an array or map element descriptor
const ELEMENT: u32 = 0x0000_0100;

const PAGE: usize = 65536;
const MAX_FILE: usize = 64 << 20;
const MAX_DEPTH: usize = 8;
const MAX_VALUES: usize = 20_000;
const MAX_ROWS: usize = 4_000;

// template rows of the name table, added to the shared names so every view resolves them
const TEMPLATE_NAMES: &[&str] = &[
    "vehicle", "car", "bus", "truck", "van", "utility", "tractor", "trailer", "train", "bike", "bicycle",
    "quadbike", "boat", "forklift",
];

// scanned vehicle model names by lower case hash
pub type Names = HashMap<u32, String>;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f32),
    Vector(Vec<f32>),
    Hash(u32),
    Hash64(u64),
    Enum { raw: i64, name: Option<u32> },
    Flags { raw: Vec<u32>, names: Vec<u32> },
    Struct(u32, Vec<Field>),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Null,
    // decode budget or pointer depth ran out
    Skipped,
    Unknown(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: u32,
    pub value: Value,
}

// one display line of a component nested values are indented by depth
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub depth: usize,
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct Component {
    pub struct_hash: u32,
    pub record: usize,
    // name table entry that defines the record
    pub owner: u32,
    // steps up the parent chain 0 means the vehicle defines the record itself
    pub depth: usize,
    // name table entries that resolve to this record including this one
    pub shared_by: u32,
    // typed values the panel only draws the rows built from them
    #[allow(dead_code)]
    pub fields: Vec<Field>,
    pub rows: Vec<Row>,
}

impl Component {
    pub fn title(&self, names: &Names) -> String {
        let mut out = format!("{} · record {}", schema_label(self.struct_hash), self.record);
        if self.depth == 0 {
            out.push_str(" · own");
            if self.shared_by > 1 {
                let _ = write!(out, " · shared by {} entries", self.shared_by);
            }
        } else {
            let _ = write!(out, " · inherited from {}", value_label(self.owner, names));
            let _ = write!(out, " · shared by {} entries", self.shared_by);
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct VehicleData {
    pub name_hash: u32,
    // parent chain nearest first
    pub parents: Vec<u32>,
    // own components first then inherited ones nearest parent first
    pub components: Vec<Component>,
}

impl VehicleData {
    pub fn own_count(&self) -> usize {
        self.components.iter().filter(|c| c.depth == 0).count()
    }

    pub fn to_text(&self, names: &Names) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{} (0x{:08x})", value_label(self.name_hash, names), self.name_hash);
        if !self.parents.is_empty() {
            let chain: Vec<String> = self.parents.iter().map(|&h| value_label(h, names)).collect();
            let _ = writeln!(out, "inherits: {}", chain.join(" > "));
        }
        for component in &self.components {
            let _ = writeln!(out, "\n[{}]", component.title(names));
            for row in &component.rows {
                let _ = writeln!(out, "{:indent$}{} = {}", "", row.label, row.value, indent = 2 + row.depth * 2);
            }
        }
        out
    }
}

fn marked(hash: u32, name: String) -> String {
    // assembled names may be chance matches
    if names::is_built(hash) {
        name + "?"
    } else {
        name
    }
}

// name of a struct, field or enum value, hashed as written
pub fn schema_label(hash: u32) -> String {
    match names::lookup(hash) {
        Some(name) => marked(hash, name),
        None => format!("0x{hash:08x}"),
    }
}

// name behind a hash stored as data such as a vehicle or template row, hashed in lower case
pub fn value_label(hash: u32, models: &Names) -> String {
    if let Some(name) = models.get(&hash) {
        return name.clone();
    }
    match names::lookup_value(hash) {
        Some(name) => marked(hash, name),
        None => format!("0x{hash:08x}"),
    }
}

#[derive(Clone, Debug)]
struct FieldDef {
    name: u32,
    offset: usize,
    kind: u8,
    // upper half of the type word element descriptor index for containers bit count for flags
    sub: usize,
    type_raw: u32,
    reference: u32,
}

#[derive(Clone, Debug)]
struct StructDef {
    size: usize,
    fields: Vec<FieldDef>,
}

#[derive(Clone, Copy, Debug)]
struct Instance {
    tag: u32,
    offset: usize,
    size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Target {
    instance: usize,
    offset: usize,
}

#[derive(Clone, Debug)]
struct Entry {
    parent: u32,
    components: Vec<Target>,
}

pub struct VehicleDb {
    payload: Vec<u8>,
    instances: Vec<Instance>,
    structs: HashMap<u32, StructDef>,
    enums: HashMap<u32, Vec<(i64, u32)>>,
    entries: HashMap<u32, Entry>,
    users: HashMap<Target, u32>,
    // offset of the file inside data.rpf zero when parsed from memory
    pub source_offset: u64,
}

fn rd16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o.checked_add(2)?)?.try_into().ok()?))
}

fn rd32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

fn rd64(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

fn rdf32(d: &[u8], o: usize) -> Option<f32> {
    rd32(d, o).map(f32::from_bits)
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f32;
    let magnitude = match exp {
        0 => frac * 2f32.powi(-24),
        31 => {
            if frac == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    };
    sign * magnitude
}

// iff style sections tag then big endian size that includes the 8 byte header
fn sections(data: &[u8]) -> (Vec<(&[u8], &[u8])>, usize) {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        if size < 8 || pos + size > data.len() {
            break;
        }
        out.push((&data[pos..pos + 4], &data[pos + 8..pos + size]));
        pos += size;
    }
    (out, pos)
}

// true once the buffer holds every section up to the trailing checksum
fn file_complete(data: &[u8]) -> bool {
    let (found, end) = sections(data);
    end == data.len() && found.last().is_some_and(|(tag, _)| *tag == b"CHKS")
}

fn parse_instances(body: &[u8]) -> Option<(Vec<Instance>, usize)> {
    let mut pos = 0usize;
    let mut cursor = 0usize;
    let mut instances = Vec::new();
    loop {
        let head = body.get(pos..pos + 8)?;
        if head == b"pppppppp" {
            return Some((instances, pos + 8));
        }
        let (tag, size) = (rd32(body, pos)?, rd32(body, pos + 4)? as usize);
        instances.push(Instance { tag, offset: cursor, size });
        // every instance is padded to 16 bytes in the payload
        cursor = cursor.checked_add(size.checked_add(15)? & !15)?;
        pos += 16;
    }
}

// all 32 byte struct headers come first then the 16 byte field entries of every struct in order
fn parse_structs(lsch: &[u8]) -> HashMap<u32, StructDef> {
    let mut pos = 0usize;
    let mut headers = Vec::new();
    while pos + 32 <= lsch.len() && lsch[pos + 16..pos + 24] == [0xaa; 8] {
        let hash = rd32(lsch, pos).unwrap_or(0);
        let size = rd32(lsch, pos + 24).unwrap_or(0) as usize;
        let count = (rd32(lsch, pos + 28).unwrap_or(0) >> 16) as usize;
        headers.push((hash, size, count));
        pos += 32;
    }
    let mut structs = HashMap::with_capacity(headers.len());
    for (hash, size, count) in headers {
        let mut fields = Vec::with_capacity(count.min(lsch.len().saturating_sub(pos) / 16));
        for _ in 0..count {
            let (Some(name), Some(offset), Some(type_raw), Some(reference)) =
                (rd32(lsch, pos), rd32(lsch, pos + 4), rd32(lsch, pos + 8), rd32(lsch, pos + 12))
            else {
                break;
            };
            fields.push(FieldDef {
                name,
                offset: offset as usize,
                kind: type_raw as u8,
                sub: (type_raw >> 16) as usize,
                type_raw,
                reference,
            });
            pos += 16;
        }
        structs.insert(hash, StructDef { size, fields });
    }
    structs
}

// all 24 byte enum headers come first then 16 byte entries of value and name hash
fn parse_enums(lenm: &[u8]) -> HashMap<u32, Vec<(i64, u32)>> {
    let mut pos = 0usize;
    let mut headers = Vec::new();
    while pos + 24 <= lenm.len() && lenm[pos + 8..pos + 16] == [0xee; 8] {
        headers.push((rd32(lenm, pos).unwrap_or(0), rd32(lenm, pos + 16).unwrap_or(0) as usize));
        pos += 24;
    }
    let mut enums = HashMap::with_capacity(headers.len());
    for (hash, count) in headers {
        let mut values = Vec::with_capacity(count.min(lenm.len().saturating_sub(pos) / 16));
        for _ in 0..count {
            let (Some(value), Some(name)) = (rd64(lenm, pos), rd32(lenm, pos + 8)) else { break };
            values.push((value as i64, name));
            pos += 16;
        }
        enums.insert(hash, values);
    }
    enums
}

impl VehicleDb {
    pub fn parse(data: &[u8]) -> Result<VehicleDb, String> {
        if data.len() < 16 || &data[..4] != b"PSIN" {
            return Err("not a psin file".into());
        }
        let (found, _) = sections(data);
        let body = |tag: &[u8]| found.iter().find(|(t, _)| *t == tag).map(|(_, b)| *b).unwrap_or(&[]);
        let psin = body(b"PSIN");
        let (instances, payload_at) = parse_instances(psin).ok_or("truncated instance table")?;
        let mut db = VehicleDb {
            payload: psin[payload_at..].to_vec(),
            instances,
            structs: parse_structs(body(b"LSCH")),
            enums: parse_enums(body(b"LENM")),
            entries: HashMap::new(),
            users: HashMap::new(),
            source_offset: 0,
        };
        db.read_name_table()?;
        names::add(TEMPLATE_NAMES.iter());
        let hashes: Vec<u32> = db.entries.keys().copied().collect();
        for hash in hashes {
            for (_, _, target) in db.effective(hash) {
                *db.users.entry(target).or_insert(0) += 1;
            }
        }
        Ok(db)
    }

    fn read_name_table(&mut self) -> Result<(), String> {
        let def = self.structs.get(&NAME_TABLE).ok_or("no vehicle name table struct")?;
        let field = |name: u32| def.fields.iter().find(|f| f.name == name);
        let (Some(name_f), Some(parent_f), Some(list_f)) = (field(NAME_FIELD), field(PARENT_FIELD), field(COMPONENTS_FIELD))
        else {
            return Err("vehicle name table has an unexpected layout".into());
        };
        if def.size == 0 {
            return Err("vehicle name table has an unexpected layout".into());
        }
        let mut entries = HashMap::new();
        for inst in self.instances.iter().filter(|i| i.tag == NAME_TABLE) {
            for record in 0..inst.size / def.size {
                let at = inst.offset + record * def.size;
                let (Some(name), Some(parent)) = (rd32(&self.payload, at + name_f.offset), rd32(&self.payload, at + parent_f.offset))
                else {
                    break;
                };
                let mut components = Vec::new();
                if let Some((base, count, elem, elem_size)) = self.array_span(list_f, &def.fields, at + list_f.offset) {
                    if elem.kind == 0x07 {
                        for i in 0..count {
                            let target = rd64(&self.payload, base + i * elem_size).and_then(|p| self.resolve(p));
                            if let Some(target) = target.filter(|t| self.record_of(*t).is_some()) {
                                components.push(target);
                            }
                        }
                    }
                }
                entries.insert(name, Entry { parent, components });
            }
        }
        if entries.is_empty() {
            return Err("vehicle name table is empty".into());
        }
        self.entries = entries;
        Ok(())
    }

    pub fn contains(&self, name_hash: u32) -> bool {
        self.entries.contains_key(&name_hash)
    }

    // a pointer packs the one based instance index in the low bits and the byte offset above bit 20
    // only 120 instances exist so whether the index is 12 or 20 bits wide cannot be told apart
    fn resolve(&self, pointer: u64) -> Option<Target> {
        let index = (pointer & 0xf_ffff) as usize;
        let offset = (pointer >> 20) as usize;
        let inst = self.instances.get(index.checked_sub(1)?)?;
        (offset < inst.size).then_some(Target { instance: index - 1, offset })
    }

    // struct hash and record index when the target is the start of a record
    fn record_of(&self, target: Target) -> Option<(u32, usize)> {
        let inst = self.instances.get(target.instance)?;
        let size = self.structs.get(&inst.tag)?.size;
        (size > 0 && target.offset % size == 0).then_some((inst.tag, target.offset / size))
    }

    fn type_size(&self, def: &FieldDef, siblings: &[FieldDef]) -> Option<usize> {
        Some(match def.kind {
            0x01 | 0x11 | 0x60 => 1,
            0x13 | 0x20 => 2,
            0x14 | 0x15 | 0x21 | 0x4a | 0x4b | 0x4d | 0x62 => 4,
            0x07 | 0x08 | 0x30 | 0x4f => 8,
            0x31 | 0x33 | 0x34 | 0x52 | 0x5c | 0x75 => 16,
            0x36 => 64,
            0x05 => self.structs.get(&def.reference)?.size,
            0x65 => def.sub.div_ceil(32).max(1) * 4,
            0x50 => self.type_size(siblings.get(def.sub)?, siblings)?.checked_mul(def.reference as usize)?,
            _ => return None,
        })
    }

    // payload offset element count element descriptor and element size of a heap array
    fn array_span<'a>(&self, def: &FieldDef, siblings: &'a [FieldDef], at: usize) -> Option<(usize, usize, &'a FieldDef, usize)> {
        let (pointer, count) = match def.kind {
            // pointer then u16 count and u16 capacity
            0x52 => (rd64(&self.payload, at)?, rd16(&self.payload, at + 8)? as usize),
            // u32 count and u32 capacity then pointer
            0x5c => (rd64(&self.payload, at + 8)?, rd32(&self.payload, at)? as usize),
            _ => return None,
        };
        let elem = siblings.get(def.sub)?;
        // two always empty arrays name element structs the schema does not define
        if count == 0 {
            return Some((0, 0, elem, 1));
        }
        let elem_size = self.type_size(elem, siblings).filter(|&s| s > 0)?;
        let target = self.resolve(pointer)?;
        let inst = self.instances[target.instance];
        let count = count.min((inst.size - target.offset) / elem_size);
        Some((inst.offset + target.offset, count, elem, elem_size))
    }

    fn decode_struct(&self, hash: u32, at: usize, depth: usize, budget: &mut usize) -> Value {
        let Some(def) = self.structs.get(&hash) else { return Value::Unknown(0x05) };
        let fields = def
            .fields
            .iter()
            .filter(|f| f.name != ELEMENT)
            .map(|f| Field { name: f.name, value: self.decode(f, &def.fields, at + f.offset, depth, budget) })
            .collect();
        Value::Struct(hash, fields)
    }

    fn decode_items(&self, elem: &FieldDef, siblings: &[FieldDef], base: usize, count: usize, size: usize, depth: usize, budget: &mut usize) -> Value {
        Value::Array((0..count).map(|i| self.decode(elem, siblings, base + i * size, depth, budget)).collect())
    }

    fn decode(&self, def: &FieldDef, siblings: &[FieldDef], at: usize, depth: usize, budget: &mut usize) -> Value {
        if *budget == 0 {
            return Value::Skipped;
        }
        *budget -= 1;
        let d = &self.payload[..];
        let unknown = Value::Unknown(def.type_raw);
        let floats = |n: usize| (0..n).map(|i| rdf32(d, at + i * 4)).collect::<Option<Vec<f32>>>().map(Value::Vector);
        let decoded = match def.kind {
            0x01 => d.get(at).map(|&b| Value::Bool(b != 0)),
            0x11 => d.get(at).map(|&b| Value::Int(b as i64)),
            0x14 => rd32(d, at).map(|v| Value::Int(v as i32 as i64)),
            0x21 => rdf32(d, at).map(Value::Float),
            // sixteen byte vectors the unused lanes of a vec2 repeat x and are dropped
            0x31 => floats(2),
            0x33 => floats(3),
            0x34 => floats(4),
            0x4a | 0x4d => rd32(d, at).map(Value::Hash),
            0x4f => rd64(d, at).map(Value::Hash64),
            0x62 => rd32(d, at).map(|v| {
                let raw = v as i32 as i64;
                let name = self.enums.get(&def.reference).and_then(|e| e.iter().find(|(value, _)| *value == raw)).map(|e| e.1);
                Value::Enum { raw, name }
            }),
            // bit count sits in the type word each bit is named by the enum entry of the same value
            0x65 => {
                let words = def.sub.div_ceil(32).max(1);
                (0..words).map(|i| rd32(d, at + i * 4)).collect::<Option<Vec<u32>>>().map(|raw| {
                    let values = self.enums.get(&def.reference);
                    let names = (0..def.sub.min(words * 32))
                        .filter(|&bit| raw[bit / 32] >> (bit % 32) & 1 != 0)
                        .map(|bit| {
                            values
                                .and_then(|e| e.iter().find(|(value, _)| *value == bit as i64))
                                .map(|e| e.1)
                                .unwrap_or(bit as u32)
                        })
                        .collect();
                    Value::Flags { raw, names }
                })
            }
            0x05 => Some(self.decode_struct(def.reference, at, depth, budget)),
            0x07 => rd64(d, at).map(|pointer| {
                if pointer == 0 {
                    return Value::Null;
                }
                if depth >= MAX_DEPTH {
                    return Value::Skipped;
                }
                match self.resolve(pointer) {
                    Some(target) if self.record_of(target).is_some() => {
                        let inst = self.instances[target.instance];
                        self.decode_struct(inst.tag, inst.offset + target.offset, depth + 1, budget)
                    }
                    _ => Value::Unknown(def.type_raw),
                }
            }),
            0x52 | 0x5c => self
                .array_span(def, siblings, at)
                .map(|(base, count, elem, size)| self.decode_items(elem, siblings, base, count, size, depth, budget)),
            // fixed count inline array
            0x50 => siblings.get(def.sub).and_then(|elem| {
                let size = self.type_size(elem, siblings)?;
                Some(self.decode_items(elem, siblings, at, def.reference as usize, size, depth, budget))
            }),
            // inline storage for a fixed capacity followed by the used count
            // seen in one struct only so this layout is a best guess
            0x53 => siblings.get(def.sub).and_then(|elem| {
                let size = self.type_size(elem, siblings)?;
                let capacity = def.reference as usize;
                let count = rd32(d, at + capacity * size)? as usize;
                (count <= capacity).then(|| self.decode_items(elem, siblings, at, count, size, depth, budget))
            }),
            0x75 => self.decode_map(at, depth, budget),
            // not present in the vehicle file sizes follow the other psin files
            0x08 => rd64(d, at).map(|v| Value::Int(v as i64)),
            0x13 => rd16(d, at).map(|v| Value::Int(v as i64)),
            0x15 => rd32(d, at).map(|v| Value::Int(v as i64)),
            0x20 => rd16(d, at).map(|v| Value::Float(half_to_f32(v))),
            0x30 => floats(2),
            0x36 => floats(16),
            0x4b => rd32(d, at).map(Value::Hash),
            0x60 => d.get(at).map(|&b| {
                let raw = b as i64;
                let name = self.enums.get(&def.reference).and_then(|e| e.iter().find(|(value, _)| *value == raw)).map(|e| e.1);
                Value::Enum { raw, name }
            }),
            _ => None,
        };
        decoded.unwrap_or(unknown)
    }

    // a map is an open addressing table of 16 slot groups
    // each group starts with 16 control bytes where 0x80 marks an empty slot
    fn decode_map(&self, at: usize, depth: usize, budget: &mut usize) -> Option<Value> {
        let d = &self.payload[..];
        let groups = rd32(d, at)? as usize;
        let pointer = rd64(d, at + 8)?;
        if pointer == 0 || groups == 0 {
            return Some(Value::Map(Vec::new()));
        }
        let target = self.resolve(pointer)?;
        let inst = self.instances[target.instance];
        let group = self.structs.get(&inst.tag)?;
        let mut real = group.fields.iter().filter(|f| f.name != ELEMENT);
        let (control, slots) = (real.next()?, real.next()?);
        if control.kind != 0x50 || slots.kind != 0x50 || group.size == 0 {
            return None;
        }
        let slot = group.fields.get(slots.sub)?;
        let slot_size = self.type_size(slot, &group.fields)?;
        let per_group = (control.reference.min(slots.reference)) as usize;
        let groups = groups.min((inst.size - target.offset) / group.size);
        let mut entries = Vec::new();
        for g in 0..groups {
            let base = inst.offset + target.offset + g * group.size;
            for i in 0..per_group {
                if *d.get(base + control.offset + i)? >= 0x80 {
                    continue;
                }
                match self.decode(slot, &group.fields, base + slots.offset + i * slot_size, depth, budget) {
                    Value::Struct(_, mut pair) if pair.len() == 2 => {
                        let value = pair.pop()?.value;
                        entries.push((pair.pop()?.value, value));
                    }
                    other => entries.push((Value::Int(i as i64), other)),
                }
            }
        }
        // the header also stores the entry count at +4 it always equals the occupied slots
        Some(Value::Map(entries))
    }

    // parent chain starting at the entry itself
    fn chain(&self, name_hash: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut hash = name_hash;
        while let Some(entry) = self.entries.get(&hash) {
            if out.contains(&hash) || out.len() >= 64 {
                break;
            }
            out.push(hash);
            hash = entry.parent;
        }
        out
    }

    // records an entry resolves to with their owner and chain depth nearest record of a struct wins
    fn effective(&self, name_hash: u32) -> Vec<(u32, usize, Target)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (depth, owner) in self.chain(name_hash).into_iter().enumerate() {
            for &target in &self.entries[&owner].components {
                if seen.insert(self.instances[target.instance].tag) {
                    out.push((owner, depth, target));
                }
            }
        }
        out
    }

    pub fn lookup(&self, name_hash: u32, names: &Names) -> Option<VehicleData> {
        if !self.entries.contains_key(&name_hash) {
            return None;
        }
        let components = self
            .effective(name_hash)
            .into_iter()
            .filter_map(|(owner, depth, target)| {
                let (struct_hash, record) = self.record_of(target)?;
                let inst = self.instances[target.instance];
                let mut budget = MAX_VALUES;
                let Value::Struct(_, fields) = self.decode_struct(struct_hash, inst.offset + target.offset, 0, &mut budget) else {
                    return None;
                };
                let mut rows = Vec::new();
                for field in &fields {
                    push_rows(&mut rows, 0, schema_label(field.name), &field.value, names);
                }
                Some(Component {
                    struct_hash,
                    record,
                    owner,
                    depth,
                    shared_by: self.users.get(&target).copied().unwrap_or(1),
                    fields,
                    rows,
                })
            })
            .collect();
        Some(VehicleData {
            name_hash,
            parents: self.chain(name_hash).into_iter().skip(1).collect(),
            components,
        })
    }
}

fn scalar_text(value: &Value, names: &Names) -> Option<String> {
    Some(match value {
        Value::Bool(v) => v.to_string(),
        Value::Int(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        Value::Vector(v) => format!("({})", v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ")),
        Value::Hash(0) => "0".into(),
        Value::Hash(v) => value_label(*v, names),
        Value::Hash64(v) => format!("0x{v:016x}"),
        Value::Enum { raw, name: Some(name) } => format!("{} ({raw})", schema_label(*name)),
        Value::Enum { raw, name: None } => raw.to_string(),
        Value::Flags { raw, names: set } => {
            let hex: String = raw.iter().rev().map(|w| format!("{w:08x}")).collect();
            if set.is_empty() {
                format!("0x{hex}")
            } else {
                format!("0x{hex} [{}]", set.iter().map(|&n| schema_label(n)).collect::<Vec<_>>().join(", "))
            }
        }
        Value::Null => "null".into(),
        Value::Skipped => "…".into(),
        Value::Unknown(type_raw) => format!("unknown type 0x{type_raw:08x}"),
        Value::Struct(..) | Value::Array(_) | Value::Map(_) => return None,
    })
}

fn push_rows(rows: &mut Vec<Row>, depth: usize, label: String, value: &Value, names: &Names) {
    if rows.len() >= MAX_ROWS {
        if rows.len() == MAX_ROWS {
            rows.push(Row { depth: 0, label: "…".into(), value: "more values not shown".into() });
        }
        return;
    }
    if let Some(text) = scalar_text(value, names) {
        rows.push(Row { depth, label, value: text });
        return;
    }
    match value {
        Value::Struct(hash, fields) => {
            rows.push(Row { depth, label, value: format!("struct {}", schema_label(*hash)) });
            for field in fields {
                push_rows(rows, depth + 1, schema_label(field.name), &field.value, names);
            }
        }
        Value::Array(items) => {
            let inline: Option<Vec<String>> = items.iter().map(|item| scalar_text(item, names)).collect();
            match inline {
                Some(texts) if texts.len() <= 16 => rows.push(Row { depth, label, value: format!("[{}]", texts.join(", ")) }),
                _ => {
                    rows.push(Row { depth, label, value: format!("{} items", items.len()) });
                    for (index, item) in items.iter().enumerate() {
                        push_rows(rows, depth + 1, format!("[{index}]"), item, names);
                    }
                }
            }
        }
        Value::Map(entries) => {
            rows.push(Row { depth, label, value: format!("map of {}", entries.len()) });
            for (key, item) in entries {
                let key = scalar_text(key, names).unwrap_or_else(|| "?".into());
                push_rows(rows, depth + 1, key, item, names);
            }
        }
        _ => {}
    }
}

fn zlib_head(d: &[u8]) -> bool {
    d.len() >= 2 && d[0] == 0x78 && (((d[0] as u16) << 8) | d[1] as u16) % 31 == 0
}

fn read_full(file: &mut File, buf: &mut [u8]) -> usize {
    let mut filled = 0usize;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => filled += n,
        }
    }
    filled
}

// a file is stored as zlib streams of 64k pages each stream starts on a 16 byte boundary
fn read_paged(file: &mut File, offset: u64, first_only: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut raw = vec![0u8; 2 * PAGE];
    let mut pos = offset;
    loop {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let n = read_full(file, &mut raw);
        if !zlib_head(&raw[..n]) {
            break;
        }
        let before = out.len();
        out.reserve(PAGE + 64);
        let mut inflater = Decompress::new(true);
        if !matches!(inflater.decompress_vec(&raw[..n], &mut out, FlushDecompress::Finish), Ok(Status::StreamEnd)) {
            out.truncate(before);
            break;
        }
        pos = (pos + inflater.total_in() + 15) & !15;
        if first_only || out.len() - before != PAGE || out.len() > MAX_FILE || file_complete(&out) {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

// loads the file at offset when it is the vehicle metadata file
fn try_offset(file: &mut File, offset: u64) -> Option<VehicleDb> {
    let head = read_paged(file, offset, true)?;
    if !head.starts_with(b"PSIN") {
        return None;
    }
    // the instance table sits at the start so the first page tells whether the name table exists
    let mut table = head.get(8..)?.chunks_exact(16).take_while(|e| &e[..8] != b"pppppppp");
    if !table.any(|e| rd32(e, 0) == Some(NAME_TABLE)) {
        return None;
    }
    let data = read_paged(file, offset, false)?;
    let mut db = VehicleDb::parse(&data).ok()?;
    // other files may reuse the table struct the vehicle one is told apart by its root template
    if !db.contains(joaat("vehicle")) {
        return None;
    }
    db.source_offset = offset;
    Some(db)
}

// finds the vehicle metadata file inside data.rpf the hint is tried before scanning the archive
fn locate(file: &mut File, hint: Option<u64>) -> Option<VehicleDb> {
    if let Some(db) = hint.and_then(|offset| try_offset(file, offset)) {
        return Some(db);
    }
    const BLOCK: usize = 4 << 20;
    const PEEK: usize = 256;
    let len = file.metadata().ok()?.len();
    let mut buf = vec![0u8; BLOCK + PEEK];
    let mut inflater = Decompress::new(true);
    let mut probe = [0u8; 8];
    let mut base = 0u64;
    while base < len {
        file.seek(SeekFrom::Start(base)).ok()?;
        let n = read_full(file, &mut buf);
        let mut pos = 0usize;
        while pos < n.min(BLOCK) {
            if zlib_head(&buf[pos..n]) {
                inflater.reset(true);
                let end = (pos + PEEK).min(n);
                let _ = inflater.decompress(&buf[pos..end], &mut probe, FlushDecompress::None);
                if inflater.total_out() >= 4 && &probe[..4] == b"PSIN" {
                    if let Some(db) = try_offset(file, base + pos as u64) {
                        return Some(db);
                    }
                }
            }
            pos += 16;
        }
        base += BLOCK as u64;
    }
    None
}

type Cache = Mutex<HashMap<PathBuf, Option<Arc<VehicleDb>>>>;

// vehicle data of the data.rpf in a game folder parsed once per folder
// none when the folder has no data.rpf or the file holds no vehicle table
pub fn load_from_folder(folder: &Path) -> Option<Arc<VehicleDb>> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    if let Some(found) = cache.get(folder) {
        return found.clone();
    }
    let found = File::open(folder.join("data.rpf"))
        .ok()
        .and_then(|mut file| locate(&mut file, Some(OFFSET_HINT)))
        .map(Arc::new);
    cache.insert(folder.to_path_buf(), found.clone());
    found
}

// collapsible read only list of a vehicle's components for the model viewer side panel
pub fn panel_ui(ui: &mut egui::Ui, data: &VehicleData, names: &Names) {
    let own = data.own_count();
    egui::CollapsingHeader::new(format!("Vehicle data ({own} own · {} inherited)", data.components.len() - own))
        .id_salt("vehicle_data")
        .show(ui, |ui| {
            let note = Color32::from_rgb(135, 145, 165);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Copy values as text")
                    .on_hover_text("Copy every component of this vehicle with its values")
                    .clicked()
                {
                    ui.ctx().copy_text(data.to_text(names));
                }
                if !data.parents.is_empty() {
                    let chain: Vec<String> = data.parents.iter().map(|&hash| value_label(hash, names)).collect();
                    ui.label(RichText::new(format!("Inherits: {}", chain.join(" > "))).size(10.5).color(note));
                }
            });
            egui::ScrollArea::vertical()
                .id_salt("vehicle_data_list")
                .max_height(260.0)
                .show(ui, |ui| {
                    for component in &data.components {
                        // records taken from a parent template are shared with every entry that inherits them
                        let colour = if component.depth == 0 { Color32::from_rgb(180, 210, 190) } else { Color32::from_rgb(215, 180, 120) };
                        egui::CollapsingHeader::new(RichText::new(component.title(names)).size(11.5).color(colour))
                            .id_salt(("vehicle_component", component.struct_hash))
                            .show(ui, |ui| {
                                egui::Grid::new(("vehicle_component_rows", component.struct_hash))
                                    .num_columns(2)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for row in &component.rows {
                                            ui.horizontal(|ui| {
                                                ui.add_space(row.depth as f32 * 12.0);
                                                ui.label(RichText::new(&row.label).monospace().size(11.0));
                                            });
                                            ui.label(RichText::new(&row.value).monospace().size(11.0));
                                            ui.end_row();
                                        }
                                    });
                            });
                    }
                });
            ui.label(
                RichText::new("Read only. Names marked ? are guesses, unknown ones show as hashes. Inherited records belong to a parent template.")
                    .size(10.5)
                    .color(note),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASS_STRUCT: u32 = 0xb4e0_f8b0;

    // nth float among the top level fields
    fn float(component: &Component, index: usize) -> Option<f32> {
        let floats = component.fields.iter().filter_map(|f| match f.value {
            Value::Float(v) => Some(v),
            _ => None,
        });
        floats.clone().nth(index)
    }

    fn component(data: &VehicleData, struct_hash: u32) -> &Component {
        data.components.iter().find(|c| c.struct_hash == struct_hash).expect("component")
    }

    fn put32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn section(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = tag.to_vec();
        out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    fn pointer(instance: u64, offset: u64) -> u64 {
        (instance + 1) | (offset << 20)
    }

    // two entries where the child overrides one of the two parent components
    fn synthetic() -> Vec<u8> {
        const BODY: u32 = 0x1111_1111;
        const TYRE: u32 = 0x2222_2222;
        let mut lsch = Vec::new();
        for (hash, size, fields) in [(NAME_TABLE, 32u32, 4u32), (BODY, 16, 2), (TYRE, 8, 1)] {
            put32(&mut lsch, hash);
            put32(&mut lsch, 0);
            put32(&mut lsch, 0x300);
            put32(&mut lsch, 0);
            lsch.extend_from_slice(&[0xaa; 8]);
            put32(&mut lsch, size);
            put32(&mut lsch, fields << 16);
        }
        for (name, offset, ty, reference) in [
            (NAME_FIELD, 8u32, 0x4au32, 0u32),
            (PARENT_FIELD, 12, 0x4a, 0),
            (ELEMENT, 0, 0x07, 0),
            (COMPONENTS_FIELD, 16, 0x0002_005c, 0),
            (0xaaaa_0001, 8, 0x21, 0),
            (0xaaaa_0002, 12, 0x0001_0065, 0x3333_3333),
            (0xbbbb_0001, 0, 0x21, 0),
        ] {
            for v in [name, offset, ty, reference] {
                put32(&mut lsch, v);
            }
        }
        let mut lenm = Vec::new();
        put32(&mut lenm, 0x3333_3333);
        put32(&mut lenm, 0);
        lenm.extend_from_slice(&[0xee; 8]);
        put32(&mut lenm, 1);
        put32(&mut lenm, 0);
        lenm.extend_from_slice(&0u64.to_le_bytes());
        put32(&mut lenm, 0x4444_4444);
        put32(&mut lenm, 0);

        // instance 0 pointer lists 1 body records 2 tyre record 3 name table
        let mut pointers = Vec::new();
        for p in [pointer(1, 0), pointer(2, 0), pointer(1, 16)] {
            pointers.extend_from_slice(&p.to_le_bytes());
        }
        pointers.extend_from_slice(&[0; 8]);
        let mut bodies = Vec::new();
        for (mass, flags) in [(1000.0f32, 0u32), (1630.0, 1)] {
            bodies.extend_from_slice(&[0; 8]);
            bodies.extend_from_slice(&mass.to_le_bytes());
            put32(&mut bodies, flags);
        }
        let mut tyres = 0.9f32.to_le_bytes().to_vec();
        tyres.extend_from_slice(&[0; 12]);
        let mut table = Vec::new();
        for (name, parent, count, list) in [(joaat("car"), 0u32, 2u32, pointer(0, 0)), (joaat("cheetah2"), joaat("car"), 1, pointer(0, 16))] {
            table.extend_from_slice(&[0; 8]);
            put32(&mut table, name);
            put32(&mut table, parent);
            put32(&mut table, count);
            put32(&mut table, count);
            table.extend_from_slice(&list.to_le_bytes());
        }
        let mut psin = Vec::new();
        for (tag, size) in [(7u32, 24u32), (BODY, 32), (TYRE, 8), (NAME_TABLE, 64)] {
            put32(&mut psin, tag);
            put32(&mut psin, size);
            psin.extend_from_slice(&[0xaa; 8]);
        }
        psin.extend_from_slice(b"pppppppp");
        for block in [&pointers, &bodies, &tyres, &table] {
            psin.extend_from_slice(block);
        }
        let mut file = section(b"PSIN", &psin);
        file.extend(section(b"LSCH", &lsch));
        file.extend(section(b"LENM", &lenm));
        file.extend(section(b"CHKS", &[0; 12]));
        file
    }

    #[test]
    fn child_overrides_parent_records_of_the_same_struct() {
        let data = synthetic();
        assert!(file_complete(&data));
        let db = VehicleDb::parse(&data).expect("synthetic file");
        assert_eq!(db.entries.len(), 2);
        let names = Names::new();
        let car = db.lookup(joaat("car"), &names).unwrap();
        assert_eq!(car.own_count(), 2);
        assert_eq!(float(&car.components[0], 0), Some(1000.0));
        // the tyre record is the effective one for both entries the car body only for the car
        assert_eq!(car.components[0].shared_by, 1);
        assert_eq!(car.components[1].shared_by, 2);

        let cheetah = db.lookup(joaat("cheetah2"), &names).unwrap();
        assert_eq!(cheetah.parents, [joaat("car")]);
        assert_eq!(cheetah.own_count(), 1);
        assert_eq!(cheetah.components.len(), 2);
        let body = &cheetah.components[0];
        assert_eq!((body.record, body.depth, float(&body, 0)), (1, 0, Some(1630.0)));
        assert_eq!(body.fields[1].value, Value::Flags { raw: vec![1], names: vec![0x4444_4444] });
        let tyre = &cheetah.components[1];
        assert_eq!((tyre.owner, tyre.depth, float(&tyre, 0)), (joaat("car"), 1, Some(0.9)));
        assert!(tyre.title(&names).contains("inherited from car · shared by 2 entries"));
        assert!(cheetah.to_text(&names).contains("0xaaaa0001 = 1630"));
        assert!(db.lookup(joaat("coach"), &names).is_none());
    }

    fn drawn_text(shape: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
        match shape {
            egui::Shape::Text(text) => out.push((text.galley.text().to_string(), text.pos)),
            egui::Shape::Vec(list) => list.iter().for_each(|s| drawn_text(s, out)),
            _ => {}
        }
    }

    // draws the panel without a window clicking the texts that contain the given parts in turn
    // returns every text on screen after the last click and what was copied to the clipboard
    fn render_clicking(data: &VehicleData, names: &Names, clicks: &[&str]) -> (Vec<String>, Option<String>) {
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let mut copied = None;
        let mut frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))),
                events,
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| panel_ui(ui, data, names));
            });
            for command in output.platform_output.commands {
                if let egui::OutputCommand::CopyText(text) = command {
                    copied = Some(text);
                }
            }
            let mut texts = Vec::new();
            output.shapes.iter().for_each(|s| drawn_text(&s.shape, &mut texts));
            texts
        };
        frame(Vec::new());
        let mut shown = frame(Vec::new());
        for part in clicks {
            let pos = shown.iter().find(|(text, _)| text.contains(part)).expect("text to click").1 + egui::vec2(4.0, 4.0);
            let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            frame(vec![egui::Event::PointerMoved(pos)]);
            frame(vec![button(true)]);
            frame(vec![button(false)]);
            frame(Vec::new());
            shown = frame(Vec::new());
        }
        (shown.into_iter().map(|(text, _)| text).collect(), copied)
    }

    #[test]
    fn panel_lists_components_and_copies_their_values() {
        let db = VehicleDb::parse(&synthetic()).unwrap();
        let names = Names::new();
        let data = db.lookup(joaat("cheetah2"), &names).unwrap();

        let (closed, _) = render_clicking(&data, &names, &[]);
        assert_eq!(closed, ["Vehicle data (1 own · 1 inherited)"]);

        let (open, copied) = render_clicking(&data, &names, &["Vehicle data", "0x11111111 · record 1", "Copy values"]);
        for expected in [
            "Inherits: car",
            "0x11111111 · record 1 · own",
            "0x22222222 · record 0 · inherited from car · shared by 2 entries",
            "0xaaaa0001",
            "1630",
            "0x00000001 [0x44444444]",
        ] {
            assert!(open.iter().any(|text| text == expected), "{expected} missing from {open:?}");
        }
        assert_eq!(copied, Some(data.to_text(&names)));
    }

    #[test]
    fn labels_use_the_shared_names() {
        names::add(["fVehicleDataPanelTestName"]);
        names::add_built(["fVehicleDataPanelBuiltName"]);
        assert_eq!(schema_label(names::joaat_cs("fVehicleDataPanelTestName")), "fVehicleDataPanelTestName");
        assert_eq!(schema_label(names::joaat_cs("fVehicleDataPanelBuiltName")), "fVehicleDataPanelBuiltName?");
        assert_eq!(schema_label(0x0123_4567), "0x01234567");
        // row names are lower case hashes and scanned model names win over the table
        let models = Names::from([(joaat("cheetah2"), "cheetah2".to_string())]);
        assert_eq!(value_label(joaat("cheetah2"), &models), "cheetah2");
        assert_eq!(value_label(joaat("fVehicleDataPanelTestName"), &models), "fVehicleDataPanelTestName");
        assert_eq!(value_label(0x0123_4567, &models), "0x01234567");
    }

    #[test]
    fn rejects_files_without_a_name_table() {
        assert!(VehicleDb::parse(b"PSIN\0\0\0\x10pppppppp").is_err());
        assert!(VehicleDb::parse(b"not a psin file!").is_err());
    }

    #[test]
    fn decodes_half_floats() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x0000), 0.0);
    }

    fn game_folder() -> Option<PathBuf> {
        let levels = PathBuf::from(std::env::var_os("PERCYSTAR_LEVELS_RPF")?);
        let folder = levels.parent()?.to_path_buf();
        folder.join("data.rpf").is_file().then_some(folder)
    }

    #[test]
    fn finds_cheetah2_and_coach_in_game_data() {
        let Some(folder) = game_folder() else { return };
        let db = load_from_folder(&folder).expect("vehicle data in data.rpf");
        let names = Names::new();

        let cheetah = db.lookup(joaat("cheetah2"), &names).expect("cheetah2");
        assert_eq!(cheetah.own_count(), 8);
        let body = component(&cheetah, MASS_STRUCT);
        assert_eq!((body.depth, float(body, 0)), (0, Some(1630.0)));
        assert!(cheetah.parents.contains(&joaat("car")) && cheetah.parents.contains(&joaat("vehicle")));
        // the same record through the panel, mass is its first row and its field name is a known one
        crate::psin::seed_names();
        let cheetah = db.lookup(joaat("cheetah2"), &names).expect("cheetah2");
        let (shown, _) = render_clicking(&cheetah, &names, &["Vehicle data", "· record 89 · own"]);
        assert!(shown.contains(&"Vehicle data (8 own · 15 inherited)".to_string()));
        assert!(shown.contains(&"fMass".to_string()) && shown.contains(&"1630".to_string()), "{shown:?}");

        let coach = db.lookup(joaat("coach"), &names).expect("coach");
        assert_eq!(coach.own_count(), 7);
        assert_eq!(float(component(&coach, MASS_STRUCT), 0), Some(16500.0));
        assert_eq!(coach.parents[0], joaat("bus"));

        // police defines nothing itself and takes every record from its parent chain
        let police = db.lookup(joaat("police"), &names).expect("police");
        assert_eq!(police.own_count(), 0);
        let body = component(&police, MASS_STRUCT);
        assert_eq!((body.owner, body.depth), (joaat("stanierg2"), 1));
        assert!(body.shared_by > 1);
    }

    #[test]
    fn every_value_in_game_data_decodes_to_a_known_type() {
        let Some(folder) = game_folder() else { return };
        let db = load_from_folder(&folder).expect("vehicle data in data.rpf");
        fn check(value: &Value) {
            match value {
                Value::Unknown(ty) => panic!("undecoded type 0x{ty:08x}"),
                Value::Skipped => panic!("decode budget ran out"),
                Value::Struct(_, fields) => fields.iter().for_each(|f| check(&f.value)),
                Value::Array(items) => items.iter().for_each(check),
                Value::Map(entries) => entries.iter().for_each(|(k, v)| {
                    check(k);
                    check(v);
                }),
                Value::Float(v) => assert!(v.is_finite()),
                _ => {}
            }
        }
        let names = Names::new();
        let mut records = 0usize;
        for &hash in db.entries.keys() {
            let data = db.lookup(hash, &names).unwrap();
            for component in data.components.iter().filter(|c| c.depth == 0) {
                component.fields.iter().for_each(|f| check(&f.value));
                records += 1;
            }
        }
        // every component record belongs to exactly one entry
        assert_eq!(records, db.entries.values().map(|e| e.components.len()).sum::<usize>());
        assert!(db.entries.len() > 1000);
    }

    #[test]
    fn scan_finds_the_file_without_the_offset_hint() {
        let Some(folder) = game_folder() else { return };
        let mut file = File::open(folder.join("data.rpf")).unwrap();
        let db = locate(&mut file, None).expect("scan result");
        assert!(db.contains(joaat("cheetah2")));
        let hinted = locate(&mut file, Some(OFFSET_HINT)).expect("hinted result");
        assert_eq!(db.source_offset, hinted.source_offset);
    }
}
