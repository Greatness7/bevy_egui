#[cfg(target_arch = "wasm32")]
use crate::text_agent::{is_mobile_safari, update_text_agent};
use crate::{
    helpers::{vec2_into_egui_pos2, QueryHelper},
    EguiContext, EguiContextSettings, EguiGlobalSettings, EguiInput, EguiOutput,
};
use bevy_ecs::{event::EventIterator, prelude::*, system::SystemParam};
use bevy_input::{
    keyboard::{Key, KeyCode, KeyboardFocusLost, KeyboardInput},
    mouse::{MouseButton, MouseButtonInput, MouseScrollUnit, MouseWheel},
    touch::TouchInput,
    ButtonInput, ButtonState,
};
use bevy_log::{self as log};
use bevy_time::{Real, Time};
use bevy_window::{CursorMoved, FileDragAndDrop, Ime, Window};
use egui::Modifiers;

/// Cached pointer position, used to populate [`egui::Event::PointerButton`] events.
#[derive(Component, Default)]
pub struct EguiContextPointerPosition {
    /// Pointer position.
    pub position: egui::Pos2,
}

/// Stores an active touch id.
#[derive(Component, Default)]
pub struct EguiContextPointerTouchId {
    /// Active touch id.
    pub pointer_touch_id: Option<u64>,
}

/// Indicates whether [IME](https://en.wikipedia.org/wiki/Input_method) is enabled or disabled to avoid sending event duplicates.
#[derive(Component, Default)]
pub struct EguiContextImeState {
    /// Indicates whether IME is enabled.
    pub has_sent_ime_enabled: bool,
    /// Indicates whether IME is currently allowed, i.e. if the virtual keyboard is shown.
    pub is_ime_allowed: bool,
}

#[derive(Event, BufferedEvent)]
/// Wraps Egui events emitted by [`crate::EguiInputSet`] systems.
pub struct EguiInputEvent {
    /// Context to pass an event to.
    pub context: Entity,
    /// Wrapped event.
    pub event: egui::Event,
}

#[derive(Event, BufferedEvent)]
/// Wraps [`bevy::FileDragAndDrop`](bevy_window::FileDragAndDrop) events emitted by [`crate::EguiInputSet`] systems.
pub struct EguiFileDragAndDropEvent {
    /// Context to pass an event to.
    pub context: Entity,
    /// Wrapped event.
    pub event: FileDragAndDrop,
}

#[derive(Resource, Clone)]
/// Insert this resource when a pointer hovers over a non-window (e.g. world-space) [`EguiContext`] entity.
/// Also, make sure to update an [`EguiContextPointerPosition`] component of a hovered entity.
/// Both updates should happen during [`crate::EguiInputSet::InitReading`].
///
/// To learn how `bevy_egui` uses this resource, see the [`FocusedNonWindowEguiContext`] documentation.
pub struct HoveredNonWindowEguiContext(pub Entity);

/// Stores an entity of a focused non-window context (to push keyboard events to).
///
/// The resource won't exist if no context is focused, [`Option<Res<FocusedNonWindowEguiContext>>`] must be used to read from it.
/// If the [`HoveredNonWindowEguiContext`] resource exists, the [`FocusedNonWindowEguiContext`]
/// resource will get inserted on mouse button press or touch start event
/// (and removed if no hovered non-window context exists respectively).
///
/// Atm, it's up to users to update [`HoveredNonWindowEguiContext`] and [`EguiContextPointerPosition`].
/// We might be able to add proper `bevy_picking` support for world space UI once [`bevy_picking::backend::HitData`]
/// starts exposing triangle index or UV.
///
/// Updating focused contexts happens during [`crate::EguiInputSet::FocusContext`],
/// see [`write_pointer_button_events_system`] and [`write_window_touch_events_system`].
#[derive(Resource, Clone)]
pub struct FocusedNonWindowEguiContext(pub Entity);

/// Stores "pressed" state of modifier keys.
#[derive(Resource, Clone, Copy, Debug)]
pub struct ModifierKeysState {
    /// Indicates whether the [`Key::Shift`] key is pressed.
    pub shift: bool,
    /// Indicates whether the [`Key::Control`] key is pressed.
    pub ctrl: bool,
    /// Indicates whether the [`Key::Alt`] key is pressed.
    pub alt: bool,
    /// Indicates whether the [`Key::Super`] (or [`Key::Meta`]) key is pressed.
    pub win: bool,
    is_macos: bool,
}

impl Default for ModifierKeysState {
    fn default() -> Self {
        let mut state = Self {
            shift: false,
            ctrl: false,
            alt: false,
            win: false,
            is_macos: false,
        };

        #[cfg(not(target_arch = "wasm32"))]
        {
            state.is_macos = cfg!(target_os = "macos");
        }

        #[cfg(target_arch = "wasm32")]
        if let Some(window) = web_sys::window() {
            let nav = window.navigator();
            if let Ok(user_agent) = nav.user_agent() {
                if user_agent.to_ascii_lowercase().contains("mac") {
                    state.is_macos = true;
                }
            }
        }

        state
    }
}

impl ModifierKeysState {
    /// Converts the struct to [`egui::Modifiers`].
    pub fn to_egui_modifiers(&self) -> egui::Modifiers {
        egui::Modifiers {
            alt: self.alt,
            ctrl: self.ctrl,
            shift: self.shift,
            mac_cmd: if self.is_macos { self.win } else { false },
            command: if self.is_macos { self.win } else { self.ctrl },
        }
    }

    /// Returns `true` if modifiers shouldn't prevent text input (we don't want to put characters on pressing Ctrl+A, etc).
    pub fn text_input_is_allowed(&self) -> bool {
        // Ctrl + Alt enables AltGr which is used to print special characters.
        !self.win && !self.ctrl || !self.is_macos && self.ctrl && self.alt
    }

    fn reset(&mut self) {
        self.shift = false;
        self.ctrl = false;
        self.alt = false;
        self.win = false;
    }
}

#[derive(Resource, Default)]
/// A bidirectional map between [`Window`] and [`EguiContext`] entities.
/// Multiple contexts may belong to a single window.
pub struct WindowToEguiContextMap {
    /// Indexes contexts by windows.
    pub window_to_contexts:
        bevy_platform::collections::HashMap<Entity, bevy_platform::collections::HashSet<Entity>>,
    /// Indexes windows by contexts.
    pub context_to_window: bevy_platform::collections::HashMap<Entity, Entity>,
}

#[cfg(feature = "render")]
impl WindowToEguiContextMap {
    /// Adds a context to the map on creation.
    pub fn on_egui_context_added_system(
        mut res: ResMut<Self>,
        added_contexts: Query<(Entity, &bevy_render::camera::Camera), Added<EguiContext>>,
        primary_window: Query<Entity, With<bevy_window::PrimaryWindow>>,
    ) {
        for (egui_context_entity, camera) in added_contexts {
            if let Some(bevy_render::camera::NormalizedRenderTarget::Window(window_ref)) =
                camera.target.normalize(primary_window.single().ok())
            {
                res.window_to_contexts
                    .entry(window_ref.entity())
                    .or_default()
                    .insert(egui_context_entity);
                res.context_to_window
                    .insert(egui_context_entity, window_ref.entity());
            }
        }
    }

    /// Removes a context from the map on removal.
    pub fn on_egui_context_removed_system(
        mut res: ResMut<Self>,
        mut removed_contexts: RemovedComponents<EguiContext>,
    ) {
        for egui_context_entity in removed_contexts.read() {
            let Some(window_entity) = res.context_to_window.remove(&egui_context_entity) else {
                continue;
            };

            let Some(window_contexts) = res.window_to_contexts.get_mut(&window_entity) else {
                log::warn!(
                    "A destroyed Egui context's window isn't registered: {egui_context_entity:?}"
                );
                continue;
            };

            window_contexts.remove(&egui_context_entity);
        }
    }
}

/// Iterates over pairs of `(Event, Entity)`, where the entity points to the context that the event is related to.
pub struct EguiContextsEventIterator<'a, E: BufferedEvent, F> {
    event_iter: EventIterator<'a, E>,
    map_event_to_window_id_f: F,
    current_event: Option<&'a E>,
    current_event_contexts: Vec<Entity>,
    non_window_context: Option<Entity>,
    map: &'a WindowToEguiContextMap,
}

impl<'a, E: BufferedEvent, F: FnMut(&'a E) -> Entity> Iterator
    for EguiContextsEventIterator<'a, E, F>
{
    type Item = (&'a E, Entity);

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_event_contexts.is_empty() {
            self.current_event = None;
        }

        if self.current_event.is_none() {
            self.current_event = self.event_iter.next();

            if self.non_window_context.is_some() {
                return self.current_event.zip(self.non_window_context);
            }

            if let Some(current) = self.current_event {
                if let Some(contexts) = self
                    .map
                    .window_to_contexts
                    .get(&(self.map_event_to_window_id_f)(current))
                {
                    self.current_event_contexts = contexts.iter().cloned().collect();
                }
            }
        }

        self.current_event.zip(self.current_event_contexts.pop())
    }
}

#[derive(SystemParam)]
/// A helper system param to iterate over pairs of events and Egui contexts, see [`EguiContextsEventIterator`].
pub struct EguiContextEventReader<'w, 's, E: BufferedEvent> {
    event_reader: EventReader<'w, 's, E>,
    map: Res<'w, WindowToEguiContextMap>,
    hovered_non_window_egui_context: Option<Res<'w, HoveredNonWindowEguiContext>>,
    focused_non_window_egui_context: Option<Res<'w, FocusedNonWindowEguiContext>>,
}

impl<'w, 's, E: BufferedEvent> EguiContextEventReader<'w, 's, E> {
    /// Returns [`EguiContextsEventIterator`] that iterates only over window events (i.e. skips contexts that render to images, etc.),
    /// expects a lambda that extracts a window id from an event.
    pub fn read<'a, F>(
        &'a mut self,
        map_event_to_window_id_f: F,
    ) -> EguiContextsEventIterator<'a, E, F>
    where
        F: FnMut(&'a E) -> Entity,
        E: BufferedEvent,
    {
        EguiContextsEventIterator {
            event_iter: self.event_reader.read(),
            map_event_to_window_id_f,
            current_event: None,
            current_event_contexts: Vec::new(),
            non_window_context: None,
            map: &self.map,
        }
    }

    /// Returns [`EguiContextsEventIterator`] that iterates over window events but might substitute contexts with a currently hovered non-window context (see [`HoveredNonWindowEguiContext`]), expects a lambda that extracts a window id from an event.
    pub fn read_with_non_window_hovered<'a, F>(
        &'a mut self,
        map_event_to_window_id_f: F,
    ) -> EguiContextsEventIterator<'a, E, F>
    where
        F: FnMut(&'a E) -> Entity,
        E: BufferedEvent,
    {
        EguiContextsEventIterator {
            event_iter: self.event_reader.read(),
            map_event_to_window_id_f,
            current_event: None,
            current_event_contexts: Vec::new(),
            non_window_context: self
                .hovered_non_window_egui_context
                .as_deref()
                .map(|context| context.0),
            map: &self.map,
        }
    }

    /// Returns [`EguiContextsEventIterator`] that iterates over window events but might substitute contexts with a currently focused non-window context (see [`FocusedNonWindowEguiContext`]), expects a lambda that extracts a window id from an event.
    pub fn read_with_non_window_focused<'a, F>(
        &'a mut self,
        map_event_to_window_id_f: F,
    ) -> EguiContextsEventIterator<'a, E, F>
    where
        F: FnMut(&'a E) -> Entity,
        E: Event,
    {
        EguiContextsEventIterator {
            event_iter: self.event_reader.read(),
            map_event_to_window_id_f,
            current_event: None,
            current_event_contexts: Vec::new(),
            non_window_context: self
                .focused_non_window_egui_context
                .as_deref()
                .map(|context| context.0),
            map: &self.map,
        }
    }
}

/// Reads [`KeyboardInput`] events to update the [`ModifierKeysState`] resource.
pub fn write_modifiers_keys_state_system(
    mut ev_keyboard_input: EventReader<KeyboardInput>,
    mut ev_focus: EventReader<KeyboardFocusLost>,
    mut modifier_keys_state: ResMut<ModifierKeysState>,
) {
    // If window focus is lost, clear all modifiers to avoid stuck keys.
    if !ev_focus.is_empty() {
        ev_focus.clear();
        modifier_keys_state.reset();
    }

    for event in ev_keyboard_input.read() {
        let KeyboardInput {
            logical_key, state, ..
        } = event;
        match logical_key {
            Key::Shift => {
                modifier_keys_state.shift = state.is_pressed();
            }
            Key::Control => {
                modifier_keys_state.ctrl = state.is_pressed();
            }
            Key::Alt => {
                modifier_keys_state.alt = state.is_pressed();
            }
            Key::Super | Key::Meta => {
                modifier_keys_state.win = state.is_pressed();
            }
            _ => {}
        };
    }
}

/// Reads [`MouseButtonInput`] events and wraps them into [`EguiInputEvent`] (only for window contexts).
pub fn write_window_pointer_moved_events_system(
    mut cursor_moved_reader: EguiContextEventReader<CursorMoved>,
    mut egui_input_event_writer: EventWriter<EguiInputEvent>,
    mut egui_contexts: Query<
        (&EguiContextSettings, &mut EguiContextPointerPosition),
        With<EguiContext>,
    >,
) {
    for (event, context) in cursor_moved_reader.read(|event| event.window) {
        let Some((context_settings, mut context_pointer_position)) =
            egui_contexts.get_some_mut(context)
        else {
            continue;
        };

        if !context_settings
            .input_system_settings
            .run_write_window_pointer_moved_events_system
        {
            continue;
        }

        let scale_factor = context_settings.scale_factor;
        let pointer_position = vec2_into_egui_pos2(event.position / scale_factor);
        context_pointer_position.position = pointer_position;
        egui_input_event_writer.write(EguiInputEvent {
            context,
            event: egui::Event::PointerMoved(pointer_position),
        });
    }
}

/// Reads [`MouseButtonInput`] events and wraps them into [`EguiInputEvent`], can redirect events to [`HoveredNonWindowEguiContext`],
/// inserts, updates or removes the [`FocusedNonWindowEguiContext`] resource based on a hovered context.
pub fn write_pointer_button_events_system(
    egui_global_settings: Res<EguiGlobalSettings>,
    mut commands: Commands,
    modifier_keys_state: Res<ModifierKeysState>,
    mut mouse_button_input_reader: EguiContextEventReader<MouseButtonInput>,
    mut egui_input_event_writer: EventWriter<EguiInputEvent>,
    egui_contexts: Query<(&EguiContextSettings, &EguiContextPointerPosition), With<EguiContext>>,
) {
    let modifiers = modifier_keys_state.to_egui_modifiers();
    let hovered_non_window_egui_context = mouse_button_input_reader
        .hovered_non_window_egui_context
        .as_deref()
        .cloned();
    for (event, context) in
        mouse_button_input_reader.read_with_non_window_hovered(|event| event.window)
    {
        let Some((context_settings, context_pointer_position)) = egui_contexts.get_some(context)
        else {
            continue;
        };

        if !context_settings
            .input_system_settings
            .run_write_pointer_button_events_system
        {
            continue;
        }

        let button = match event.button {
            MouseButton::Left => Some(egui::PointerButton::Primary),
            MouseButton::Right => Some(egui::PointerButton::Secondary),
            MouseButton::Middle => Some(egui::PointerButton::Middle),
            MouseButton::Back => Some(egui::PointerButton::Extra1),
            MouseButton::Forward => Some(egui::PointerButton::Extra2),
            _ => None,
        };
        let Some(button) = button else {
            continue;
        };
        let pressed = match event.state {
            ButtonState::Pressed => true,
            ButtonState::Released => false,
        };
        egui_input_event_writer.write(EguiInputEvent {
            context,
            event: egui::Event::PointerButton {
                pos: context_pointer_position.position,
                button,
                pressed,
                modifiers,
            },
        });

        // If we are hovering over some UI in world space, we want to mark it as focused on mouse click.
        if egui_global_settings.enable_focused_non_window_context_updates && pressed {
            if let Some(hovered_non_window_egui_context) = &hovered_non_window_egui_context {
                commands.insert_resource(FocusedNonWindowEguiContext(
                    hovered_non_window_egui_context.0,
                ));
            } else {
                commands.remove_resource::<FocusedNonWindowEguiContext>();
            }
        }
    }
}

/// Reads [`CursorMoved`] events and wraps them into [`EguiInputEvent`] for a [`HoveredNonWindowEguiContext`] context (if one exists).
pub fn write_non_window_pointer_moved_events_system(
    hovered_non_window_egui_context: Option<Res<HoveredNonWindowEguiContext>>,
    mut cursor_moved_reader: EventReader<CursorMoved>,
    mut egui_input_event_writer: EventWriter<EguiInputEvent>,
    egui_contexts: Query<(&EguiContextSettings, &EguiContextPointerPosition), With<EguiContext>>,
) {
    if cursor_moved_reader.is_empty() {
        return;
    }

    cursor_moved_reader.clear();
    let Some(HoveredNonWindowEguiContext(hovered_non_window_egui_context)) =
        hovered_non_window_egui_context.as_deref()
    else {
        return;
    };

    let Some((context_settings, context_pointer_position)) =
        egui_contexts.get_some(*hovered_non_window_egui_context)
    else {
        return;
    };

    if !context_settings
        .input_system_settings
        .run_write_non_window_pointer_moved_events_system
    {
        return;
    }

    egui_input_event_writer.write(EguiInputEvent {
        context: *hovered_non_window_egui_context,
        event: egui::Event::PointerMoved(context_pointer_position.position),
    });
}

/// Reads [`MouseWheel`] events and wraps them into [`EguiInputEvent`], can redirect events to [`HoveredNonWindowEguiContext`].
pub fn write_mouse_wheel_events_system(
    modifier_keys_state: Res<ModifierKeysState>,
    mut mouse_wheel_reader: EguiContextEventReader<MouseWheel>,
    mut egui_input_event_writer: EventWriter<EguiInputEvent>,
    egui_contexts: Query<&EguiContextSettings, With<EguiContext>>,
) {
    let modifiers = modifier_keys_state.to_egui_modifiers();
    for (event, context) in mouse_wheel_reader.read_with_non_window_hovered(|event| event.window) {
        let delta = egui::vec2(event.x, event.y);
        let unit = match event.unit {
            MouseScrollUnit::Line => egui::MouseWheelUnit::Line,
            MouseScrollUnit::Pixel => egui::MouseWheelUnit::Point,
        };

        let Some(context_settings) = egui_contexts.get_some(context) else {
            continue;
        };

        if !context_settings
            .input_system_settings
            .run_write_mouse_wheel_events_system
        {
            continue;
        }

        egui_input_event_writer.write(EguiInputEvent {
            context,
            event: egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
            },
        });
    }
}

/// Reads [`KeyboardInput`] events and wraps them into [`EguiInputEvent`], can redirect events to [`FocusedNonWindowEguiContext`].
pub fn write_keyboard_input_events_system(
    modifier_keys_state: Res<ModifierKeysState>,
    #[cfg(all(
        feature = "manage_clipboard",
        not(target_os = "android"),
        not(target_arch = "wasm32")
    ))]
    mut egui_clipboard: ResMut<crate::EguiClipboard>,
    mut keyboard_input_reader: EguiContextEventReader<KeyboardInput>,
    mut egui_input_event_writer: EventWriter<EguiInputEvent>,
    egui_contexts: Query<&EguiContextSettings, With<EguiContext>>,
) {
    let modifiers = modifier_keys_state.to_egui_modifiers();
    for (event, context) in keyboard_input_reader.read_with_non_window_focused(|event| event.window)
    {
        let Some(context_settings) = egui_contexts.get_some(context) else {
            continue;
        };

        if !context_settings
            .input_system_settings
            .run_write_keyboard_input_events_system
        {
            continue;
        }

        if modifier_keys_state.text_input_is_allowed() && event.state.is_pressed() {
            match &event.logical_key {
                Key::Character(char) if char.matches(char::is_control).count() == 0 => {
                    egui_input_event_writer.write(EguiInputEvent {
                        context,
                        event: egui::Event::Text(char.to_string()),
                    });
                }
                Key::Space => {
                    egui_input_event_writer.write(EguiInputEvent {
                        context,
                        event: egui::Event::Text(" ".to_string()),
                    });
                }
                _ => (),
            }
        }

        let key = crate::helpers::bevy_to_egui_key(&event.logical_key);
        let physical_key = crate::helpers::bevy_to_egui_physical_key(&event.key_code);

        // "Logical OR physical key" is a fallback mechanism for keyboard layouts without Latin characters
        // See: https://github.com/emilk/egui/blob/66c73b9cbfbd4d44489fc6f6a840d7d82bc34389/crates/egui-winit/src/lib.rs#L760
        let (Some(key), physical_key) = (key.or(physical_key), physical_key) else {
            continue;
        };

        let egui_event = egui::Event::Key {
            key,
            pressed: event.state.is_pressed(),
            repeat: false,
            modifiers,
            physical_key,
        };
        egui_input_event_writer.write(EguiInputEvent {
            context,
            event: egui_event,
        });

        // We also check that it's a `ButtonState::Pressed` event, as we don't want to
        // copy, cut or paste on the key release.
        #[cfg(all(
            feature = "manage_clipboard",
            not(target_os = "android"),
            not(target_arch = "wasm32")
        ))]
        if modifiers.command && event.state.is_pressed() {
            match key {
                egui::Key::C => {
                    egui_input_event_writer.write(EguiInputEvent {
                        context,
                        event: egui::Event::Copy,
                    });
                }
                egui::Key::X => {
                    egui_input_event_writer.write(EguiInputEvent {
                        context,
                        event: egui::Event::Cut,
                    });
                }
                egui::Key::V => {
                    if let Some(contents) = egui_clipboard.get_text() {
                        egui_input_event_writer.write(EguiInputEvent {
                            context,
                            event: egui::Event::Text(contents),
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

/// Reads [`Ime`] events and wraps them into [`EguiInputEvent`], can redirect events to [`FocusedNonWindowEguiContext`].
pub fn write_ime_events_system(
    mut ime_reader: EguiContextEventReader<Ime>,
    mut egui_input_event_writer: EventWriter<EguiInputEvent>,
    mut egui_contexts: Query<
        (
            Entity,
            &EguiContextSettings,
            &mut EguiContextImeState,
            &EguiOutput,
        ),
        With<EguiContext>,
    >,
) {
    for (event, context) in ime_reader.read_with_non_window_focused(|event| match &event {
        Ime::Preedit { window, .. }
        | Ime::Commit { window, .. }
        | Ime::Disabled { window }
        | Ime::Enabled { window } => *window,
    }) {
        let Some((_entity, context_settings, mut ime_state, _egui_output)) =
            egui_contexts.get_some_mut(context)
        else {
            continue;
        };

        if !context_settings
            .input_system_settings
            .run_write_ime_events_system
        {
            continue;
        }

        let ime_event_enable =
            |ime_state: &mut EguiContextImeState,
             egui_input_event_writer: &mut EventWriter<EguiInputEvent>| {
                if !ime_state.has_sent_ime_enabled {
                    egui_input_event_writer.write(EguiInputEvent {
                        context,
                        event: egui::Event::Ime(egui::ImeEvent::Enabled),
                    });
                    ime_state.has_sent_ime_enabled = true;
                }
            };

        let ime_event_disable =
            |ime_state: &mut EguiContextImeState,
             egui_input_event_writer: &mut EventWriter<EguiInputEvent>| {
                if !ime_state.has_sent_ime_enabled {
                    egui_input_event_writer.write(EguiInputEvent {
                        context,
                        event: egui::Event::Ime(egui::ImeEvent::Disabled),
                    });
                    ime_state.has_sent_ime_enabled = false;
                }
            };

        // Aligned with the egui-winit implementation: https://github.com/emilk/egui/blob/0f2b427ff4c0a8c68f6622ec7d0afb7ba7e71bba/crates/egui-winit/src/lib.rs#L348
        match event {
            Ime::Enabled { window: _ } => {
                ime_event_enable(&mut ime_state, &mut egui_input_event_writer);
            }
            Ime::Preedit {
                value,
                window: _,
                cursor: _,
            } => {
                ime_event_enable(&mut ime_state, &mut egui_input_event_writer);
                egui_input_event_writer.write(EguiInputEvent {
                    context,
                    event: egui::Event::Ime(egui::ImeEvent::Preedit(value.clone())),
                });
            }
            Ime::Commit { value, window: _ } => {
                egui_input_event_writer.write(EguiInputEvent {
                    context,
                    event: egui::Event::Ime(egui::ImeEvent::Commit(value.clone())),
                });
                ime_event_disable(&mut ime_state, &mut egui_input_event_writer);
            }
            Ime::Disabled { window: _ } => {
                ime_event_disable(&mut ime_state, &mut egui_input_event_writer);
            }
        }
    }
}

/// Show the virtual keyboard when a text input is focused.
/// Works by reading [`EguiOutput`] and calling `Window::set_ime_allowed` if the `ime` field is set.
#[cfg(any(target_os = "ios", target_os = "android"))]
pub fn set_ime_allowed_system(
    mut egui_context: Query<(&EguiOutput, &mut EguiContextImeState)>,
    windows: Query<Entity, With<bevy_window::PrimaryWindow>>,
    winit_windows: NonSendMut<bevy_winit::WinitWindows>,
) {
    // We are on mobile, so we expect a single window.
    let Ok(window) = windows.single() else {
        return;
    };

    let Some(winit_window) = winit_windows.get_window(window) else {
        log::warn!(
            "Cannot access an underlying winit window for a window entity {}",
            window
        );

        return;
    };

    let Ok((egui_output, mut egui_ime_state)) = egui_context.single_mut() else {
        return;
    };

    let ime_allowed = egui_output.platform_output.ime.is_some();
    if ime_allowed != egui_ime_state.is_ime_allowed {
        winit_window.set_ime_allowed(ime_allowed);
        egui_ime_state.is_ime_allowed = ime_allowed;
    }
}

/// Reads [`FileDragAndDrop`] events and wraps them into [`EguiFileDragAndDropEvent`], can redirect events to [`HoveredNonWindowEguiContext`].
pub fn write_file_dnd_events_system(
    mut dnd_reader: EguiContextEventReader<FileDragAndDrop>,
    mut egui_file_dnd_event_writer: EventWriter<EguiFileDragAndDropEvent>,
    egui_contexts: Query<&EguiContextSettings, With<EguiContext>>,
) {
    for (event, context) in dnd_reader.read_with_non_window_hovered(|event| match &event {
        FileDragAndDrop::DroppedFile { window, .. }
        | FileDragAndDrop::HoveredFile { window, .. }
        | FileDragAndDrop::HoveredFileCanceled { window } => *window,
    }) {
        let Some(context_settings) = egui_contexts.get_some(context) else {
            continue;
        };

        if !context_settings
            .input_system_settings
            .run_write_file_dnd_events_system
        {
            continue;
        }

        match event {
            FileDragAndDrop::DroppedFile { window, path_buf } => {
                egui_file_dnd_event_writer.write(EguiFileDragAndDropEvent {
                    context,
                    event: FileDragAndDrop::DroppedFile {
                        window: *window,
                        path_buf: path_buf.clone(),
                    },
                });
            }
            FileDragAndDrop::HoveredFile { window, path_buf } => {
                egui_file_dnd_event_writer.write(EguiFileDragAndDropEvent {
                    context,
                    event: FileDragAndDrop::HoveredFile {
                        window: *window,
                        path_buf: path_buf.clone(),
                    },
                });
            }
            FileDragAndDrop::HoveredFileCanceled { window } => {
                egui_file_dnd_event_writer.write(EguiFileDragAndDropEvent {
                    context,
                    event: FileDragAndDrop::HoveredFileCanceled { window: *window },
                });
            }
        }
    }
}

/// Reads [`TouchInput`] events and wraps them into [`EguiInputEvent`].
pub fn write_window_touch_events_system(
    mut commands: Commands,
    egui_global_settings: Res<EguiGlobalSettings>,
    modifier_keys_state: Res<ModifierKeysState>,
    mut touch_input_reader: EguiContextEventReader<TouchInput>,
    mut egui_input_event_writer: EventWriter<EguiInputEvent>,
    mut egui_contexts: Query<
        (
            &EguiContextSettings,
            &mut EguiContextPointerPosition,
            &mut EguiContextPointerTouchId,
            &EguiOutput,
        ),
        With<EguiContext>,
    >,
) {
    let modifiers = modifier_keys_state.to_egui_modifiers();
    let hovered_non_window_egui_context = touch_input_reader
        .hovered_non_window_egui_context
        .as_deref()
        .cloned();

    for (event, context) in touch_input_reader.read(|event| event.window) {
        let Some((
            context_settings,
            mut context_pointer_position,
            mut context_pointer_touch_id,
            output,
        )) = egui_contexts.get_some_mut(context)
        else {
            continue;
        };

        if egui_global_settings.enable_focused_non_window_context_updates {
            if let bevy_input::touch::TouchPhase::Started = event.phase {
                if let Some(hovered_non_window_egui_context) = &hovered_non_window_egui_context {
                    if let bevy_input::touch::TouchPhase::Started = event.phase {
                        commands.insert_resource(FocusedNonWindowEguiContext(
                            hovered_non_window_egui_context.0,
                        ));
                    }

                    continue;
                }

                commands.remove_resource::<FocusedNonWindowEguiContext>();
            }
        }

        if !context_settings
            .input_system_settings
            .run_write_window_touch_events_system
        {
            continue;
        }

        let scale_factor = context_settings.scale_factor;
        let touch_position = vec2_into_egui_pos2(event.position / scale_factor);
        context_pointer_position.position = touch_position;
        write_touch_event(
            &mut egui_input_event_writer,
            event,
            context,
            output,
            touch_position,
            modifiers,
            &mut context_pointer_touch_id,
        );
    }
}

/// Reads [`TouchInput`] events and wraps them into [`EguiInputEvent`] for a [`HoveredNonWindowEguiContext`] context (if one exists).
pub fn write_non_window_touch_events_system(
    focused_non_window_egui_context: Option<Res<FocusedNonWindowEguiContext>>,
    mut touch_input_reader: EventReader<TouchInput>,
    mut egui_input_event_writer: EventWriter<EguiInputEvent>,
    modifier_keys_state: Res<ModifierKeysState>,
    mut egui_contexts: Query<
        (
            &EguiContextSettings,
            &EguiContextPointerPosition,
            &mut EguiContextPointerTouchId,
            &EguiOutput,
        ),
        With<EguiContext>,
    >,
) {
    let modifiers = modifier_keys_state.to_egui_modifiers();
    for event in touch_input_reader.read() {
        let Some(&FocusedNonWindowEguiContext(focused_non_window_egui_context)) =
            focused_non_window_egui_context.as_deref()
        else {
            continue;
        };

        let Some((
            context_settings,
            context_pointer_position,
            mut context_pointer_touch_id,
            output,
        )) = egui_contexts.get_some_mut(focused_non_window_egui_context)
        else {
            continue;
        };

        if !context_settings
            .input_system_settings
            .run_write_non_window_touch_events_system
        {
            continue;
        }

        write_touch_event(
            &mut egui_input_event_writer,
            event,
            focused_non_window_egui_context,
            output,
            context_pointer_position.position,
            modifiers,
            &mut context_pointer_touch_id,
        );
    }
}

fn write_touch_event(
    egui_input_event_writer: &mut EventWriter<EguiInputEvent>,
    event: &TouchInput,
    context: Entity,
    _output: &EguiOutput,
    pointer_position: egui::Pos2,
    modifiers: Modifiers,
    context_pointer_touch_id: &mut EguiContextPointerTouchId,
) {
    let touch_id = egui::TouchId::from(event.id);

    // Emit the touch event.
    egui_input_event_writer.write(EguiInputEvent {
        context,
        event: egui::Event::Touch {
            device_id: egui::TouchDeviceId(event.window.to_bits()),
            id: touch_id,
            phase: match event.phase {
                bevy_input::touch::TouchPhase::Started => egui::TouchPhase::Start,
                bevy_input::touch::TouchPhase::Moved => egui::TouchPhase::Move,
                bevy_input::touch::TouchPhase::Ended => egui::TouchPhase::End,
                bevy_input::touch::TouchPhase::Canceled => egui::TouchPhase::Cancel,
            },
            pos: pointer_position,
            force: match event.force {
                Some(bevy_input::touch::ForceTouch::Normalized(force)) => Some(force as f32),
                Some(bevy_input::touch::ForceTouch::Calibrated {
                    force,
                    max_possible_force,
                    ..
                }) => Some((force / max_possible_force) as f32),
                None => None,
            },
        },
    });

    // If we're not yet translating a touch, or we're translating this very
    // touch, …
    if context_pointer_touch_id.pointer_touch_id.is_none()
        || context_pointer_touch_id.pointer_touch_id.unwrap() == event.id
    {
        // … emit PointerButton resp. PointerMoved events to emulate mouse.
        match event.phase {
            bevy_input::touch::TouchPhase::Started => {
                context_pointer_touch_id.pointer_touch_id = Some(event.id);
                // First move the pointer to the right location.
                egui_input_event_writer.write(EguiInputEvent {
                    context,
                    event: egui::Event::PointerMoved(pointer_position),
                });
                // Then do mouse button input.
                egui_input_event_writer.write(EguiInputEvent {
                    context,
                    event: egui::Event::PointerButton {
                        pos: pointer_position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers,
                    },
                });
            }
            bevy_input::touch::TouchPhase::Moved => {
                egui_input_event_writer.write(EguiInputEvent {
                    context,
                    event: egui::Event::PointerMoved(pointer_position),
                });
            }
            bevy_input::touch::TouchPhase::Ended => {
                context_pointer_touch_id.pointer_touch_id = None;
                egui_input_event_writer.write(EguiInputEvent {
                    context,
                    event: egui::Event::PointerButton {
                        pos: pointer_position,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers,
                    },
                });
                egui_input_event_writer.write(EguiInputEvent {
                    context,
                    event: egui::Event::PointerGone,
                });

                #[cfg(target_arch = "wasm32")]
                if !is_mobile_safari() {
                    update_text_agent(
                        _output.platform_output.ime.is_some()
                            || _output.platform_output.mutable_text_under_cursor,
                    );
                }
            }
            bevy_input::touch::TouchPhase::Canceled => {
                context_pointer_touch_id.pointer_touch_id = None;
                egui_input_event_writer.write(EguiInputEvent {
                    context,
                    event: egui::Event::PointerGone,
                });
            }
        }
    }
}

/// Reads both [`EguiFileDragAndDropEvent`] and [`EguiInputEvent`] events and feeds them to Egui.
#[allow(clippy::too_many_arguments)]
pub fn write_egui_input_system(
    focused_non_window_egui_context: Option<Res<FocusedNonWindowEguiContext>>,
    window_to_egui_context_map: Res<WindowToEguiContextMap>,
    modifier_keys_state: Res<ModifierKeysState>,
    mut egui_input_event_reader: EventReader<EguiInputEvent>,
    mut egui_file_dnd_event_reader: EventReader<EguiFileDragAndDropEvent>,
    mut egui_contexts: Query<(Entity, &mut EguiInput)>,
    windows: Query<&Window>,
    time: Res<Time<Real>>,
) {
    for EguiInputEvent { context, event } in egui_input_event_reader.read() {
        #[cfg(feature = "log_input_events")]
        log::warn!("{context:?}: {event:?}");

        let (_, mut egui_input) = match egui_contexts.get_mut(*context) {
            Ok(egui_input) => egui_input,
            Err(err) => {
                log::error!(
                    "Failed to get an Egui context ({context:?}) for an event ({event:?}): {err:?}"
                );
                continue;
            }
        };

        egui_input.events.push(event.clone());
    }

    for EguiFileDragAndDropEvent { context, event } in egui_file_dnd_event_reader.read() {
        #[cfg(feature = "log_file_dnd_events")]
        log::warn!("{context:?}: {event:?}");

        let (_, mut egui_input) = match egui_contexts.get_mut(*context) {
            Ok(egui_input) => egui_input,
            Err(err) => {
                log::error!(
                    "Failed to get an Egui context ({context:?}) for an event ({event:?}): {err:?}"
                );
                continue;
            }
        };

        match event {
            FileDragAndDrop::DroppedFile {
                window: _,
                path_buf,
            } => {
                egui_input.hovered_files.clear();
                egui_input.dropped_files.push(egui::DroppedFile {
                    path: Some(path_buf.clone()),
                    ..Default::default()
                });
            }
            FileDragAndDrop::HoveredFile {
                window: _,
                path_buf,
            } => {
                egui_input.hovered_files.push(egui::HoveredFile {
                    path: Some(path_buf.clone()),
                    ..Default::default()
                });
            }
            FileDragAndDrop::HoveredFileCanceled { window: _ } => {
                egui_input.hovered_files.clear();
            }
        }
    }

    for (entity, mut egui_input) in egui_contexts.iter_mut() {
        egui_input.focused = focused_non_window_egui_context.as_deref().map_or_else(
            || {
                window_to_egui_context_map
                    .context_to_window
                    .get(&entity)
                    .and_then(|window_entity| windows.get_some(*window_entity))
                    .is_some_and(|window| window.focused)
            },
            |context| context.0 == entity,
        );
        egui_input.modifiers = modifier_keys_state.to_egui_modifiers();
        egui_input.time = Some(time.elapsed_secs_f64());
    }
}

/// Clears Bevy input event buffers and resets [`ButtonInput`] resources if Egui
/// is using pointer or keyboard (see the [`write_egui_wants_input_system`] run condition).
///
/// This system isn't run by default, set [`EguiGlobalSettings::enable_absorb_bevy_input_system`]
/// to `true` to enable it.
///
/// ## Considerations
///
/// Enabling this system makes an assumption that `bevy_egui` takes priority in input handling
/// over other plugins and systems. This should work ok as long as there's no other system
/// clearing events the same way that might be in conflict with `bevy_egui`, and there's
/// no other system that needs a non-interrupted flow of events.
///
/// ## Alternative
///
/// A safer alternative is to apply `run_if(not(egui_wants_any_pointer_input))` or `run_if(not(egui_wants_any_keyboard_input))` to your systems
/// that need to be disabled while Egui is using input (see the [`egui_wants_any_pointer_input`], [`egui_wants_any_keyboard_input`] run conditions).
pub fn absorb_bevy_input_system(
    egui_wants_input: Res<EguiWantsInput>,
    mut mouse_input: ResMut<ButtonInput<MouseButton>>,
    mut keyboard_input: ResMut<ButtonInput<KeyCode>>,
    mut keyboard_input_events: ResMut<Events<KeyboardInput>>,
    mut mouse_wheel_events: ResMut<Events<MouseWheel>>,
    mut mouse_button_input_events: ResMut<Events<MouseButtonInput>>,
) {
    let modifiers = [
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::AltLeft,
        KeyCode::AltRight,
        KeyCode::ShiftLeft,
        KeyCode::ShiftRight,
    ];

    let pressed = modifiers.map(|key| keyboard_input.pressed(key).then_some(key));

    // TODO: the list of events is definitely not comprehensive, but it should at least cover
    //  the most popular use-cases. We can add more on request.
    if egui_wants_input.wants_any_keyboard_input() {
        keyboard_input.reset_all();
        keyboard_input_events.clear();
    }
    if egui_wants_input.wants_any_pointer_input() {
        mouse_input.reset_all();
        mouse_wheel_events.clear();
        mouse_button_input_events.clear();
    }

    for key in pressed.into_iter().flatten() {
        keyboard_input.press(key);
    }
}

/// Stores whether there's an Egui context using pointer or keyboard.
#[derive(Resource, Clone, Debug, Default)]
pub struct EguiWantsInput {
    is_pointer_over_area: bool,
    wants_pointer_input: bool,
    is_using_pointer: bool,
    wants_keyboard_input: bool,
    is_popup_open: bool,
}

impl EguiWantsInput {
    /// Is the pointer (mouse/touch) over any egui area?
    pub fn is_pointer_over_area(&self) -> bool {
        self.is_pointer_over_area
    }

    /// True if egui is currently interested in the pointer (mouse or touch).
    ///
    /// Could be the pointer is hovering over a [`egui::Window`] or the user is dragging a widget.
    /// If `false`, the pointer is outside of any egui area and so
    /// you may be interested in what it is doing (e.g. controlling your game).
    /// Returns `false` if a drag started outside of egui and then moved over an egui area.
    pub fn wants_pointer_input(&self) -> bool {
        self.wants_pointer_input
    }

    /// Is egui currently using the pointer position (e.g. dragging a slider)?
    ///
    /// NOTE: this will return `false` if the pointer is just hovering over an egui area.
    pub fn is_using_pointer(&self) -> bool {
        self.is_using_pointer
    }

    /// If `true`, egui is currently listening on text input (e.g. typing text in a [`egui::TextEdit`]).
    pub fn wants_keyboard_input(&self) -> bool {
        self.wants_keyboard_input
    }

    /// Is an egui context menu open?
    #[deprecated = "use is_popup_open, renamed upstream in egui"]
    pub fn is_context_menu_open(&self) -> bool {
        self.is_popup_open
    }

    /// Is an egui context menu open?
    pub fn is_popup_open(&self) -> bool {
        self.is_popup_open
    }

    /// Returns `true` if any of the following is true:
    /// [`EguiWantsInput::is_pointer_over_area`], [`EguiWantsInput::wants_pointer_input`], [`EguiWantsInput::is_using_pointer`], [`EguiWantsInput::is_context_menu_open`].
    pub fn wants_any_pointer_input(&self) -> bool {
        self.is_pointer_over_area
            || self.wants_pointer_input
            || self.is_using_pointer
            || self.is_popup_open
    }

    /// Returns `true` if any of the following is true:
    /// [`EguiWantsInput::wants_keyboard_input`], [`EguiWantsInput::is_context_menu_open`].
    pub fn wants_any_keyboard_input(&self) -> bool {
        self.wants_keyboard_input || self.is_popup_open
    }

    /// Returns `true` if any of the following is true:
    /// [`EguiWantsInput::wants_any_pointer_input`], [`EguiWantsInput::wants_any_keyboard_input`].
    pub fn wants_any_input(&self) -> bool {
        self.wants_any_pointer_input() || self.wants_any_keyboard_input()
    }

    fn reset(&mut self) {
        self.is_pointer_over_area = false;
        self.wants_pointer_input = false;
        self.is_using_pointer = false;
        self.wants_keyboard_input = false;
        self.is_popup_open = false;
    }
}

/// Updates the [`EguiWantsInput`] resource.
pub fn write_egui_wants_input_system(
    mut egui_context_query: Query<&mut EguiContext>,
    mut egui_wants_input: ResMut<EguiWantsInput>,
) {
    egui_wants_input.reset();

    for mut ctx in egui_context_query.iter_mut() {
        let egui_ctx = ctx.get_mut();
        egui_wants_input.is_pointer_over_area =
            egui_wants_input.is_pointer_over_area || egui_ctx.is_pointer_over_area();
        egui_wants_input.wants_pointer_input =
            egui_wants_input.wants_pointer_input || egui_ctx.wants_pointer_input();
        egui_wants_input.is_using_pointer =
            egui_wants_input.is_using_pointer || egui_ctx.is_using_pointer();
        egui_wants_input.wants_keyboard_input =
            egui_wants_input.wants_keyboard_input || egui_ctx.wants_keyboard_input();
        egui_wants_input.is_popup_open = egui_wants_input.is_popup_open || egui_ctx.is_popup_open();
    }
}

/// Returns `true` if any of the following is true:
/// [`EguiWantsInput::is_pointer_over_area`], [`EguiWantsInput::wants_pointer_input`], [`EguiWantsInput::is_using_pointer`], [`EguiWantsInput::is_context_menu_open`].
pub fn egui_wants_any_pointer_input(egui_wants_input_resource: Res<EguiWantsInput>) -> bool {
    egui_wants_input_resource.wants_any_pointer_input()
}

/// Returns `true` if any of the following is true:
/// [`EguiWantsInput::wants_keyboard_input`], [`EguiWantsInput::is_context_menu_open`].
pub fn egui_wants_any_keyboard_input(egui_wants_input_resource: Res<EguiWantsInput>) -> bool {
    egui_wants_input_resource.wants_any_keyboard_input()
}

/// Returns `true` if any of the following is true:
/// [`EguiWantsInput::wants_any_pointer_input`], [`EguiWantsInput::wants_any_keyboard_input`].
pub fn egui_wants_any_input(egui_wants_input_resource: Res<EguiWantsInput>) -> bool {
    egui_wants_input_resource.wants_any_input()
}
