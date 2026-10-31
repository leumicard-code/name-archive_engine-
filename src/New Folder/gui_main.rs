use archive_engine::{compress, extract, format_size, list_archive, Entry, Format};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

#[derive(PartialEq)]
enum ActiveTab {
    Compress,
    Extract,
}

enum EngineMsg {
    Progress {
        current: usize,
        total: usize,
        name: String,
    },
    Done(Result<String, String>),
    ListLoaded(Result<Vec<Entry>, String>),
}

struct ArchiveGuiApp {
    tab: ActiveTab,

    // Settings & Dialogs
    dark_mode: bool,
    font_scale: f32,
    show_about_dialog: bool,

    // Compress state
    compress_inputs: Vec<PathBuf>,
    compress_output: Option<PathBuf>,
    format: Format,
    level: i32,

    // Extract state
    archive_to_extract: Option<PathBuf>,
    extract_destination: Option<PathBuf>,
    archive_contents: Vec<Entry>,

    // Worker thread communication
    tx: Sender<EngineMsg>,
    rx: Receiver<EngineMsg>,
    is_busy: bool,
    progress: f32,
    status_label: String,
}

impl ArchiveGuiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // טעינת הפונט ישירות לתוך הבינארי בזמן קומפילציה
        let mut fonts = egui::FontDefinitions::default();

        fonts.font_data.insert(
            "custom_font".to_owned(),
            egui::FontData::from_static(include_bytes!("../assets/rb.ttf")),
        );

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

        // החלת גדלי פונטים ראשוניים
        Self::apply_font_scale(&cc.egui_ctx, 1.0);

        let (tx, rx) = channel();
        Self {
            tab: ActiveTab::Compress,
            dark_mode: true,
            font_scale: 1.0,
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

    // עדכון גדלי הפונטים בכל המערכת בזמן אמת לפי הסליידר
    fn apply_font_scale(ctx: &egui::Context, scale: f32) {
        let mut style = (*ctx.style()).clone();
        style.text_styles = [
            (
                egui::TextStyle::Heading,
                egui::FontId::new(18.0 * scale, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Button,
                egui::FontId::new(14.0 * scale, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Body,
                egui::FontId::new(13.0 * scale, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Small,
                egui::FontId::new(11.0 * scale, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Monospace,
                egui::FontId::new(12.0 * scale, egui::FontFamily::Monospace),
            ),
        ]
        .into();

        style.spacing.button_padding = egui::vec2(10.0 * scale, 5.0 * scale);
        ctx.set_style(style);
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
                } else {
                    return;
                }
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
                let _ = tx.send(EngineMsg::Progress {
                    current: curr,
                    total,
                    name: name.to_string(),
                });
            });

            match res {
                Ok(_) => {
                    let _ = tx.send(EngineMsg::Done(Ok(format!(
                        "Created: {}",
                        target_file.display()
                    ))));
                }
                Err(e) => {
                    let _ = tx.send(EngineMsg::Done(Err(e.to_string())));
                }
            }
        });
    }

    fn run_extraction(&mut self) {
        let archive = match &self.archive_to_extract {
            Some(a) => a.clone(),
            None => return,
        };

        let dest = match &self.extract_destination {
            Some(d) => d.clone(),
            None => {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.extract_destination = Some(folder.clone());
                    folder
                } else {
                    return;
                }
            }
        };

        self.is_busy = true;
        self.progress = 0.5;
        self.status_label = "Extracting archive...".into();

        let tx = self.tx.clone();
        thread::spawn(move || match extract(&archive, &dest) {
            Ok(_) => {
                let _ = tx.send(EngineMsg::Done(Ok(format!(
                    "Extracted to: {}",
                    dest.display()
                ))));
            }
            Err(e) => {
                let _ = tx.send(EngineMsg::Done(Err(e.to_string())));
            }
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

impl eframe::App for ArchiveGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // עדכונים מה-Worker Threads
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                EngineMsg::Progress {
                    current,
                    total,
                    name,
                } => {
                    self.progress = current as f32 / total as f32;
                    self.status_label = format!("Processing ({current}/{total}): {name}");
                }
                EngineMsg::Done(res) => {
                    self.is_busy = false;
                    self.progress = 1.0;
                    match res {
                        Ok(msg) => self.status_label = format!("✓ {msg}"),
                        Err(e) => self.status_label = format!("✗ Error: {e}"),
                    }
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

        // תמיכה ב-Drag & Drop
        ctx.input(|i| {

            if !i.raw.dropped_files.is_empty() && !self.is_busy {
                for file in &i.raw.dropped_files {
                    if let Some(path) = &file.path {
                        if Format::from_path(path).is_some() && self.tab == ActiveTab::Extract {
                            self.load_archive_entries(path.clone());
                        } else if !self.compress_inputs.contains(path) {
                            self.compress_inputs.push(path.clone());
                        }
                    }
                }
            }

        });

        if self.is_busy {
            ctx.request_repaint();
        }

        // ==========================================
        // 1. פאנל עליון: בחירת טאב, Theme, About וסליידר פונט
        // ==========================================
        egui::TopBottomPanel::top("top_header").show(ctx, |ui| {
            // קיבוע סגנון מקומי לסרגל העליון בלבד כדי למנוע תזוזת רכיבים בזמן גרירה
            ui.style_mut().text_styles = [
                (
                    egui::TextStyle::Button,
                    egui::FontId::new(13.5, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Body,
                    egui::FontId::new(13.0, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Small,
                    egui::FontId::new(11.0, egui::FontFamily::Proportional),
                ),
            ]
            .into();
            ui.style_mut().spacing.button_padding = egui::vec2(8.0, 4.0);

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // לשוניות (שומרות על גודל פונט עליון יציב)
                ui.selectable_value(&mut self.tab, ActiveTab::Compress, "📦 Compress");
                ui.selectable_value(&mut self.tab, ActiveTab::Extract, "📂 Extract & Browse");

                // יישור הגדרות הצד הימני
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // כפתור Theme
                    let theme_btn = if self.dark_mode {
                        "☀️ Light"
                    } else {
                        "🌙 Dark"
                    };
                    if ui.button(theme_btn).clicked() {
                        self.dark_mode = !self.dark_mode;
                        ctx.set_visuals(if self.dark_mode {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                    }

                    // כפתור About
                    if ui.button("ℹ About").clicked() {
                        self.show_about_dialog = true;
                    }

                    // סליידר גודל פונט עם צעדים רציפים ורוחב מוגדר
                    ui.label("Size:");
                    let slider = egui::Slider::new(&mut self.font_scale, 0.8..=1.5)
                        .step_by(0.05)
                        .show_value(false);

                    // הקצאת רוחב קבוע של 100 פיקסלים לסליידר
                    if ui.add_sized([100.0, 18.0], slider).changed() {
                        Self::apply_font_scale(ctx, self.font_scale);
                    }
                    ui.label("Aa");
                });
            });
            ui.add_space(4.0);
        });

        // ==========================================
        // 2. בר סטטוס תחתון קבוע (Bottom Status Bar)
        // ==========================================
        egui::TopBottomPanel::bottom("bottom_status_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // הצגת הטקסט של הסטטוס בצד שמאל
                ui.label(egui::RichText::new(&self.status_label).small());

                // הצגת ה-Progress Bar בצד ימין
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::ProgressBar::new(self.progress)
                            .desired_width(180.0)
                            .show_percentage(),
                    );
                });
            });
            ui.add_space(4.0);
        });

        // ==========================================
        // 3. חלון אודות (Modal Dialog)
        // ==========================================
        // ==========================================
        // 3. חלון אודות (Modal Dialog)
        // ==========================================
        if self.show_about_dialog {
            let mut is_open = true;
            let mut close_clicked = false;

            egui::Window::new("About Archive Engine")
                .open(&mut is_open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("Archive-EngineV12");
                        ui.label(
                            egui::RichText::new("v0.3.0 - High Performance Linux Archiver").small(),
                        );
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(8.0);

                        ui.label(egui::RichText::new("Shay Kadosh").strong());
                        ui.add_space(4.0);
                        ui.label("⚡ System Architect & Lead Developer");
                        ui.label("🤖 Gemini - AI Co-Pilot & Rust Optimizer");

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(8.0);

                        ui.label("Built with Rust, egui & wgpu");
                        ui.label("Engineered for speed & NVMe longevity.");
                        ui.label("Built with love DEV.TEAM");

                        ui.add_space(10.0);
                        if ui.button("Close").clicked() {
                            close_clicked = true;
                        }
                    });
                });

            // עדכון המצב: נסגר או בלחיצה על ה-X או בלחיצה על כפתור Close
            if !is_open || close_clicked {
                self.show_about_dialog = false;
            }
        }
        // ==========================================
        // 4. תוכן מרכזי (Central Panel)
        // ==========================================
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            ActiveTab::Compress => self.show_compress_tab(ui),
            ActiveTab::Extract => self.show_extract_tab(ui),
        });
    }
}

impl ArchiveGuiApp {
    fn show_compress_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("Drop files here or choose from system:");
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.is_busy, egui::Button::new("Add Files...")).clicked() {
                if let Some(files) = rfd::FileDialog::new().pick_files() {
                    self.compress_inputs.extend(files);
                }
            }
            if ui.add_enabled(!self.is_busy, egui::Button::new("Add Folder...")).clicked() {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.compress_inputs.push(folder);
                }
            }
            if ui.button("Clear List").clicked() && !self.is_busy {
                self.compress_inputs.clear();
            }
        });

        ui.add_space(6.0);

        // פריסה תחתונה עם מרווחי נשימה מוגדרים
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(10.0); // מרווח מסרגל הסטטוס התחתון

            let btn_label = if self.is_busy { "Processing..." } else { "Choose Location & Compress" };
            if ui.add_enabled(
                !self.is_busy,
                egui::Button::new(btn_label).min_size([160.0, 32.0].into())
            ).clicked() {
                self.run_compression();
            }

            ui.add_space(12.0); // מרווח ברור בין הכפתור לשורת הפורמט

            ui.horizontal(|ui| {
                ui.label("Format:");
                egui::ComboBox::from_id_source("format_selector")
                .selected_text(self.format.extension())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.format, Format::TarZstd, "tar.zst (Fast & Recommended)");
                    ui.selectable_value(&mut self.format, Format::Zip, "zip (Standard)");
                    ui.selectable_value(&mut self.format, Format::TarGz, "tar.gz (Gzip)");
                    ui.selectable_value(&mut self.format, Format::TarXz, "tar.xz (High Ratio)");
                    ui.selectable_value(&mut self.format, Format::SevenZ, "7z (SevenZip)");
                });

                ui.add_space(12.0);
                ui.label("Level:");
                ui.add_enabled(!self.is_busy, egui::Slider::new(&mut self.level, 1..=19));
            });

            ui.add_space(8.0); // מרווח מעל שורת הפורמט
            ui.separator();
            ui.add_space(6.0); // מרווח מקו ההפרדה לרשימת הקבצים

            // רשימת הקבצים במרכז
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    if self.compress_inputs.is_empty() {
                        ui.weak("No files added yet");
                    } else {
                        for (idx, path) in self.compress_inputs.iter().enumerate() {
                            ui.label(format!("{}. {}", idx + 1, path.display()));
                        }
                    }
                });
            });
        });
    }

    fn show_extract_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.is_busy, egui::Button::new("Open Archive...")).clicked() {
                if let Some(f) = rfd::FileDialog::new().pick_file() {
                    self.load_archive_entries(f);
                }
            }
            if let Some(path) = &self.archive_to_extract {
                ui.label(format!("Selected: {}", path.file_name().unwrap_or_default().to_string_lossy()));
            }
        });

        ui.separator();
        ui.label(format!("Archive Contents ({} items):", self.archive_contents.len()));
        ui.add_space(6.0);

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(10.0); // מרווח מסרגל הסטטוס התחתון

            let btn_label = if self.is_busy { "Extracting..." } else { "Extract All" };
            if ui.add_enabled(
                !self.is_busy && self.archive_to_extract.is_some(),
                              egui::Button::new(btn_label).min_size([120.0, 32.0].into()),
            ).clicked() {
                self.run_extraction();
            }

            ui.add_space(12.0); // מרווח בין כפתור הפריסה לבחירת היעד

            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_busy, egui::Button::new("Extract Destination...")).clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.extract_destination = Some(d);
                    }
                }
                let dest_name = self.extract_destination.as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "Default Folder".into());
                ui.label(dest_name);
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);

            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    if self.archive_contents.is_empty() {
                        ui.weak("Open an archive to view its contents");
                    } else {
                        for entry in &self.archive_contents {
                            ui.horizontal(|ui| {
                                let icon = if entry.is_dir { "📁" } else { "📄" };
                                ui.label(format!("{icon} {}", entry.path));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(format_size(entry.size));
                                });
                            });
                        }
                    }
                });
            });
        });
    }
}




fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([680.0, 500.0])
            .with_min_inner_size([520.0, 400.0])
            .with_title("Archive Engine"),
        ..Default::default()
    };

    eframe::run_native(
        "Archive Engine",
        options,
        Box::new(|cc| Ok(Box::new(ArchiveGuiApp::new(cc)))),
    )
}
