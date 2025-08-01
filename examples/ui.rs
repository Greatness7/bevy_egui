use bevy::{
    log::{Level, LogPlugin},
    prelude::*,
};
use bevy_egui::{
    EguiContextSettings, EguiContexts, EguiPlugin, EguiPrimaryContextPass, EguiStartupSet,
};

struct Images {
    bevy_icon: Handle<Image>,
    bevy_icon_inverted: Handle<Image>,
}

impl FromWorld for Images {
    fn from_world(world: &mut World) -> Self {
        let asset_server = world.get_resource_mut::<AssetServer>().unwrap();
        Self {
            bevy_icon: asset_server.load("icon.png"),
            bevy_icon_inverted: asset_server.load("icon_inverted.png"),
        }
    }
}

/// This example demonstrates the following functionality and use-cases of bevy_egui:
/// - rendering loaded assets;
/// - toggling hidpi scaling (by pressing '/' button);
/// - configuring egui contexts during the startup.
fn main() {
    App::new()
        .insert_resource(ClearColor(Color::BLACK))
        .init_resource::<UiState>()
        .add_plugins(
            DefaultPlugins
                .set(LogPlugin {
                    filter: "warn,ui=info".to_string(),
                    level: Level::INFO,
                    ..Default::default()
                })
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        // You may want this set to `true` if you need virtual keyboard work in mobile browsers.
                        prevent_default_event_handling: false,
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins(EguiPlugin::default())
        .add_systems(
            PreStartup,
            setup_camera_system.before(EguiStartupSet::InitContexts),
        )
        .add_systems(
            Startup,
            (configure_visuals_system, configure_ui_state_system),
        )
        .add_systems(
            EguiPrimaryContextPass,
            (ui_example_system, update_ui_scale_factor_system),
        )
        .run();
}

fn setup_camera_system(mut commands: Commands) {
    commands.spawn(Camera2d);
}

#[derive(Default, Resource)]
struct UiState {
    label: String,
    value: f32,
    painting: Painting,
    inverted: bool,
    egui_texture_handle: Option<egui::TextureHandle>,
    is_window_open: bool,
}

fn configure_visuals_system(mut contexts: EguiContexts) -> Result {
    contexts.ctx_mut()?.set_visuals(egui::Visuals {
        window_corner_radius: 0.0.into(),
        ..Default::default()
    });
    Ok(())
}

fn configure_ui_state_system(mut ui_state: ResMut<UiState>) {
    ui_state.is_window_open = true;
}

fn update_ui_scale_factor_system(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut toggle_scale_factor: Local<Option<bool>>,
    egui_context: Single<(&mut EguiContextSettings, &Camera)>,
) {
    let (mut egui_settings, camera) = egui_context.into_inner();
    if keyboard_input.just_pressed(KeyCode::Slash) || toggle_scale_factor.is_none() {
        *toggle_scale_factor = Some(!toggle_scale_factor.unwrap_or(true));

        let scale_factor = if toggle_scale_factor.unwrap() {
            1.0
        } else {
            1.0 / camera.target_scaling_factor().unwrap_or(1.0)
        };
        egui_settings.scale_factor = scale_factor;
    }
}

fn ui_example_system(
    mut ui_state: ResMut<UiState>,
    // You are not required to store Egui texture ids in systems. We store this one here just to
    // demonstrate that rendering by using a texture id of a removed image is handled without
    // making bevy_egui panic.
    mut rendered_texture_id: Local<egui::TextureId>,
    mut is_initialized: Local<bool>,
    // If you need to access the ids from multiple systems, you can also initialize the `Images`
    // resource while building the app and use `Res<Images>` instead.
    images: Local<Images>,
    image_assets: ResMut<Assets<Image>>,
    mut contexts: EguiContexts,
) -> Result {
    if !*is_initialized {
        *is_initialized = true;
        *rendered_texture_id = contexts.add_image(images.bevy_icon.clone_weak());
    }

    let ctx = contexts.ctx_mut()?;

    let egui_texture_handle = ui_state
        .egui_texture_handle
        .get_or_insert_with(|| {
            ctx.load_texture(
                "example-image",
                egui::ColorImage::example(),
                Default::default(),
            )
        })
        .clone();

    let mut load = false;
    let mut copy = false;
    let mut remove = false;
    let mut invert = false;

    egui::SidePanel::left("side_panel")
        .default_width(200.0)
        .show(ctx, |ui| {
            ui.heading("Side Panel");

            ui.horizontal(|ui| {
                ui.label("Write something: ");
                ui.text_edit_singleline(&mut ui_state.label);
            });

            ui.add(egui::widgets::Image::new(egui::load::SizedTexture::new(
                egui_texture_handle.id(),
                egui_texture_handle.size_vec2(),
            )));

            ui.add(egui::Slider::new(&mut ui_state.value, 0.0..=10.0).text("value"));
            if ui.button("Increment").clicked() {
                ui_state.value += 1.0;
            }

            ui.allocate_space(egui::Vec2::new(1.0, 100.0));
            ui.horizontal(|ui| {
                load = ui.button("Load").clicked();
                copy = ui.button("Copy").clicked();
                invert = ui.button("Invert").clicked();
                remove = ui.button("Remove").clicked();
            });

            ui.add(egui::widgets::Image::new(egui::load::SizedTexture::new(
                *rendered_texture_id,
                [256.0, 256.0],
            )));

            ui.allocate_space(egui::Vec2::new(1.0, 10.0));
            ui.checkbox(&mut ui_state.is_window_open, "Window Is Open");

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add(egui::Hyperlink::from_label_and_url(
                    "powered by egui",
                    "https://github.com/emilk/egui/",
                ));
            });
        });

    egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
        // The top panel is often a good place for a menu bar:
        egui::MenuBar::new().ui(ui, |ui| {
            egui::containers::menu::MenuButton::new("File").ui(ui, |ui| {
                if ui.button("Quit").clicked() {
                    std::process::exit(0);
                }
            });
        });
    });

    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("Egui Template");
        ui.hyperlink("https://github.com/emilk/egui_template");
        ui.add(egui::github_link_file_line!(
            "https://github.com/vladbat00/bevy_egui/blob/main/",
            "Direct link to source code."
        ));
        egui::warn_if_debug_build(ui);

        ui.separator();

        ui.heading("Central Panel");
        ui.label("The central panel is the region left after adding TopPanels and SidePanels.");
        ui.label("It is often a great place for big things, like drawings:");

        ui.heading("Draw with your mouse to paint:");
        ui_state.painting.ui_control(ui);
        egui::Frame::dark_canvas(ui.style()).show(ui, |ui| {
            ui_state.painting.ui_content(ui);
        });
    });

    egui::Window::new("Window")
        .vscroll(true)
        .open(&mut ui_state.is_window_open)
        .show(ctx, |ui| {
            ui.label("Windows can be moved by dragging them.");
            ui.label("They are automatically sized based on contents.");
            ui.label("You can turn on resizing and scrolling if you like.");
            ui.label("You would normally chose either panels OR windows.");
        });

    if invert {
        ui_state.inverted = !ui_state.inverted;
    }
    let bevy_icon_handle = if ui_state.inverted {
        images.bevy_icon_inverted.clone_weak()
    } else {
        images.bevy_icon.clone_weak()
    };
    if load || invert {
        // If an image is already added to the context, it'll return an existing texture id.
        *rendered_texture_id = contexts.add_image(bevy_icon_handle.clone_weak());
    }
    if copy {
        let image = image_assets
            .get(&bevy_icon_handle)
            .expect("images should be created");

        contexts
            .ctx_mut()?
            .copy_image(egui::ColorImage::from_rgba_unmultiplied(
                image.size().to_array().map(|a| a as usize),
                image.data.as_ref().expect("image data"),
            ));
    }
    if remove {
        contexts.remove_image(&images.bevy_icon);
        contexts.remove_image(&images.bevy_icon_inverted);
    }
    Ok(())
}

struct Painting {
    lines: Vec<Vec<egui::Vec2>>,
    stroke: egui::Stroke,
}

impl Default for Painting {
    fn default() -> Self {
        Self {
            lines: Default::default(),
            stroke: egui::Stroke::new(1.0, egui::Color32::LIGHT_BLUE),
        }
    }
}

impl Painting {
    pub fn ui_control(&mut self, ui: &mut egui::Ui) -> egui::Response {
        ui.horizontal(|ui| {
            ui.add(&mut self.stroke);
            ui.separator();
            if ui.button("Clear Painting").clicked() {
                self.lines.clear();
            }
        })
        .response
    }

    pub fn ui_content(&mut self, ui: &mut egui::Ui) {
        let (response, painter) =
            ui.allocate_painter(ui.available_size_before_wrap(), egui::Sense::drag());
        let rect = response.rect;

        if self.lines.is_empty() {
            self.lines.push(vec![]);
        }

        let current_line = self.lines.last_mut().unwrap();

        if let Some(pointer_pos) = response.interact_pointer_pos() {
            let canvas_pos = pointer_pos - rect.min;
            if current_line.last() != Some(&canvas_pos) {
                current_line.push(canvas_pos);
            }
        } else if !current_line.is_empty() {
            self.lines.push(vec![]);
        }

        for line in &self.lines {
            if line.len() >= 2 {
                let points: Vec<egui::Pos2> = line.iter().map(|p| rect.min + *p).collect();
                painter.add(egui::Shape::line(points, self.stroke));
            }
        }
    }
}
