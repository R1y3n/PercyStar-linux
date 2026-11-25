//! Effect/technique/pass navigation with context-preserving shader inspection.
use crate::dxbc::{self, CompactRootSignature};
use crate::sgd5::{self, Sgd5File, Sgd5ReflectionTable, STAGES};
use crate::sgd5_metadata as metadata;
use crate::{egui, ShaderViewMode};
use egui::{RichText, Ui};
use std::fmt::Write as _;

const METADATA_LABEL_COLOR: egui::Color32 = egui::Color32::from_rgb(120, 220, 255);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Selection {
    #[default]
    Group,
    SharedReflection,
    BindingTable(usize),
    Effect(usize),
    Technique {
        effect: usize,
        technique: usize,
    },
    Pass {
        effect: usize,
        technique: Option<usize>,
        pass: usize,
    },
    Shader {
        shader: usize,
        effect: Option<usize>,
        technique: Option<usize>,
        pass: Option<usize>,
    },
}

struct Node {
    id: usize,
    label: String,
    search: String,
    stages: u8,
    selection: Option<Selection>,
    children: Vec<Node>,
}

impl Node {
    fn new(label: String, selection: Option<Selection>, stages: u8, children: Vec<Node>) -> Self {
        Self {
            id: 0,
            search: label.to_lowercase(),
            label,
            selection,
            stages,
            children,
        }
    }

    fn assign_ids(&mut self, next: &mut usize) {
        self.id = *next;
        *next += 1;
        for child in &mut self.children {
            child.assign_ids(next);
        }
    }

    fn filter(
        &self,
        query: &str,
        stage: Option<usize>,
        inherited: bool,
        visible: &mut [bool],
    ) -> bool {
        let matches = inherited || self.search.contains(query);
        let mut descendant = false;
        for child in &self.children {
            descendant |= child.filter(query, stage, matches, visible);
        }
        let stage_matches = stage.is_none_or(|stage| self.stages & (1 << stage) != 0);
        visible[self.id] = stage_matches && (matches || descendant);
        visible[self.id]
    }

    fn show(
        &self,
        ui: &mut Ui,
        group: usize,
        visible: &[bool],
        searching: bool,
        selected: &mut Selection,
    ) {
        if !visible[self.id] {
            return;
        }
        let active = self.selection == Some(*selected);
        let label = RichText::new(&self.label).strong().color(if active {
            egui::Color32::from_rgb(255, 180, 70)
        } else if matches!(self.selection, Some(Selection::Shader { .. })) {
            crate::sgd5_stage_color(self.label.split_whitespace().next().unwrap_or(""))
        } else {
            ui.visuals().text_color()
        });
        if self.children.is_empty() {
            if let Some(selection) = self.selection {
                if ui.selectable_label(active, label).clicked() {
                    *selected = selection;
                }
            } else {
                ui.label(label);
            }
        } else {
            let response = egui::CollapsingHeader::new(label)
                .id_salt(("sgd5_tree", group, self.id))
                .default_open(self.label.starts_with("Effects ("))
                .open(searching.then_some(true))
                .show(ui, |ui| {
                    for child in &self.children {
                        child.show(ui, group, visible, searching, selected);
                    }
                });
            if response.header_response.clicked() {
                if let Some(selection) = self.selection {
                    *selected = selection;
                }
            }
        }
    }
}

struct Navigation {
    nodes: Vec<Node>,
    visible: Vec<bool>,
    filter_key: Option<(String, Option<usize>)>,
}

impl Navigation {
    fn shader(
        file: &Sgd5File,
        shader: usize,
        effect: Option<usize>,
        technique: Option<usize>,
        pass: Option<usize>,
    ) -> Node {
        let entry = &file.shaders[shader];
        let stage = STAGES.iter().position(|s| s.0 == entry.stage).unwrap_or(0);
        let mut node = Node::new(
            format!("{} #{} · {}", entry.stage, entry.index, entry.name),
            Some(Selection::Shader {
                shader,
                effect,
                technique,
                pass,
            }),
            1 << stage,
            Vec::new(),
        );
        node.search.push_str(&format!(" {:016x}", entry.sga_hash));
        node
    }

    fn pass(
        file: &Sgd5File,
        effect_index: usize,
        technique: Option<usize>,
        pass_index: usize,
    ) -> Node {
        let effect = &file.effects[effect_index];
        let pass = &effect.metadata.passes[pass_index];
        let mut stages = 0;
        let mut children = Vec::new();
        for stage in 0..6 {
            if let Some(shader) = file.pass_shader(effect, pass, stage) {
                stages |= 1 << stage;
                if let Some(position) = file.shader_position(shader.stage, shader.index) {
                    children.push(Self::shader(
                        file,
                        position,
                        Some(effect_index),
                        technique,
                        Some(pass_index),
                    ));
                }
            }
        }
        let summary = STAGES
            .iter()
            .enumerate()
            .filter(|(s, _)| stages & (1 << s) != 0)
            .map(|(_, stage)| stage.0)
            .collect::<Vec<_>>()
            .join(" + ");
        Node::new(
            format!(
                "Pass {pass_index} · {}",
                if summary.is_empty() {
                    "state only"
                } else {
                    &summary
                }
            ),
            Some(Selection::Pass {
                effect: effect_index,
                technique,
                pass: pass_index,
            }),
            stages,
            children,
        )
    }

    fn new(file: &Sgd5File) -> Self {
        let mut effects = Vec::new();
        for (effect_index, effect) in file.effects.iter().enumerate() {
            let mut children = Vec::new();
            let mut covered = vec![false; effect.metadata.passes.len()];
            let mut stages = 0;
            for (technique_index, technique) in effect.metadata.techniques.iter().enumerate() {
                let start = usize::from(technique.first_pass);
                let end = start + usize::from(technique.pass_count);
                let passes = (start..end.min(covered.len()))
                    .map(|pass| {
                        covered[pass] = true;
                        Self::pass(file, effect_index, Some(technique_index), pass)
                    })
                    .collect::<Vec<_>>();
                let mask = passes.iter().fold(0, |mask, node| mask | node.stages);
                stages |= mask;
                children.push(Node::new(
                    format!(
                        "{} · {} passes",
                        technique.name.as_deref().unwrap_or("Unnamed technique"),
                        technique.pass_count
                    ),
                    Some(Selection::Technique {
                        effect: effect_index,
                        technique: technique_index,
                    }),
                    mask,
                    passes,
                ));
            }
            let other = covered
                .iter()
                .enumerate()
                .filter(|(_, covered)| !**covered)
                .map(|(pass, _)| Self::pass(file, effect_index, None, pass))
                .collect::<Vec<_>>();
            if !other.is_empty() {
                let mask = other.iter().fold(0, |mask, node| mask | node.stages);
                stages |= mask;
                children.push(Node::new(
                    "Passes outside techniques".into(),
                    None,
                    mask,
                    other,
                ));
            }
            // Keep every declared variant reachable, including variants without a technique/pass use.
            let mut variants = Vec::new();
            for (stage, indices) in effect.stage_indices.iter().enumerate() {
                for index in indices {
                    if let Some(position) = file.shader_position(STAGES[stage].0, *index) {
                        stages |= 1 << stage;
                        variants.push(Self::shader(file, position, Some(effect_index), None, None));
                    }
                }
            }
            if !variants.is_empty() {
                let mask = variants.iter().fold(0, |mask, node| mask | node.stages);
                children.push(Node::new(
                    format!("Declared variants ({})", variants.len()),
                    None,
                    mask,
                    variants,
                ));
            }
            effects.push(Node::new(
                format!(
                    "{} · {} techniques · {} passes",
                    effect.name,
                    effect.metadata.techniques.len(),
                    effect.metadata.passes.len()
                ),
                Some(Selection::Effect(effect_index)),
                stages,
                children,
            ));
        }
        let mut nodes = vec![
            Node::new(
                "Archive overview".into(),
                Some(Selection::Group),
                0x3f,
                Vec::new(),
            ),
            Node::new(format!("Effects ({})", effects.len()), None, 0x3f, effects),
            Node::new(
                "Shared reflection".into(),
                Some(Selection::SharedReflection),
                0x3f,
                Vec::new(),
            ),
        ];
        let tables = file
            .binding_tables
            .iter()
            .enumerate()
            .map(|(index, table)| {
                Node::new(
                    format!(
                        "Table {index} · 0x{:08x} · {} entries",
                        table.header.table_hash,
                        table.entries.len()
                    ),
                    Some(Selection::BindingTable(index)),
                    0x3f,
                    Vec::new(),
                )
            })
            .collect();
        nodes.push(Node::new(
            format!("Shared binding tables ({})", file.binding_tables.len()),
            None,
            0x3f,
            tables,
        ));
        let library = file
            .shaders
            .iter()
            .enumerate()
            .map(|(index, _)| Self::shader(file, index, None, None, None))
            .collect();
        nodes.push(Node::new(
            format!("All shaders ({})", file.shaders.len()),
            None,
            0x3f,
            library,
        ));
        let mut count = 0;
        for node in &mut nodes {
            node.assign_ids(&mut count);
        }
        Self {
            nodes,
            visible: vec![true; count],
            filter_key: None,
        }
    }

    fn filter(&mut self, query: &str, stage: Option<usize>) {
        let key = (query.to_lowercase(), stage);
        if self.filter_key.as_ref() != Some(&key) {
            for node in &self.nodes {
                node.filter(&key.0, stage, false, &mut self.visible);
            }
            self.filter_key = Some(key);
        }
    }
}

struct PreparedShader {
    shader: usize,
    blob: Result<Vec<u8>, String>,
    programs: Result<Vec<dxbc::FssmShader>, String>,
    native_metadata: Vec<Result<dxbc::FssmNativeMetadata, String>>,
    native_mappings: Vec<String>,
    root: Option<CompactRootSignature>,
    metadata: String,
}

#[derive(Default)]
pub struct Sgd5Browser {
    group: usize,
    selection: Selection,
    search: String,
    stage: Option<usize>,
    navigation: Option<Navigation>,
    prepared: Option<PreparedShader>,
    text: Option<(Selection, ShaderViewMode, usize, String)>,
    shader_tab: usize,
    program_index: usize,
    status: String,
}

impl Sgd5Browser {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn show_navigation(&mut self, ui: &mut Ui, files: &[Sgd5File]) {
        if files.is_empty() {
            ui.label("No SGD5 groups loaded.");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for index in 0..files.len() {
                if ui
                    .selectable_label(self.group == index, format!("Group {index}"))
                    .clicked()
                    && self.group != index
                {
                    self.group = index;
                    self.selection = Selection::Group;
                    self.navigation = None;
                    self.prepared = None;
                    self.text = None;
                }
            }
        });
        let file = &files[self.group.min(files.len() - 1)];
        ui.label(format!(
            "{} effects · {} shaders",
            file.effects.len(),
            file.shaders.len()
        ));
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.stage, None, "All stages");
            for (index, (stage, _)) in STAGES.iter().enumerate() {
                ui.selectable_value(&mut self.stage, Some(index), *stage);
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Find effect, technique, shader, or hash…")
                .desired_width(f32::INFINITY),
        );
        let navigation = self.navigation.get_or_insert_with(|| Navigation::new(file));
        navigation.filter(self.search.trim(), self.stage);
        egui::ScrollArea::vertical()
            .id_salt("sgd5_navigation")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for node in &navigation.nodes {
                    node.show(
                        ui,
                        self.group,
                        &navigation.visible,
                        !self.search.trim().is_empty(),
                        &mut self.selection,
                    );
                }
            });
    }

    pub fn show_content(&mut self, ui: &mut Ui, files: &[Sgd5File], mode: &mut ShaderViewMode) {
        let Some(file) = files.get(self.group) else {
            ui.label("Load an SGD5 archive to inspect its effects.");
            return;
        };
        if let Selection::Shader {
            shader,
            effect,
            pass,
            ..
        } = self.selection
        {
            self.show_shader(ui, file, shader, effect, pass, mode);
            return;
        }
        self.breadcrumbs(ui, file);
        egui::ScrollArea::both().id_salt(("sgd5_inspector", self.selection)).auto_shrink([false, false]).show(ui, |ui| {
            match self.selection {
                Selection::Group => {
                    ui.heading(RichText::new("SGD5 archive").color(METADATA_LABEL_COLOR));
                    info(ui, "Group hash", format!("0x{:016x}", file.group_hash));
                    info(ui, "Effects / shaders", format!("{} / {}", file.effects.len(), file.shaders.len()));
                    info(ui, "Shared binding tables", file.binding_tables.len());
                    info(ui, "Trailer flags", format!("0x{:08x}", file.trailer_flags));
                    ui.label("Select an effect, then a technique and pass to inspect its shader and parameter bindings.");
                    reflection(ui, file, &file.shared_reflection, "Shared reflection", false);
                }
                Selection::SharedReflection => reflection(ui, file, &file.shared_reflection, "Archive shared reflection", false),
                Selection::BindingTable(index) => {
                    if let Some(table) = file.binding_tables.get(index) { binding_table(ui, file, table); }
                }
                Selection::Effect(index) => effect_overview(ui, file, index),
                Selection::Technique { effect, technique } => {
                    if let Some(entry) = file.effects.get(effect).and_then(|e| e.metadata.techniques.get(technique)) {
                        ui.heading(RichText::new(entry.name.as_deref().unwrap_or("Unnamed technique")).color(METADATA_LABEL_COLOR));
                        info(ui, "First pass", entry.first_pass);
                        info(ui, "Pass count", entry.pass_count);
                        for pass in usize::from(entry.first_pass)..usize::from(entry.first_pass) + usize::from(entry.pass_count) {
                            if ui.button(format!("Inspect pass {pass}")).clicked() { self.selection = Selection::Pass { effect, technique: Some(technique), pass }; }
                        }
                    }
                }
                Selection::Pass { effect, pass, .. } => {
                    pass_overview(ui, file, effect, pass);
                    if self.text.as_ref().is_none_or(|(selection, _, tab, _)| *selection != self.selection || *tab != 3) {
                        self.text = Some((self.selection, *mode, 3, metadata::format_pass_metadata(file, effect, pass)));
                    }
                    if let Some((_, _, _, text)) = &self.text {
                        egui::CollapsingHeader::new("Verified parameter mappings").default_open(true).show(ui, |ui| { text_view(ui, text); });
                    }
                }
                Selection::Shader { .. } => {}
            }
        });
    }

    fn breadcrumbs(&mut self, ui: &mut Ui, file: &Sgd5File) {
        let (effect, technique, pass) = match self.selection {
            Selection::Effect(e) => (Some(e), None, None),
            Selection::Technique { effect, technique } => (Some(effect), Some(technique), None),
            Selection::Pass {
                effect,
                technique,
                pass,
            } => (Some(effect), technique, Some(pass)),
            Selection::Shader {
                effect,
                technique,
                pass,
                ..
            } => (effect, technique, pass),
            _ => (None, None, None),
        };
        ui.horizontal_wrapped(|ui| {
            if ui.link(format!("Group {}", self.group)).clicked() {
                self.selection = Selection::Group;
            }
            if let Some(effect) = effect {
                ui.label("›");
                if ui.link(&file.effects[effect].name).clicked() {
                    self.selection = Selection::Effect(effect);
                }
                if let Some(technique) = technique {
                    ui.label("›");
                    if ui
                        .link(
                            file.effects[effect].metadata.techniques[technique]
                                .name
                                .as_deref()
                                .unwrap_or("Technique"),
                        )
                        .clicked()
                    {
                        self.selection = Selection::Technique { effect, technique };
                    }
                }
                if let Some(pass) = pass {
                    ui.label("›");
                    if ui.link(format!("Pass {pass}")).clicked() {
                        self.selection = Selection::Pass {
                            effect,
                            technique,
                            pass,
                        };
                    }
                }
            }
        });
    }

    fn show_shader(
        &mut self,
        ui: &mut Ui,
        file: &Sgd5File,
        shader_index: usize,
        effect: Option<usize>,
        pass: Option<usize>,
        mode: &mut ShaderViewMode,
    ) {
        self.breadcrumbs(ui, file);
        let Some(shader) = file.shaders.get(shader_index) else {
            return;
        };
        ui.heading(
            RichText::new(format!(
                "{} #{} · {}",
                shader.stage, shader.index, shader.name
            ))
            .color(METADATA_LABEL_COLOR),
        );
        if effect.is_none() {
            let effects = file.effect_names_for_shader(shader.stage, shader.index);
            if !effects.is_empty() {
                ui.label(format!("Declared by: {}", effects.join(", ")));
            }
        }
        if self
            .prepared
            .as_ref()
            .is_none_or(|prepared| prepared.shader != shader_index)
        {
            let blob = sgd5::decompress_shader(file, shader).map_err(|e| e.to_string());
            let programs = blob
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|blob| dxbc::extract_fssm_programs(blob));
            let native_metadata: Vec<Result<dxbc::FssmNativeMetadata, String>> = programs
                .as_ref()
                .map(|programs| {
                    programs
                        .iter()
                        .map(dxbc::FssmShader::native_metadata)
                        .collect()
                })
                .unwrap_or_default();
            let root = blob
                .as_ref()
                .ok()
                .and_then(|blob| dxbc::extract_compact_root_signature(blob).ok());
            let native_mappings = native_metadata
                .iter()
                .map(|native: &Result<dxbc::FssmNativeMetadata, String>| {
                    root.as_ref()
                        .zip(native.as_ref().ok())
                        .map(|(root, native)| {
                            metadata::format_native_parameter_mapping(file, shader, root, native)
                        })
                        .unwrap_or_default()
                })
                .collect();
            let default_program = programs
                .as_ref()
                .ok()
                .and_then(|programs| {
                    programs.iter().position(|program| {
                        matches!(
                            (shader.stage, program.hardware_stage()),
                            ("VS", dxbc::NativeStage::Vs)
                                | ("PS", dxbc::NativeStage::Ps)
                                | ("GS", dxbc::NativeStage::Gs)
                                | ("CS", dxbc::NativeStage::Compute)
                        )
                    })
                })
                .unwrap_or(0);
            self.prepared = Some(PreparedShader {
                shader: shader_index,
                blob,
                programs,
                native_metadata,
                native_mappings,
                root,
                metadata: metadata::format_shader_metadata(file, shader),
            });
            self.text = None;
            self.program_index = default_program;
        }
        if let Ok(programs) = &self.prepared.as_ref().unwrap().programs {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Native program")
                        .strong()
                        .color(METADATA_LABEL_COLOR),
                );
                for (index, program) in programs.iter().enumerate() {
                    if ui
                        .selectable_value(
                            &mut self.program_index,
                            index,
                            format!("{} #{}", program.hardware_stage().tag(), index),
                        )
                        .changed()
                    {
                        self.text = None;
                    }
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            for (index, label) in ["Shader", "Parameters", "Metadata"].into_iter().enumerate() {
                ui.selectable_value(&mut self.shader_tab, index, label);
            }
            if self.shader_tab == 0 {
                ui.separator();
                for (value, label) in [
                    (ShaderViewMode::Asm, "GFX10.3 ASM"),
                    (ShaderViewMode::Hlsl, "Lifted HLSL"),
                    (ShaderViewMode::SpirvAbi, "SPIR-V ABI"),
                ] {
                    ui.selectable_value(mode, value, label);
                }
            }
        });
        let prepared = self.prepared.as_ref().unwrap();
        let program = prepared
            .programs
            .as_ref()
            .ok()
            .and_then(|programs| programs.get(self.program_index));
        let native = prepared
            .native_metadata
            .get(self.program_index)
            .and_then(|result| result.as_ref().ok());
        let context = || {
            effect
                .zip(pass)
                .map(|(effect, pass)| metadata::format_pass_metadata(file, effect, pass))
                .unwrap_or_default()
        };
        if self.shader_tab != 0 {
            if self.text.as_ref().is_none_or(|(selection, _, tab, _)| {
                *selection != self.selection || *tab != self.shader_tab
            }) {
                let text = if self.shader_tab == 1 {
                    let mut text =
                        metadata::format_parameter_mapping(file, shader, prepared.root.as_ref());
                    if let Some((root, native)) = prepared.root.as_ref().zip(native) {
                        text.push_str(&metadata::format_native_parameter_mapping(
                            file, shader, root, native,
                        ));
                    }
                    text + &context()
                } else {
                    prepared.metadata.clone()
                };
                self.text = Some((self.selection, *mode, self.shader_tab, text));
            }
        } else if program.is_some() {
            if self
                .text
                .as_ref()
                .is_none_or(|(selection, cached_mode, tab, _)| {
                    *selection != self.selection || *cached_mode != *mode || *tab != 0
                })
            {
                let mut text = shader_text(prepared, shader, *mode, self.program_index);
                let context = context();
                if !context.is_empty() {
                    text = metadata::prepend_comments(
                        &context,
                        if *mode == ShaderViewMode::Asm {
                            ";"
                        } else {
                            "//"
                        },
                        text,
                    );
                }
                self.text = Some((self.selection, *mode, 0, text));
            }
        } else {
            // Metadata remains reachable when a compiled container has no supported FSSM program.
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                prepared
                    .programs
                    .as_ref()
                    .err()
                    .map(String::as_str)
                    .unwrap_or("Native program unavailable"),
            );
            return;
        }
        let stem = format!(
            "sgd5_g{}_{}_{}",
            self.group,
            shader.stage.to_lowercase(),
            shader.index
        );
        let native_stem = format!("{stem}_native{}", self.program_index);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Export DXBC").clicked() {
                if let Ok(blob) = &prepared.blob {
                    self.status = save(&format!("{stem}.dxbc"), blob);
                }
            }
            if ui.button("Export displayed text").clicked() {
                if let Some((_, _, _, text)) = &self.text {
                    self.status = save(&format!("{native_stem}.txt"), text.as_bytes());
                }
            }
            if let Some(program) = program {
                for (label, extension, output_mode) in [
                    ("Export ASM", "asm", ShaderViewMode::Asm),
                    ("Export HLSL", "hlsl", ShaderViewMode::Hlsl),
                ] {
                    if ui.button(label).clicked() {
                        let text = metadata::prepend_comments(
                            &context(),
                            if output_mode == ShaderViewMode::Asm {
                                ";"
                            } else {
                                "//"
                            },
                            shader_text(prepared, shader, output_mode, self.program_index),
                        );
                        self.status = save(&format!("{native_stem}.{extension}"), text.as_bytes());
                    }
                }
                if ui.button("Export SPIR-V").clicked() {
                    let entry = crate::sgd5_ps09_entry(shader, program);
                    self.status = save(
                        &format!("{native_stem}.spv"),
                        &crate::lift_to_spirv(&entry, program.code()),
                    );
                }
            }
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        egui::ScrollArea::both()
            .id_salt(("sgd5_shader", shader_index, self.shader_tab))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some((_, _, _, text)) = &self.text {
                    text_view(ui, text);
                }
            });
    }
}

fn info(ui: &mut Ui, label: &str, value: impl std::fmt::Display) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong().color(METADATA_LABEL_COLOR));
        ui.label(value.to_string());
    });
}
fn shader_text(
    prepared: &PreparedShader,
    shader: &sgd5::Sgd5Shader,
    mode: ShaderViewMode,
    program_index: usize,
) -> String {
    let Some(program) = prepared
        .programs
        .as_ref()
        .ok()
        .and_then(|programs| programs.get(program_index))
    else {
        return prepared.metadata.clone();
    };
    let entry = crate::sgd5_ps09_entry(shader, program);
    let mapping = prepared
        .native_mappings
        .get(program_index)
        .map(String::as_str)
        .unwrap_or_default();
    crate::sgd5_shader_text(
        &format!("{}{mapping}", prepared.metadata),
        &entry,
        program.code(),
        mode,
    )
}
fn text_view(ui: &mut Ui, text: &str) {
    ui.add(
        egui::TextEdit::multiline(&mut &*text)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY),
    );
}
fn save(name: &str, bytes: &[u8]) -> String {
    let Some(path) = rfd::FileDialog::new().set_file_name(name).save_file() else {
        return String::new();
    };
    match std::fs::write(&path, bytes) {
        Ok(()) => format!("Saved {}", path.display()),
        Err(error) => format!("Export failed: {error}"),
    }
}

fn effect_overview(ui: &mut Ui, file: &Sgd5File, index: usize) {
    let Some(effect) = file.effects.get(index) else {
        return;
    };
    ui.heading(RichText::new(&effect.name).color(METADATA_LABEL_COLOR));
    info(
        ui,
        "Runtime data allocation",
        format!("{} bytes", effect.data_buffer_size),
    );
    info(
        ui,
        "Techniques / passes",
        format!(
            "{} / {}",
            effect.metadata.techniques.len(),
            effect.metadata.passes.len()
        ),
    );
    info(
        ui,
        "Binding layouts / texture handles",
        format!(
            "{} / {}",
            effect.binding_list.entries.len(),
            effect.binding_list.texture_handle_count
        ),
    );
    info(
        ui,
        "Serialized payload",
        format!("0x{:x}..0x{:x}", effect.payload_offset, effect.payload_end),
    );
    info(
        ui,
        "Declared stage variants",
        STAGES
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}: {}", s.0, effect.stage_indices[i].len()))
            .collect::<Vec<_>>()
            .join(" · "),
    );
    let header = &effect.metadata.header;
    info(
        ui,
        "Aggregate vertex mask",
        format!("0x{:016x}", header.vertex_inputs.attribute_mask),
    );
    info(
        ui,
        "Additional vertex mask (purpose unresolved)",
        format!("0x{:016x}", header.attribute_mask_48),
    );
    info(
        ui,
        "System inputs",
        format!(
            "VertexID={} · InstanceID={}",
            header.vertex_inputs.uses_vertex_id(),
            header.vertex_inputs.uses_instance_id()
        ),
    );
    egui::CollapsingHeader::new("Vertex attributes (effect formats provisional)").show(ui, |ui| {
        for slot in 0..52 {
            if header.vertex_inputs.attribute_mask & (1 << slot) == 0 {
                continue;
            }
            let (semantic, index) = sgd5::Sgd5VertexInputMetadata::semantic(slot).unwrap();
            info(
                ui,
                &format!("{semantic}{index}"),
                format!(
                    "raw=0x{:02x} · DXGI candidate={:?}",
                    header.vertex_inputs.attribute_formats[slot],
                    header.vertex_inputs.dxgi_format(slot)
                ),
            );
        }
        info(
            ui,
            "Raw flags / reserved",
            format!(
                "0x{:016x} / 0x{:08x}",
                header.vertex_inputs.shader_flags, header.vertex_inputs.reserved_44
            ),
        );
    });
    reflection(
        ui,
        file,
        &effect.main_reflection,
        "Material parameters",
        true,
    );
    reflection(
        ui,
        file,
        &effect.property_reflection,
        "Effect properties (proplst)",
        false,
    );
    match &effect.shared_reflection_source {
        sgd5::Sgd5ReflectionSource::Shared => {
            ui.label("Common definitions reuse the archive shared reflection table.");
        }
        sgd5::Sgd5ReflectionSource::Inline(table) => {
            reflection(ui, file, table, "Inline common definitions", false)
        }
        sgd5::Sgd5ReflectionSource::Absent => {
            ui.label("No common reflection source.");
        }
    }
    egui::CollapsingHeader::new("Binding layouts").show(ui, |ui| {
        for (index, _) in effect.binding_list.entries.iter().enumerate() {
            egui::CollapsingHeader::new(format!("Layout {index}"))
                .id_salt(("layout", index))
                .show(ui, |ui| {
                    if let Some(table) = file.binding_table_at(effect, index) {
                        binding_table(ui, file, table);
                    }
                });
        }
    });
    egui::CollapsingHeader::new("Render and sampler state library").show(ui, |ui| {
        for (index, state) in effect.metadata.rasterizer_states.iter().enumerate() {
            egui::CollapsingHeader::new(format!("Rasterizer {index}")).show(ui, |ui| {
                rasterizer(ui, state);
            });
        }
        for (index, state) in effect.metadata.depth_stencil_states.iter().enumerate() {
            egui::CollapsingHeader::new(format!("Depth/stencil {index}")).show(ui, |ui| {
                depth_stencil(ui, state);
            });
        }
        for (index, state) in effect.metadata.blend_states.iter().enumerate() {
            egui::CollapsingHeader::new(format!("Blend {index}")).show(ui, |ui| {
                blend(ui, state);
            });
        }
        for (index, state) in effect.metadata.sampler_states.iter().enumerate() {
            egui::CollapsingHeader::new(format!("Sampler {index}")).show(ui, |ui| {
                let desc = state.descriptor();
                info(
                    ui,
                    "Preset / address U,V,W",
                    format!("{} / {:?}", desc.preset_index, desc.address_modes),
                );
                info(
                    ui,
                    "LOD bias / min / max",
                    format!(
                        "{} / {} / {}",
                        desc.mip_lod_bias, desc.min_lod, desc.max_lod
                    ),
                );
                info(ui, "Border color", format!("{:?}", desc.border_color));
                info(
                    ui,
                    "Filter / comparison bits / max anisotropy",
                    format!(
                        "0x{:04x} / 0x{:02x} / {}",
                        desc.filter_bits, desc.comparison_bits, desc.max_anisotropy
                    ),
                );
                ui.monospace(format!("{:02x?}", state.data));
            });
        }
    });
    egui::CollapsingHeader::new("Raw effect lookup words").show(ui, |ui| {
        ui.monospace(format!("{:08x?}", effect.metadata.pass_lookup_words));
    });
    egui::CollapsingHeader::new(format!(
        "Effect strings ({} bytes)",
        effect.string_pool.len()
    ))
    .show(ui, |ui| {
        for string in &effect.strings {
            ui.label(string);
        }
    });
}

fn pass_overview(ui: &mut Ui, file: &Sgd5File, effect_index: usize, pass_index: usize) {
    let Some(effect) = file.effects.get(effect_index) else {
        return;
    };
    let Some(pass) = effect.metadata.passes.get(pass_index) else {
        return;
    };
    ui.heading(RichText::new(format!("Pass {pass_index}")).color(METADATA_LABEL_COLOR));
    for stage in 0..6 {
        if let Some(shader) = file.pass_shader(effect, pass, stage) {
            info(
                ui,
                shader.stage,
                format!("#{} · {}", shader.index, shader.name),
            );
        }
    }
    if let Some(compute) = file.pass_shader(effect, pass, 5) {
        info(
            ui,
            "Compute layouts space0..3",
            format!(
                "{:?} (255 absent)",
                compute.compute_binding_selectors().unwrap()
            ),
        );
    } else {
        for (space, selector) in pass.binding_table_indices.iter().enumerate() {
            info(
                ui,
                &format!("Graphics layout space{space}"),
                if *selector == 255 {
                    "absent".into()
                } else {
                    selector.to_string()
                },
            );
        }
    }
    info(ui, "State flags", format!("0x{:02x}", pass.state_flags));
    if let Some(reference) = pass.selected_stencil_reference() {
        info(ui, "Stencil reference", reference);
    }
    if pass.state_flags & 1 != 0 {
        if let Some(state) = effect
            .metadata
            .rasterizer_states
            .get(pass.rasterizer_state_index as usize)
        {
            egui::CollapsingHeader::new(format!("Rasterizer {}", pass.rasterizer_state_index))
                .show(ui, |ui| {
                    rasterizer(ui, state);
                });
        }
    }
    if pass.state_flags & 2 != 0 {
        if let Some(state) = effect
            .metadata
            .depth_stencil_states
            .get(pass.depth_stencil_state_index as usize)
        {
            egui::CollapsingHeader::new(format!(
                "Depth/stencil {}",
                pass.depth_stencil_state_index
            ))
            .show(ui, |ui| {
                depth_stencil(ui, state);
            });
        }
    }
    if pass.state_flags & 4 != 0 {
        if let Some(state) = effect
            .metadata
            .blend_states
            .get(pass.blend_state_index as usize)
        {
            egui::CollapsingHeader::new(format!(
                "Blend {} (all eight targets)",
                pass.blend_state_index
            ))
            .show(ui, |ui| {
                blend(ui, state);
            });
        }
    }
    egui::CollapsingHeader::new("Raw pass fields").show(ui, |ui| {
        text_view(ui, &format!("{pass:#?}"));
    });
}

fn reflection(
    ui: &mut Ui,
    file: &Sgd5File,
    table: &Sgd5ReflectionTable,
    label: &str,
    material: bool,
) {
    egui::CollapsingHeader::new(format!("{label} · {} resources · {} members · {} properties", table.resources.len(), table.members.len(), table.auxiliary_records.len())).id_salt((label, table.offset)).show(ui, |ui| {
        for record in &table.resources {
            let name = record.name.as_deref().or_else(|| file.resolve_name(record.name_hash)).map(str::to_string).unwrap_or_else(|| format!("0x{:08x}", record.name_hash));
            egui::CollapsingHeader::new(format!("{name} · {}", metadata::resource_type_name(record.resource_type))).id_salt(record.offset).show(ui, |ui| {
                info(ui, "Parameter identity", format!("0x{:016x}", record.parameter_key()));
                info(ui, "Type / flags", format!("0x{:04x} / 0x{:02x}", record.resource_type, record.flags));
                let handle = material && metadata::resource_kind(record) == dxbc::DescriptorKind::Srv && record.flags & 0xc0 == 0x80;
                info(ui, if handle { "Texture-handle slot" } else { "Metadata slot" }, if handle && record.slot == 255 { "absent".into() } else { record.slot.to_string() });
                if record.cbuffer_size != 0 { info(ui, "CBV byte size", record.cbuffer_size); }
                let start = record.offset + usize::from(record.member_data_offset);
                for member in table.members.iter().filter(|m| m.offset >= start && m.offset < start + usize::from(record.member_count) * 24) {
                    let name = member.name.as_deref().unwrap_or("Unresolved member");
                    info(ui, name, format!("hash=0x{:08x} · type={} · array={} · byte offset={} · bytes={:?} · default +0x{:x}", member.name_hash, member.value_type, member.array_size, member.byte_offset, member.byte_size(), member.default_value_offset));
                    if member.default_value_offset != 0 {
                        let start = member.offset + usize::from(member.default_value_offset);
                        let end = (start + member.byte_size().unwrap_or(4).min(16)).min(table.offset + 4 + table.size as usize);
                        if let Some(bytes) = file.data.get(start..end) { ui.monospace(format!("Default bytes: {bytes:02x?}")); }
                    }
                }
            });
        }
        if !table.auxiliary_records.is_empty() {
            egui::CollapsingHeader::new("Typed auxiliary properties").default_open(label.contains("proplst")).show(ui, |ui| {
                for record in &table.auxiliary_records {
                    let name = file.hash_names.iter().find(|(hash, _)| **hash & 0x3fff_ffff == record.name_hash30()).map(|(_, name)| name.clone()).unwrap_or_else(|| format!("hash30=0x{:08x}", record.name_hash30()));
                    info(ui, &name, metadata::format_auxiliary_value(file, table, record));
                }
            });
        }
        egui::CollapsingHeader::new("Raw reflection records").show(ui, |ui| { text_view(ui, &format!("{table:#?}")); });
    });
}

fn binding_table(ui: &mut Ui, file: &Sgd5File, table: &sgd5::Sgd5BindingTable) {
    info(
        ui,
        "Table hash / signature",
        format!("0x{:08x} / 0x{:016x}", table.header.table_hash, table.hash),
    );
    info(
        ui,
        "SRV / UAV / sampler / CBV",
        format!(
            "{} / {} / {} / {}",
            table.header.srv_count,
            table.header.uav_count,
            table.header.sampler_count,
            table.header.cbv_count
        ),
    );
    let mut text = String::from("Descriptor index  Type      Resource identity\n");
    for entry in &table.entries {
        let _ = writeln!(
            text,
            "{:>16}  0x{:04x}    0x{:08x} {} (raw index=0x{:04x})",
            entry.descriptor_index(),
            entry.binding_type,
            entry.name_hash,
            entry
                .name
                .as_deref()
                .or_else(|| file.resolve_name(entry.name_hash))
                .unwrap_or("unresolved"),
            entry.binding_index
        );
    }
    text_view(ui, &text);
}

fn rasterizer(ui: &mut Ui, state: &sgd5::Sgd5RasterizerState) {
    info(
        ui,
        "Fill mode",
        match state.fill_mode {
            2 => "Wireframe".into(),
            3 => "Solid".into(),
            value => value.to_string(),
        },
    );
    info(
        ui,
        "Cull mode",
        match state.cull_mode {
            1 => "None".into(),
            2 => "Front".into(),
            3 => "Back".into(),
            value => value.to_string(),
        },
    );
    info(ui, "Front counter-clockwise", state.front_counter_clockwise);
    info(
        ui,
        "Depth bias / clamp / slope",
        format!(
            "{} / {} / {}",
            state.depth_bias, state.depth_bias_clamp, state.slope_scaled_depth_bias
        ),
    );
    info(
        ui,
        "Depth clip / multisample",
        format!("{} / {}", state.depth_clip_enable, state.multisample_enable),
    );
    info(
        ui,
        "Flags / unknown byte",
        format!("0x{:02x} / 0x{:02x}", state.flags, state.unknown_0d),
    );
    info(
        ui,
        "Antialiased lines / conservative rasterization",
        format!(
            "{} / {}",
            state.antialiased_line_enable(),
            state.conservative_rasterization()
        ),
    );
}

fn depth_stencil(ui: &mut Ui, state: &sgd5::Sgd5DepthStencilState) {
    info(
        ui,
        "Depth enable / write mask / comparison",
        format!(
            "{} / {} / {}",
            state.depth_enable, state.depth_write_mask, state.depth_comparison_func
        ),
    );
    info(
        ui,
        "Stencil enable / read mask / write mask",
        format!(
            "{} / 0x{:02x} / 0x{:02x}",
            state.stencil_enable, state.stencil_read_mask, state.stencil_write_mask
        ),
    );
    for (name, face) in [
        ("Front face", &state.front_face),
        ("Back face", &state.back_face),
    ] {
        info(
            ui,
            name,
            format!(
                "fail={} · depth fail={} · pass={} · comparison={}",
                face.fail_op, face.depth_fail_op, face.pass_op, face.comparison_func
            ),
        );
    }
    info(
        ui,
        "Depth bounds test / unknown fields",
        format!(
            "0x{:02x} / 0x{:02x} / {:02x?}",
            state.depth_bounds_test_enable, state.unknown_0e, state.unknown_tail
        ),
    );
}

fn blend(ui: &mut Ui, state: &sgd5::Sgd5BlendState) {
    info(
        ui,
        "Alpha to coverage / independent blend",
        format!(
            "{} / {}",
            state.alpha_to_coverage_enable, state.independent_blend_enable
        ),
    );
    info(
        ui,
        "Unknown header word",
        format!("0x{:04x}", state.unknown_02),
    );
    for (index, target) in state.render_targets.iter().enumerate() {
        info(
            ui,
            &format!("Target {index}"),
            format!(
                "enable={} · color={}/{}/{} · alpha={}/{}/{} · write mask=0x{:x}",
                target.blend_enable,
                target.source_blend,
                target.destination_blend,
                target.blend_op,
                target.source_blend_alpha,
                target.destination_blend_alpha,
                target.blend_op_alpha,
                target.write_mask
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fixture() -> Sgd5File {
        let shader = |stage| sgd5::Sgd5Shader {
            stage,
            index: 0,
            name: format!("{stage}_Shared"),
            shader_version: 66,
            archive: sgd5::Sgd5ShaderArchive::default(),
            sga_hash: 0x1234,
            shader_metadata: [0; 0x90],
            reflection: Sgd5ReflectionTable::default(),
        };
        let make_effect = |name: &str, pass_count: u8, pixel_variant: bool| {
            let pass = sgd5::Sgd5Pass {
                offset: 0,
                shader_slots: [1, 0, 0, 0, 0, 0],
                rasterizer_state_index: 0,
                depth_stencil_state_index: 0,
                blend_state_index: 0,
                stencil_reference: 0,
                state_flags: 0,
                unknown_0b: [0; 5],
                usage_data_offset: 0,
                binding_table_indices: [255; 4],
                unknown_16: 0,
                resource_usage: None,
            };
            sgd5::Sgd5Effect {
                name: name.into(),
                data_buffer_size: 0,
                stage_indices: [
                    vec![0],
                    if pixel_variant { vec![0] } else { vec![] },
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ],
                payload_offset: 0,
                payload_end: 0,
                metadata: sgd5::Sgd5EffectMetadata {
                    offset: 0,
                    header: sgd5::Sgd5EffectHeader {
                        vertex_inputs: sgd5::Sgd5VertexInputMetadata {
                            attribute_mask: 0,
                            shader_flags: 0,
                            attribute_formats: [0; 52],
                            reserved_44: 0,
                        },
                        attribute_mask_48: 0,
                    },
                    rasterizer_states: vec![],
                    depth_stencil_states: vec![],
                    blend_states: vec![],
                    sampler_states: vec![],
                    techniques: vec![sgd5::Sgd5Technique {
                        offset: 0,
                        name_string_offset: 0,
                        first_pass: 0,
                        pass_count,
                        name: Some("Draw".into()),
                    }],
                    passes: vec![pass; usize::from(pass_count)],
                    pass_lookup_words: vec![],
                },
                shared_reflection_source: sgd5::Sgd5ReflectionSource::Shared,
                main_reflection: Sgd5ReflectionTable::default(),
                property_reflection: Sgd5ReflectionTable::default(),
                binding_list: sgd5::Sgd5BindingList {
                    offset: 0,
                    texture_handle_count: 0,
                    entries: vec![],
                },
                string_pool: vec![],
                strings: vec![],
            }
        };
        Sgd5File {
            data: vec![],
            group_hash: 0,
            binding_tables: vec![],
            shared_reflection: Sgd5ReflectionTable::default(),
            shaders: vec![shader("VS"), shader("PS")],
            declared_effect_count: 2,
            effect_count: 2,
            trailer_flags: 0,
            trailer_reserved: [0; 6],
            effects: vec![
                make_effect("wind", 2, true),
                make_effect("shadow", 1, false),
            ],
            hash_names: HashMap::new(),
            effect_shader_map: HashMap::new(),
        }
    }

    fn selections(nodes: &[Node], visible: &[bool], result: &mut Vec<Selection>) {
        for node in nodes {
            if visible[node.id] {
                if let Some(selection) = node.selection {
                    result.push(selection);
                }
                selections(&node.children, visible, result);
            }
        }
    }

    #[test]
    fn hierarchy_search_keeps_ancestors_variants_and_distinct_pass_contexts() {
        let file = fixture();
        let mut navigation = Navigation::new(&file);
        navigation.filter("wind", None);
        let mut visible = Vec::new();
        selections(&navigation.nodes, &navigation.visible, &mut visible);
        assert!(visible.contains(&Selection::Effect(0)));
        assert!(!visible.contains(&Selection::Effect(1)));
        for pass in 0..2 {
            assert!(visible.contains(&Selection::Shader {
                shader: 0,
                effect: Some(0),
                technique: Some(0),
                pass: Some(pass)
            }));
        }
        assert!(visible.contains(&Selection::Shader {
            shader: 1,
            effect: Some(0),
            technique: None,
            pass: None
        }));
        navigation.filter("PS_Shared", Some(1));
        visible.clear();
        selections(&navigation.nodes, &navigation.visible, &mut visible);
        assert!(visible.contains(&Selection::Effect(0)));
        assert!(!visible.contains(&Selection::Technique {
            effect: 0,
            technique: 0
        }));
        assert!(!visible
            .iter()
            .any(|s| matches!(s, Selection::Shader { shader: 0, .. })));
        navigation.filter("1234", Some(0));
        visible.clear();
        selections(&navigation.nodes, &navigation.visible, &mut visible);
        assert!(visible.contains(&Selection::Technique {
            effect: 1,
            technique: 0
        }));
    }

    #[test]
    fn headless_inspector_renders_hierarchy_and_switches_metadata_tabs_without_stale_text() {
        let files = vec![fixture()];
        let ctx = egui::Context::default();
        let mut browser = Sgd5Browser::default();
        let mut mode = ShaderViewMode::Asm;
        for selection in [
            Selection::Group,
            Selection::Effect(0),
            Selection::Technique {
                effect: 0,
                technique: 0,
            },
            Selection::Pass {
                effect: 0,
                technique: Some(0),
                pass: 1,
            },
        ] {
            browser.selection = selection;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    browser.show_navigation(ui, &files);
                    browser.show_content(ui, &files, &mut mode);
                });
            });
        }
        browser.selection = Selection::Shader {
            shader: 0,
            effect: Some(0),
            technique: Some(0),
            pass: Some(1),
        };
        for (tab, expected) in [
            (1, "Selected context: effect wind / pass 1"),
            (2, "SGD5 container metadata"),
            (1, "Parameter mapping:"),
        ] {
            browser.shader_tab = tab;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| browser.show_content(ui, &files, &mut mode));
            });
            assert!(browser.text.as_ref().unwrap().3.contains(expected));
            assert_eq!(browser.text.as_ref().unwrap().2, tab);
        }
        let prepared_address = browser.prepared.as_ref().unwrap().metadata.as_ptr();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default()
                .show(ctx, |ui| browser.show_content(ui, &files, &mut mode));
        });
        assert_eq!(
            prepared_address,
            browser.prepared.as_ref().unwrap().metadata.as_ptr()
        );
    }

    #[test]
    #[ignore = "requires PERCY_SGD5_SAMPLE pointing to the analysis archive"]
    fn real_archive_shader_views_keep_pass_parameter_context() {
        let path = std::env::var_os("PERCY_SGD5_SAMPLE").expect("set PERCY_SGD5_SAMPLE");
        let files = sgd5::load_sgd5(std::path::Path::new(&path)).unwrap();
        let file = &files[0];
        let effect_index = file.effects.iter().position(|e| e.name == "cable").unwrap();
        let effect = &file.effects[effect_index];
        let pass_index = effect
            .metadata
            .passes
            .iter()
            .position(|p| p.shader_slots[1] != 0)
            .unwrap();
        let shader = file
            .pass_shader(effect, &effect.metadata.passes[pass_index], 1)
            .unwrap();
        let shader_index = file.shader_position(shader.stage, shader.index).unwrap();
        let ctx = egui::Context::default();
        let mut browser = Sgd5Browser {
            selection: Selection::Shader {
                shader: shader_index,
                effect: Some(effect_index),
                technique: None,
                pass: Some(pass_index),
            },
            shader_tab: 1,
            search: "cable".into(),
            ..Default::default()
        };
        let mut mode = ShaderViewMode::Asm;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                browser.show_navigation(ui, &files);
                browser.show_content(ui, &files, &mut mode);
            });
        });
        let text = &browser.text.as_ref().unwrap().3;
        assert!(text.contains("Selected context: effect cable"));
        assert!(text.contains("texture_handles["));
        assert!(text.contains("descriptor_byte_offset="));
        assert!(text.contains("Compiler resource usage (PS, verified against PRIV)"));
        assert!(
            text.contains("s[15..15]")
                || browser
                    .prepared
                    .as_ref()
                    .unwrap()
                    .metadata
                    .contains("s[15..15]")
        );
        browser.shader_tab = 0;
        for value in [ShaderViewMode::Asm, ShaderViewMode::Hlsl] {
            mode = value;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| browser.show_content(ui, &files, &mut mode));
            });
            let text = &browser.text.as_ref().unwrap().3;
            assert!(text.contains("Selected context: effect cable"));
            assert!(text.contains("Parameter mapping:"));
            assert!(text.contains("texture_handles["));
        }
        let alternate_shader = file
            .shaders
            .iter()
            .position(|shader| shader.name.starts_with("VS_VoxelizeGeneric_"))
            .unwrap();
        browser.selection = Selection::Shader {
            shader: alternate_shader,
            effect: None,
            technique: None,
            pass: None,
        };
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default()
                .show(ctx, |ui| browser.show_content(ui, &files, &mut mode));
        });
        assert_eq!(
            browser
                .prepared
                .as_ref()
                .unwrap()
                .programs
                .as_ref()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            browser
                .prepared
                .as_ref()
                .unwrap()
                .programs
                .as_ref()
                .unwrap()[browser.program_index]
                .hardware_stage(),
            dxbc::NativeStage::Vs
        );
        let es_index = browser
            .prepared
            .as_ref()
            .unwrap()
            .programs
            .as_ref()
            .unwrap()
            .iter()
            .position(|program| program.hardware_stage() == dxbc::NativeStage::Es)
            .unwrap();
        browser.program_index = es_index;
        browser.text = None;
        mode = ShaderViewMode::Asm;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default()
                .show(ctx, |ui| browser.show_content(ui, &files, &mut mode));
        });
        assert!(
            browser
                .text
                .as_ref()
                .unwrap()
                .3
                .contains("Compiler resource usage (ES"),
            "selected={} mapping={:?}",
            browser.program_index,
            browser.prepared.as_ref().unwrap().native_mappings
        );
        assert_eq!(
            browser
                .prepared
                .as_ref()
                .unwrap()
                .programs
                .as_ref()
                .unwrap()[es_index]
                .hardware_stage(),
            dxbc::NativeStage::Es
        );
    }
}
