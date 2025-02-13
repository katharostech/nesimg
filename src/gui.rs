use anyhow::Context;
use eframe::{egui, IconData};
use egui::{util::undoer::Undoer, Key, Layout, Modifiers, Ui};
use egui_extras::{Size, StripBuilder};
use native_dialog::FileDialog;
use once_cell::sync::Lazy;
use path_absolutize::Absolutize;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Read, path::Path, time::Instant};
use watch::WatchReceiver;

use tracing as trc;

mod components;
mod keyboard_shortcuts;
mod project_state;
mod tabs;
mod util;

use components::{send_error_notification, show_notifications};
use keyboard_shortcuts::KeyboardShortcut;
use tabs::NesimgGuiTab;

use crate::{cli::GuiArgs, project::Project};

use self::{
    components::send_info_notification,
    project_state::{LoadedProject, ProjectState},
    util::{pick_file, FileFilter},
};

/// Run the GUI
pub fn run_gui(args: GuiArgs) {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        icon_data: Some(load_icon()),
        min_window_size: Some(egui::Vec2::new(750.0, 400.0)),
        ..Default::default()
    };

    eframe::run_native(
        "NESImg",
        native_options,
        Box::new(|cc| Box::new(NesimgGui::new(cc, args))),
    );
}

/// The root GUI element: renders the menu bar and tabs, and offloads rending the main region to the
/// specific tab gui implementations.
#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct NesimgGui {
    /// The current GUI tab
    current_tab: String,
    /// The list of tab implementations and their names
    #[serde(skip)]
    tabs: Vec<(String, Box<dyn NesimgGuiTab>)>,

    /// Dark mode enabled state
    dark_mode: bool,

    /// Whether or not to show the help panel
    show_help: bool,

    // The UI scale
    pixels_per_point: f32,

    /// The root GUI state, which will be shared with and allowed to be modified by tabs
    #[serde(skip)]
    state: RootState,
}

impl Default for NesimgGui {
    fn default() -> Self {
        Self {
            dark_mode: true,
            show_help: true,
            current_tab: "Sources".into(),
            pixels_per_point: 1.2,
            tabs: vec![
                ("Maps".into(), Box::new(tabs::maps::MapsTab::default())),
                (
                    "Metatilesets".into(),
                    Box::new(tabs::metatilesets::MetatilesetsTab::default()),
                ),
                (
                    "Metatiles".into(),
                    Box::new(tabs::metatiles::MetatilesTab::default()),
                ),
                (
                    "Sources".into(),
                    Box::new(tabs::sources::SourcesTab::default()),
                ),
            ],
            state: Default::default(),
        }
    }
}

/// The root GUI state, which will be shared with and allowed to be modified by tabs
pub struct RootState {
    /// This contains the deserialized project and it's GUI state, such as loaded images and it's
    /// undo-history
    project: Option<ProjectState>,

    /// This contains the plain deserialized project and the path that it was loaded from
    loaded_project: WatchReceiver<Option<LoadedProject>>,

    /// Start time of the app, which can be used for calculating elapsed time for [`Undoer`]s
    start: Instant,
}

impl Default for RootState {
    fn default() -> Self {
        Self {
            project: None,
            loaded_project: watch::channel(None).1,
            start: Instant::now(),
        }
    }
}

impl NesimgGui {
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>, args: GuiArgs) -> Self {
        let mut gui = {
            // Scale down the feathering slightly to compensate and keep edges from looking a little
            // blurry
            cc.egui_ctx.tessellation_options().feathering_size_in_pixels = 0.7;

            // Load previous app state (if any).
            if let Some(storage) = cc.storage {
                let gui: NesimgGui =
                    eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();

                if gui.dark_mode {
                    cc.egui_ctx.set_visuals(egui::style::Visuals::dark());
                }

                gui
            } else {
                // Default to dark theme
                cc.egui_ctx.set_visuals(egui::style::Visuals::dark());

                Default::default()
            }
        };

        cc.egui_ctx.set_pixels_per_point(gui.pixels_per_point);

        if let Some(path) = args.project {
            gui.state.loaded_project =
                watch::channel(get_loaded_project(&cc.egui_ctx, &path, true)).1;
        }

        gui
    }

    fn toggle_dark_mode(&mut self, ui: &mut Ui) {
        if ui.visuals().dark_mode {
            self.dark_mode = false;
            ui.ctx().set_visuals(egui::Visuals::light())
        } else {
            self.dark_mode = true;
            ui.ctx().set_visuals(egui::Visuals::dark())
        }
    }
}

/// Actions that can be triggered by menus or keyboard shortcuts
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) enum MainGuiAction {
    Quit,
    NewProject,
    OpenProject,
    SaveProject,
    Undo,
}

impl MainGuiAction {
    fn perform(&self, gui: &mut NesimgGui, ctx: &egui::Context, frame: &mut eframe::Frame) {
        #[allow(clippy::unit_arg)]
        if let Err(e) = match self {
            MainGuiAction::Quit => Ok(frame.quit()),
            MainGuiAction::NewProject => new_project(gui, ctx),
            MainGuiAction::OpenProject => open_project(gui, ctx),
            MainGuiAction::SaveProject => save_project(gui, ctx),
            MainGuiAction::Undo => {
                if let Some(project) = &mut gui.state.project {
                    if let Some(undone) = project.undoer.undo(&project.data) {
                        let mut needs_reload = false;
                        if project.data.sources != undone.sources {
                            needs_reload = true;
                        }
                        project.data = undone.clone();
                        if needs_reload {
                            project.reload_source_images();
                        }
                    }
                }

                Ok(())
            }
        } {
            trc::error!("{}", e);
            send_error_notification(ctx, format!("{:#}", e));
        }
    }
}

/// Keyboard shortcuts that can trigger [`MainGuiAction`]s
static MAIN_GUI_SHORTCUTS: Lazy<HashMap<MainGuiAction, KeyboardShortcut>> = Lazy::new(|| {
    let mut shortcuts = HashMap::default();

    shortcuts.insert(MainGuiAction::Quit, (Modifiers::COMMAND, Key::Q).into());
    shortcuts.insert(
        MainGuiAction::NewProject,
        (Modifiers::COMMAND, Key::N).into(),
    );
    shortcuts.insert(
        MainGuiAction::OpenProject,
        (Modifiers::COMMAND, Key::O).into(),
    );
    shortcuts.insert(
        MainGuiAction::SaveProject,
        (Modifiers::COMMAND, Key::S).into(),
    );
    shortcuts.insert(MainGuiAction::Undo, (Modifiers::COMMAND, Key::Z).into());

    shortcuts
});

/// GUI implementation
impl eframe::App for NesimgGui {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        handle_keyboard_shortcuts(self, ctx, frame);

        show_notifications(ctx);

        if let Some(loaded) = self.state.loaded_project.get_if_new() {
            if let Some(loaded) = loaded {
                let data = loaded.data;
                let mut undoer = Undoer::default();
                undoer.feed_state(self.state.start.elapsed().as_secs_f64(), &data);

                let mut state = ProjectState {
                    data,
                    path: loaded.path,
                    undoer,
                    source_images: Default::default(),
                };
                state.reload_source_images();

                self.state.project = Some(state);
            } else {
                self.state.project = None;
            }
        }

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            let default_visuals = ui.visuals().clone();

            ui.add_space(1.0);
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    let new_shortcut = MAIN_GUI_SHORTCUTS
                        .get(&MainGuiAction::NewProject)
                        .map_or(String::new(), |x| format!("\t{}", x));
                    let open_shortcut = MAIN_GUI_SHORTCUTS
                        .get(&MainGuiAction::OpenProject)
                        .map_or(String::new(), |x| format!("\t{}", x));
                    let save_shortcut = MAIN_GUI_SHORTCUTS
                        .get(&MainGuiAction::SaveProject)
                        .map_or(String::new(), |x| format!("\t{}", x));
                    let quit_shortcut = MAIN_GUI_SHORTCUTS
                        .get(&MainGuiAction::Quit)
                        .map_or(String::new(), |x| format!("\t{}", x));

                    if ui
                        .button(format!("➕ New Project{}", new_shortcut))
                        .clicked()
                    {
                        MainGuiAction::NewProject.perform(self, ctx, frame);
                        ui.close_menu();
                    }

                    if ui
                        .button(format!("🗁 Open Project{}", open_shortcut))
                        .clicked()
                    {
                        MainGuiAction::OpenProject.perform(self, ctx, frame);
                        ui.close_menu();
                    }

                    ui.add_enabled_ui(self.state.project.is_some(), |ui| {
                        if ui
                            .button(format!("📩 Save Project{}", save_shortcut))
                            .clicked()
                        {
                            MainGuiAction::SaveProject.perform(self, ctx, frame);
                            ui.close_menu();
                        }
                    });

                    ui.separator();

                    if ui.button(format!("🗙 Quit{}", quit_shortcut)).clicked() {
                        frame.quit();
                    }
                });

                ui.menu_button("Edit", |ui| {
                    ui.add_enabled_ui(self.state.project.is_some(), |ui| {
                        let undo_shortcut = MAIN_GUI_SHORTCUTS
                            .get(&MainGuiAction::Undo)
                            .map_or(String::new(), |x| format!("\t{}", x));

                        if ui.button(format!("⮪ Undo {}", undo_shortcut)).clicked() {
                            MainGuiAction::Undo.perform(self, ctx, frame);
                        }
                    });
                });

                ui.menu_button("View", |ui| {
                    if ui.checkbox(&mut self.dark_mode, "🌙 Dark Theme").clicked() {
                        self.toggle_dark_mode(ui);
                    }
                    ui.checkbox(&mut self.show_help, "ℹ Show Help Panel");

                    let previous_ppp = ctx.pixels_per_point();
                    ui.horizontal(|ui| {
                        ui.add_space(1.0);
                        ui.spacing_mut().item_spacing = egui::Vec2::splat(3.0);
                        ui.label("🔎 UI Scale: ");
                        ui.style_mut().visuals = default_visuals;
                        let resp = ui.add(
                            egui::Slider::new(&mut self.pixels_per_point, 0.5..=2.0)
                                .logarithmic(true),
                        );
                        if !resp.dragged() && previous_ppp != self.pixels_per_point {
                            ctx.set_pixels_per_point(self.pixels_per_point);
                        }
                    });

                    if cfg!(debug_assertions) {
                        ui.separator();
                        if ui.checkbox(&mut ctx.debug_on_hover(), "Debug UI").clicked() {
                            ctx.set_debug_on_hover(!ctx.debug_on_hover());
                        }
                    }
                });

                // Tab list
                ui.with_layout(Layout::right_to_left(), |ui| {
                    if self.state.project.is_none() {
                        ui.set_enabled(false);
                    }
                    ui.horizontal(|ui| {
                        for (name, tab) in &self.tabs {
                            ui.selectable_value(&mut self.current_tab, name.clone(), name)
                                .on_hover_text(tab.tooltip())
                                .on_disabled_hover_text("Open project to edit");
                        }
                    });
                    ui.separator();
                });
            });
        });

        if self.show_help {
            egui::TopBottomPanel::bottom("help_panel")
                .resizable(true)
                .default_height(160.0)
                .show(ctx, |ui| {
                    let help_text = if self.state.project.is_some() {
                        let mut help_text = "";
                        for (name, tab) in &mut self.tabs {
                            if name == &self.current_tab {
                                help_text = tab.help_text();
                                break;
                            }
                        }
                        help_text
                    } else {
                        include_str!("./gui/help.txt")
                    };

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.set_height(ui.available_height());
                        ui.add_space(7.0);

                        ui.set_width(ui.available_width());

                        ui.style_mut()
                            .text_styles
                            .get_mut(&egui::TextStyle::Heading)
                            .unwrap()
                            .size = 15.0;

                        egui_demo_lib::easy_mark::easy_mark(ui, help_text);

                        ui.add_space(7.0);
                    });
                });
        }

        // Render the actual tab contents
        if let Some(project) = &mut self.state.project {
            // TODO: Do this only when deleting things?
            project.cleanup_dead_refs();
            for (name, tab) in &mut self.tabs {
                if name == &self.current_tab {
                    tab.show(project, ctx, frame);
                }
            }
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                StripBuilder::new(ui)
                    .sizes(Size::relative(0.3), 3)
                    .vertical(|mut strip| {
                        strip.cell(|_| ());
                        strip.strip(|builder| {
                            builder
                                .sizes(Size::relative(0.25), 4)
                                .horizontal(|mut strip| {
                                    strip.cell(|_| ());
                                    strip.cell(|ui| {
                                        ui.centered_and_justified(|ui| {
                                            if ui.button("➕ New Project").clicked() {
                                                MainGuiAction::NewProject.perform(self, ctx, frame);
                                            }
                                        });
                                    });
                                    strip.cell(|ui| {
                                        ui.centered_and_justified(|ui| {
                                            if ui.button("🗁 Open Project").clicked() {
                                                MainGuiAction::OpenProject
                                                    .perform(self, ctx, frame);
                                            }
                                        });
                                    });
                                    strip.cell(|_| ());
                                });
                        });
                        strip.cell(|_| ());
                    });
            });
        }

        // Update the undo state for the project, if one has been loaded
        if let Some(project) = &mut self.state.project {
            project
                .undoer
                .feed_state(self.state.start.elapsed().as_secs_f64(), &project.data);
        }
    }
}

fn handle_keyboard_shortcuts(gui: &mut NesimgGui, ctx: &egui::Context, frame: &mut eframe::Frame) {
    for (action, shortcut) in &*MAIN_GUI_SHORTCUTS {
        if ctx
            .input_mut()
            .consume_key(shortcut.modifiers, shortcut.key)
        {
            action.perform(gui, ctx, frame);
        }
    }
}

fn new_project(gui: &mut NesimgGui, ctx: &egui::Context) -> anyhow::Result<()> {
    let (sender, receiver) = watch::channel(None);
    gui.state.loaded_project = receiver;

    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let save_path = FileDialog::new()
            .add_filter("NESImg Project", &["nesimg"])
            .set_filename("project.nesimg")
            .show_save_single_file()
            .expect("Show save dialog");

        let inner = || -> anyhow::Result<()> {
            if let Some(path) = save_path {
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .truncate(true)
                    .create(true)
                    .open(&path)
                    .context("Open file to save")?;

                let data = Project::default();

                ron::ser::to_writer_pretty(file, &data, Default::default())
                    .context("Serialize project to JSON")?;

                sender.send(Some(LoadedProject {
                    data,
                    path: path.absolutize().unwrap().to_path_buf(),
                }));
            }

            Ok(())
        };

        if let Err(e) = inner() {
            send_error_notification(&ctx, format!("{:#}", e));
        }
    });

    Ok(())
}

fn open_project(gui: &mut NesimgGui, ctx: &egui::Context) -> anyhow::Result<()> {
    let ctx = ctx.clone();
    gui.state.loaded_project = pick_file(
        &[FileFilter {
            name: "NESImg Projects",
            extensions: &["nesimg"],
        }],
        move |path| get_loaded_project(&ctx, path, false),
    );

    Ok(())
}

fn get_loaded_project(
    ctx: &egui::Context,
    path: &Path,
    create_if_not_exists: bool,
) -> Option<LoadedProject> {
    let inner = || -> anyhow::Result<_> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .context("Reading file to load")?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .context("Read project file")?;

        let data: Project;
        if create_if_not_exists && contents.is_empty() {
            data = Project::default();
            ron::ser::to_writer_pretty(file, &data, Default::default())
                .context("Serialize project")?;
        } else {
            data = ron::de::from_str(&contents).context("Parsing JSON file")?;
        }

        Ok(Some(LoadedProject {
            data,
            path: path.to_owned(),
        }))
    };

    match inner() {
        Err(e) => {
            send_error_notification(ctx, format!("{:#}", e));
            None
        }
        Ok(r) => r,
    }
}

fn save_project(gui: &mut NesimgGui, ctx: &egui::Context) -> anyhow::Result<()> {
    let project_path = if let Some(path) = gui.state.loaded_project.get().map(|x| x.path) {
        path
    } else {
        return Ok(());
    };
    let project_data = if let Some(data) = gui.state.project.as_ref().map(|x| x.data.clone()) {
        data
    } else {
        return Ok(());
    };

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .create(true)
        .open(project_path)
        .context("Open file to save")?;

    ron::ser::to_writer_pretty(file, &project_data, Default::default())
        .context("Serialize project to RON")?;

    send_info_notification(ctx, "Save successful");

    Ok(())
}

/// Load the app icon that is built-in to the binary
fn load_icon() -> IconData {
    let icon_png_bytes = include_bytes!("../docs/logo.png");
    let image = image::load_from_memory(icon_png_bytes).expect("Load icon");
    let width = image.width();
    let height = image.height();
    let rgba = image.into_rgba8().as_raw().clone();

    IconData {
        rgba,
        width,
        height,
    }
}
