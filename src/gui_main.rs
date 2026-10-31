use archive_engine::{compress, extract, format_size, list_archive, Entry, Format};
use eframe::egui;
use elegance::{
    Accent, Badge, BadgeTone, Button, ButtonSize, BuiltInTheme, Card,
    Modal, ProgressBar, SegmentedControl, SegmentedSize, Select, Theme, ThemeSwitcher,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

#[derive(PartialEq, Clone, Copy)]
enum ActiveTab {
    Compress,
    Extract,
}

enum EngineMsg {
    Progress { current: usize, total: usize, name: String },
    Done(Result<String, String>),
    ListLoaded(Result<Vec<Entry>, String>),
}

struct ArchiveGuiApp {
    tab: ActiveTab,
    theme_selector: BuiltInTheme,
    show_about_dialog: bool,

    compress_inputs: Vec<PathBuf>,
    compress_output: Option<PathBuf>,
    format: Format,
        level: i32,

        archive_to_extract: Option<PathBuf>,
        extract_destination: Option<PathBuf>,
        archive_contents: Vec<Entry>,

        tx: Sender<EngineMsg>,
        rx: Receiver<EngineMsg>,
        is_busy: bool,
        progress: f32,
        status_label: String,
}

impl ArchiveGuiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // התקנת נושא העיצוב החדש והחלק
        Theme::slate().install(&cc.egui_ctx);
        let mut fonts = egui::FontDefinitions::default();

        fonts.font_data.insert(
            "custom_font".to_owned(),
                               egui::FontData::from_static(include_bytes!("../assets/FM.ttf")).into(),

        );

        // הכנסה לאינדקס 0 נותנת לו חשיבות עליונה במנוע הרנדור (לכל שאר האותיות הרגילות).
        fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "custom_font".to_owned());

        fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("custom_font".to_owned());

        cc.egui_ctx.set_fonts(fonts);

        let (tx, rx) = channel();
        Self {
            tab: ActiveTab::Compress,
            theme_selector: BuiltInTheme::Slate,
            show_about_dialog: false,

            compress_inputs: Vec::new(),
            compress_output: None,
            format: Format::TarZstd,
                level: 5,

                archive_to_extract: None,
                extract_destination: None,
                archive_contents: Vec::new(),

                tx,
                rx,
                is_busy: false,
                progress: 0.0,
                status_label: "Ready".to_string(),
        }
    }

    fn run_compression(&mut self) {
        if self.compress_inputs.is_empty() {
            self.status_label = "Error: No files selected".into();
            return;
        }

        let ext = self.format.extension();
        let target_file = match &self.compress_output {
            Some(path) => path.clone(),
            None => {
                if let Some(saved) = rfd::FileDialog::new()
                    .add_filter("Archive", &[ext])
                    .set_file_name(&format!("archive.{}", ext))
                    .save_file()
                    {
                        self.compress_output = Some(saved.clone());
                        saved
                    } else { return; }
            }
        };

        self.is_busy = true;
        self.progress = 0.0;
        self.status_label = "Starting compression...".into();

        let tx = self.tx.clone();
        let inputs = self.compress_inputs.clone();
        let fmt = self.format;
        let lvl = self.level;

        thread::spawn(move || {
            let refs: Vec<&Path> = inputs.iter().map(|p| p.as_path()).collect();
            let res = compress(&refs, &target_file, fmt, lvl, |curr, total, name| {
                let _ = tx.send(EngineMsg::Progress { current: curr, total, name: name.to_string() });
            });
            match res {
                Ok(_) => { let _ = tx.send(EngineMsg::Done(Ok(format!("Created: {}", target_file.display())))); }
                Err(e) => { let _ = tx.send(EngineMsg::Done(Err(e.to_string()))); }
            }
        });
    }

    fn run_extraction(&mut self) {
        let archive = match &self.archive_to_extract { Some(a) => a.clone(), None => return };
        let dest = match &self.extract_destination {
            Some(d) => d.clone(),
            None => {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.extract_destination = Some(folder.clone());
                    folder
                } else { return; }
            }
        };

        self.is_busy = true;
        self.progress = 0.5;
        self.status_label = "Extracting archive...".into();

        let tx = self.tx.clone();
        thread::spawn(move || match extract(&archive, &dest) {
            Ok(_) => { let _ = tx.send(EngineMsg::Done(Ok(format!("Extracted to: {}", dest.display())))); }
            Err(e) => { let _ = tx.send(EngineMsg::Done(Err(e.to_string()))); }
        });
    }

    fn load_archive_entries(&mut self, path: PathBuf) {
        self.archive_to_extract = Some(path.clone());
        self.archive_contents.clear();
        self.status_label = "Loading archive contents...".into();
        self.is_busy = true;

        let tx = self.tx.clone();
        thread::spawn(move || {
            let res = list_archive(&path).map_err(|e| e.to_string());
            let _ = tx.send(EngineMsg::ListLoaded(res));
        });
    }
}

// ------ במערכות החדשות App מתבססת ספציפית על ה- Ui Root ----------
impl eframe::App for ArchiveGuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {

        let ctx = ui.ctx().clone();

        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                EngineMsg::Progress { current, total, name } => {
                    self.progress = current as f32 / total as f32;
                    self.status_label = format!("Processing ({current}/{total}): {name}");
                }
                EngineMsg::Done(res) => {
                    self.is_busy = false;
                    self.progress = 1.0;
                    self.status_label = match res {
                        Ok(msg) => format!("✓ {msg}"),
                        Err(e) => format!("✗ Error: {e}"),
                    };
                }
                EngineMsg::ListLoaded(res) => {
                    self.is_busy = false;
                    match res {
                        Ok(entries) => {
                            self.status_label = format!("Loaded {} items", entries.len());
                            self.archive_contents = entries;
                        }
                        Err(e) => self.status_label = format!("Failed to read archive: {e}"),
                    }
                }
            }
        }

        // --- ה Drag & Drop מחולץ במבנה הבטוח של 0.36 --------
        // --- ה Drag & Drop מחולץ במבנה הבטוח של 0.36 --------
        ctx.input(|i| {
            if !i.raw.dropped_files.is_empty() && !self.is_busy {
                for file in &i.raw.dropped_files {

                    let local_path = file.path().to_path_buf(); // עקיפה ישירה ונקיה, 0.36 דורשת אובייקט ממשי בלבד!

                    // ליתר ביטחון נוודא שמערכת ההפעלה באמת צירפה כתובת ממשית
                    if local_path.as_os_str().is_empty() {
                        continue;
                    }

                    if Format::from_path(&local_path).is_some() && self.tab == ActiveTab::Extract {
                        self.load_archive_entries(local_path);
                    } else if !self.compress_inputs.contains(&local_path) {
                        self.compress_inputs.push(local_path);
                    }
                }
            }
        });

        if self.is_busy {
            ctx.request_repaint();
        }

        Modal::new("about_modal", &mut self.show_about_dialog)
        .heading("Archive-EngineV9")
        .show(&ctx, |ui| {
            ui.label(egui::RichText::new("v0.3.0 - High Performance Linux Archiver").small());
            ui.separator();
            ui.add_space(8.0);

            ui.label(egui::RichText::new("Shay Kadosh ©️ 2000-2026").strong());
            ui.add_space(4.0);
            ui.label("⚡ System Architect & Lead Developer");
            ui.label("⚡ LLVM Compiler Infrastructure☑️");
            ui.label("⚡ Built with love 🥰 😍 DEV-TEAM ™️");
            ui.add_space(8.0);
            ui.separator();

            ui.horizontal(|ui| {
                ui.add(Badge::new("Native NVMe Speed", BadgeTone::Ok));
                ui.add(Badge::new("AI Copilot Optimizer", BadgeTone::Info));
                ui.add(Badge::new("Insanely fast", BadgeTone::Danger));
                ui.add(Badge::new("0.1.2", BadgeTone::Warning));



            });
        });


        // המעבר החכם: egui::Panel
        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let mut current_tab = if self.tab == ActiveTab::Compress { 0 } else { 1 };

                if ui.add_enabled(!self.is_busy, SegmentedControl::new(
                    &mut current_tab,
                    ["📦 Compress Center", "📂 Extract Center"]
                ).size(SegmentedSize::Large))
                    .changed() {
                        self.tab = if current_tab == 0 { ActiveTab::Compress } else { ActiveTab::Extract };
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(Button::new("ℹ About").size(ButtonSize::Medium)).clicked() {
                            self.show_about_dialog = true;
                        }
                        ui.add_space(10.0);
                        ui.add(ThemeSwitcher::new(&mut self.theme_selector));
                    });
            });
            ui.add_space(8.0);
        });

        egui::Panel::bottom("bottom_bar").show(ui, |ui| {
            ui.add_space(8.0);
            ui.add(
                ProgressBar::new(self.progress)
                .text(&self.status_label)
                .accent(Accent::Blue)
            );
            ui.add_space(4.0);
        });

        // קביעת חלון התצוגה המרכזית עם שיטות החדשות ל-ui פתוח במקום ctx מיושן
        egui::CentralPanel::default().show(ui, |ui| {
            match self.tab {
                ActiveTab::Compress => self.show_compress_tab(ui),
                                           ActiveTab::Extract => self.show_extract_tab(ui),
            }
        });
    }
}

// ------ מסכי ההידור --------
impl ArchiveGuiApp {
    fn show_compress_tab(&mut self, ui: &mut egui::Ui) {
        Card::new().heading("Select Targets").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_busy, Button::new("📄 Add Files").size(ButtonSize::Medium)).clicked() {
                    if let Some(files) = rfd::FileDialog::new().pick_files() { self.compress_inputs.extend(files); }
                }
                if ui.add_enabled(!self.is_busy, Button::new("📁 Add Folder").size(ButtonSize::Medium)).clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() { self.compress_inputs.push(folder); }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!self.is_busy, Button::new("🗑 Clear List").accent(Accent::Red).outline()).clicked() {
                        self.compress_inputs.clear();
                    }
                });
            });
        });

        ui.add_space(10.0);

        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
            let height = ui.available_height() - 95.0;

            // המילה עודכנה ל 'max_height' לטובת מנוע הבטוח של egui 0.36
            egui::ScrollArea::vertical().min_scrolled_height(height).max_height(height).show(ui, |ui| {
                Card::new().show(ui, |ui| {
                    if self.compress_inputs.is_empty() {
                        ui.centered_and_justified(|ui| { ui.weak("⬇ Drag and Drop your files & folders here"); });
                    } else {
                        for (idx, path) in self.compress_inputs.iter().enumerate() {
                            ui.label(format!("{}. {}", idx + 1, path.display()));
                            ui.add_space(2.0);
                            ui.separator();
                        }
                    }
                });
            });

            ui.add_space(10.0);

            Card::new().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Archive Format: ");
                    ui.add(Select::new("fmt_combo", &mut self.format).options([
                        (Format::TarZstd, "tar.zst (Ultra Fast NVMe)"),
                                                                              (Format::Zip, "zip (Standard Compatability)"),
                                                                              (Format::TarGz, "tar.gz (Gzip Oldschool)"),
                                                                              (Format::TarXz, "tar.xz (Max Space Ratio)"),
                                                                              (Format::SevenZ, "7z (Multi-platform)")
                    ]));

                    ui.add_space(10.0);

                    ui.label("Compression Level:");
                    ui.add_enabled(!self.is_busy, egui::Slider::new(&mut self.level, 1..=19));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn_txt = if self.is_busy { "⚙ Processing...".into() } else { format!("Compress to {}", self.format.extension()) };
                        if ui.add_enabled(
                            !self.is_busy && !self.compress_inputs.is_empty(),
                                          Button::new(btn_txt).accent(Accent::Green).size(ButtonSize::Large)
                        ).clicked() {
                            self.run_compression();
                        }
                    });
                });
            });
        });
    }

    fn show_extract_tab(&mut self, ui: &mut egui::Ui) {
        Card::new().heading("Inspector Archive").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_busy, Button::new("📂 Open Archive...").size(ButtonSize::Medium)).clicked() {
                    if let Some(f) = rfd::FileDialog::new().pick_file() {
                        self.load_archive_entries(f);
                    }
                }

                ui.add_space(8.0);

                if let Some(path) = &self.archive_to_extract {
                    ui.add(Badge::new(path.file_name().unwrap_or_default().to_string_lossy(), BadgeTone::Info));
                    ui.add(Badge::new(format!("{} elements indexed", self.archive_contents.len()), BadgeTone::Ok));
                }
            });
        });

        ui.add_space(10.0);

        let height = ui.available_height() - 75.0;

        // כאן כנ"ל מקונפג ל max_height לגרסה החלקה החדשה
        egui::ScrollArea::vertical().min_scrolled_height(height).max_height(height).show(ui, |ui| {
            Card::new().show(ui, |ui| {
                if self.archive_contents.is_empty() {
                    ui.centered_and_justified(|ui| { ui.weak("⬇ Select or Drop an archive to inspect payload ⬇"); });
                } else {
                    for entry in &self.archive_contents {
                        ui.horizontal(|ui| {
                            let (icon, color) = if entry.is_dir { ("📁", BadgeTone::Info) } else { ("📄", BadgeTone::Neutral) };
                            ui.add(Badge::new(icon, color));
                            ui.label(&entry.path);

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.weak(format_size(entry.size));
                            });
                        });
                        ui.separator();
                    }
                }
            });
        });

        ui.add_space(10.0);

        Card::new().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Extraction Folder:");
                let dest_name = self.extract_destination.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| "(Same folder as target)".into());
                ui.weak(dest_name);

                ui.add_space(10.0);

                if ui.add_enabled(!self.is_busy, Button::new("Select Setup")).clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() { self.extract_destination = Some(d); }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn_txt = if self.is_busy { "⏳ Deploying payload..." } else { "Extract Package Now" };
                    if ui.add_enabled(
                        !self.is_busy && self.archive_to_extract.is_some(),
                                      Button::new(btn_txt).accent(Accent::Amber).size(ButtonSize::Large)
                    ).clicked() {
                        self.run_extraction();
                    }
                });
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
        .with_inner_size([760.0, 560.0])
        .with_min_inner_size([520.0, 440.0])
        .with_title("Archive Engine (Elegance Edition)"),
        ..Default::default()
    };

    eframe::run_native(
        "Archive Engine",
        options,
        Box::new(|cc| Ok(Box::new(ArchiveGuiApp::new(cc)))),
    )
}
