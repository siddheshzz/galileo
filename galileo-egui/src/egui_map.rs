//! Galileo map widget for EGUI framework. See [`EguiMap`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::load::SizedTexture;
use egui::{Event, Image, ImageSource, Sense, TextureId, Ui, Vec2};
use egui_wgpu::wgpu::{FilterMode, TextureView};
use egui_wgpu::RenderState;
use galileo::control::{
    EventProcessor, MapController, MouseButton, RawUserEvent, UserEventHandler,
};
use galileo::galileo_types::cartesian::{Point2, Size};
use galileo::galileo_types::geo::impls::GeoPoint2d;
use galileo::layer::attribution::Attribution;
use galileo::render::WgpuRenderer;
use galileo::{Map, Messenger};

use crate::init::EguiMapOptions;

/// Galileo map widget for EGUI framework.
///
/// # Example
///
/// ```no_run
/// use galileo::layer::raster_tile_layer::RasterTileLayerBuilder;
/// use galileo::MapBuilder;
/// use galileo_egui::{EguiMap, EguiMapState, EguiMapOptions};
/// use galileo::galileo_types::latlon;
/// use galileo::galileo_types::geo::impls::GeoPoint2d;
///
/// struct MapApp {
///     pub map: EguiMapState,
///     pub position: GeoPoint2d,
///     pub resolution: f64,
/// }
///
/// impl eframe::App for MapApp {
///     fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
///         egui::CentralPanel::default().show(ctx, |ui| {
///             EguiMap::new(&mut self.map)
///                 .with_position(&mut self.position)
///                 .with_resolution(&mut self.resolution)
///                 .show_ui(ui);
///         });
///     }
/// }
///
/// let raster_layer = RasterTileLayerBuilder::new_osm()
///     .build()
///     .expect("failed to create layer");
///
/// let map = MapBuilder::default()
///     .with_layer(raster_layer)
///     .build();
///
/// let app_creator = move |cc: &eframe::CreationContext<'_>| {
///     let ctx = cc.egui_ctx.clone();
///     let render_state = cc
///         .wgpu_render_state
///         .clone()
///         .expect("failed to get wgpu context");
///     let egui_map_state = EguiMapState::new(map, ctx, render_state, [], EguiMapOptions::default());
///     let app: Box<dyn eframe::App> = Box::new(MapApp {
///         map: egui_map_state,
///         position: latlon!(55.0, 37.0),
///         resolution: 15000.0,
///     });
///
///     Ok(app)
/// };
///
/// eframe::run_native("Galileo Map in EGUI", eframe::NativeOptions::default(), Box::new(app_creator));
/// ```
pub struct EguiMap<'a> {
    state: &'a mut EguiMapState,
    position: Option<&'a mut GeoPoint2d>,
    resolution: Option<&'a mut f64>,
}

impl<'a> EguiMap<'a> {
    /// Creates a new instance of widget.
    pub fn new(state: &'a mut EguiMapState) -> Self {
        Self {
            state,
            position: None,
            resolution: None,
        }
    }

    /// Sets the position of the center of the map.
    ///
    /// If not specified, the center position will be controlled by the widget itself through user
    /// controls.
    pub fn with_position(&'a mut self, position: &'a mut GeoPoint2d) -> &'a mut Self {
        let curr_view = self.state.map.view();
        if curr_view.position() != Some(*position) {
            self.state.map.set_view(curr_view.with_position(position));
        }

        self.position = Some(position);
        self
    }

    /// Sets the resolution of the map.
    ///
    /// If not set, resolution will be controller by the user input.
    pub fn with_resolution(&'a mut self, resolution: &'a mut f64) -> &'a mut Self {
        let curr_view = self.state.map.view();
        if curr_view.resolution() != *resolution {
            self.state
                .map
                .set_view(curr_view.with_resolution(*resolution));
        }

        self.resolution = Some(resolution);
        self
    }

    /// Renders the map into the ui.
    ///
    /// The map will occupy all available space in the current panel.
    pub fn show_ui(&mut self, ui: &mut Ui) {
        self.state.render(ui);

        let updated_view = self.state.map.view();
        if let Some(resolution) = &mut self.resolution {
            **resolution = updated_view.resolution();
        }

        if let Some(position) = &mut self.position {
            if let Some(view_position) = updated_view.position() {
                **position = view_position;
            }
        }
    }
}

/// State of the map widget.
pub struct EguiMapState {
    map: Map,
    egui_render_state: RenderState,
    renderer: WgpuRenderer,
    requires_redraw: Arc<AtomicBool>,
    texture_id: TextureId,
    texture_view: TextureView,
    event_processor: EventProcessor,
    messenger: MapStateMessenger,
    map_ready: bool,
}

impl<'a> EguiMapState {
    /// Creates a new instance of the state.
    ///
    /// Only one instance of the state should be created for the same map, as it controls internal
    /// state of the map. Keep it inside your application state.
    ///
    /// You can add interactivity to the map by specifying event `handlers` the map will react to.
    pub fn new(
        mut map: Map,
        ctx: egui::Context,
        render_state: RenderState,
        handlers: impl IntoIterator<Item = Box<dyn UserEventHandler>>,
        options: EguiMapOptions,
    ) -> Self {
        let requires_redraw = Arc::new(AtomicBool::new(true));
        let messenger = MapStateMessenger {
            context: ctx.clone(),
            requires_redraw: requires_redraw.clone(),
        };

        map.set_messenger(Some(messenger.clone()));
        for layer in map.layers_mut().iter_mut() {
            layer.set_messenger(Box::new(messenger.clone()));
        }

        // Set a default size so that render target can be created.
        // This size will be replaced by the UI on the first frame.
        let size = Size::new(1, 1);
        map.set_size(size.cast());
        map.set_view(map.view().with_dpi_scale_factor(ctx.pixels_per_point()));

        let mut renderer = WgpuRenderer::new_with_device_and_texture(
            render_state.device.clone(),
            render_state.queue.clone(),
            size,
        );
        renderer.set_horizon_options(options.horizon_options);

        let texture = renderer
            .get_target_texture_view()
            .expect("failed to get map texture");
        let texture_id = render_state.renderer.write().register_native_texture(
            &render_state.device,
            &texture,
            FilterMode::Nearest,
        );

        let mut event_processor = EventProcessor::default();
        for handler in handlers {
            event_processor.add_handler_boxed(handler);
        }
        event_processor.add_handler(MapController::default());

        Self {
            map,
            egui_render_state: render_state,
            renderer,
            requires_redraw,
            texture_id,
            texture_view: texture,
            event_processor,
            messenger,
            map_ready: false,
        }
    }

    /// Lets the map know that it should be rendered on the next render cycle.
    pub fn request_redraw(&self) {
        self.map.redraw();
    }

    /// Renders the map into UI.
    pub fn render(&mut self, ui: &mut egui::Ui) {
        let logical_size = ui.available_size().floor();
        let pixels_per_point = ui.ctx().pixels_per_point();
        let physical_size = logical_size * pixels_per_point;

        let (rect, response) = ui.allocate_exact_size(logical_size, Sense::click_and_drag());

        let renderer_size = self.renderer.size().cast::<f32>();

        let attributions = self.collect_attributions();
        if attributions.is_some() {
            egui::Window::new("Attributions")
                .collapsible(false)
                .title_bar(false)
                .anchor(egui::Align2::RIGHT_BOTTOM, [-10., -10.])
                .auto_sized() // Position bottom-right
                .show(ui.ctx(), |ui| {
                    self.show_attributions(ui); // Render the attributions inside this window
                });
        }

        if self.event_processor.is_dragging() || response.hovered() {
            let events = ui.input(|input_state| input_state.events.clone());
            self.process_events(&events, [-rect.left(), -rect.top()]);
        }

        self.map.animate();

        if physical_size[0] != renderer_size.width() || physical_size[1] != renderer_size.height() {
            self.map_ready = true;
            self.resize_map(logical_size, pixels_per_point);
            self.map
                .set_view(self.map.view().with_dpi_scale_factor(pixels_per_point));
        }

        if self.map_ready {
            self.map.load_layers();
        }

        if self.requires_redraw.swap(false, Ordering::Relaxed) {
            self.draw();
        }

        Image::new(ImageSource::Texture(SizedTexture::new(
            self.texture_id,
            Vec2::new(renderer_size.width(), renderer_size.height()),
        )))
        .paint_at(ui, rect);
    }

    fn collect_attributions(&mut self) -> Option<Vec<Attribution>> {
        let all_layer: Vec<Attribution> = self
            .map
            .layers()
            .iter()
            .filter_map(|layer| layer.attribution())
            .collect();
        if all_layer.is_empty() {
            None
        } else {
            Some(all_layer)
        }
    }
    fn add_attribution_entry(&mut self, ui: &mut egui::Ui, attribution: &Attribution) {
        if let Some(url) = attribution.get_url() {
            ui.hyperlink_to(attribution.get_text(), url);
        } else {
            ui.label(attribution.get_text());
        }
    }

    fn show_attributions(&mut self, ui: &mut egui::Ui) {
        let attributions = self
            .collect_attributions()
            .expect("Failed to collect attributions");

        let mut is_first = true;
        for attribution in &attributions {
            self.add_attribution_entry(ui, attribution);
            if !is_first {
                ui.label(" | ");
            }
            is_first = false;
        }
    }

    /// Returns a reference to the Galileo map instance.
    pub fn map(&'a self) -> &'a Map {
        &self.map
    }

    /// Returns a mutable reference to the Galileo map instance.
    pub fn map_mut(&'a mut self) -> &'a mut Map {
        &mut self.map
    }

    /// Returns event messenger that is used by the map.
    pub fn messenger(&self) -> impl Messenger {
        self.messenger.clone()
    }

    fn resize_map(&mut self, logical_size: Vec2, pixels_per_point: f32) {
        log::trace!(
            "Resizing map to logical size: {logical_size:?}, pixels_per_point: {pixels_per_point}"
        );

        // Set the logical size for the map
        let logical_size_f64 = Size::new(logical_size.x as f64, logical_size.y as f64);
        self.map.set_size(logical_size_f64);

        // Resize the renderer to physical size (accounting for pixel density)
        let physical_size = Size::new(
            (logical_size.x * pixels_per_point) as u32,
            (logical_size.y * pixels_per_point) as u32,
        );
        self.renderer.resize(physical_size);

        // After renderer is resized, a new texture is created, so we need to update its id that we
        // use in UI.
        let texture = self
            .renderer
            .get_target_texture_view()
            .expect("failed to get map texture");

        // Use Linear filtering for better quality on HiDPI displays
        let filter_mode = if pixels_per_point > 1.0 {
            FilterMode::Linear
        } else {
            FilterMode::Nearest
        };
        log::info!("Using filter mode: {filter_mode:?}");

        let texture_id = self
            .egui_render_state
            .renderer
            .write()
            .register_native_texture(&self.egui_render_state.device, &texture, filter_mode);

        self.texture_id = texture_id;
        self.texture_view = texture;

        self.map.redraw();
    }

    fn draw(&mut self) {
        log::trace!("Redrawing the map");
        self.map.load_layers();
        self.renderer
            .render_to_texture_view(&self.map, &self.texture_view);
    }

    fn process_events(&mut self, events: &[Event], offset: [f32; 2]) {
        for event in events {
            if let Some(raw_event) = Self::convert_event(event, offset) {
                self.event_processor.handle(raw_event, &mut self.map);
            }
        }
    }

    fn convert_event(event: &Event, offset: [f32; 2]) -> Option<RawUserEvent> {
        match event {
            Event::PointerButton {
                button, pressed, ..
            } => {
                let button = match button {
                    egui::PointerButton::Primary => MouseButton::Left,
                    egui::PointerButton::Secondary => MouseButton::Right,
                    egui::PointerButton::Middle => MouseButton::Middle,
                    _ => MouseButton::Other,
                };

                Some(match pressed {
                    true => RawUserEvent::ButtonPressed(button),
                    false => RawUserEvent::ButtonReleased(button),
                })
            }
            Event::PointerMoved(position) => {
                let scale = 1.0;
                let pointer_position = Point2::new(
                    (position.x + offset[0]) as f64 / scale,
                    (position.y + offset[1]) as f64 / scale,
                );
                Some(RawUserEvent::PointerMoved(pointer_position))
            }
            #[cfg(not(target_arch = "wasm32"))]
            Event::MouseWheel { delta, .. } => {
                let zoom = delta[1] as f64;

                if zoom.abs() < 0.0001 {
                    return None;
                }

                Some(RawUserEvent::Scroll(zoom))
            }
            #[cfg(target_arch = "wasm32")]
            Event::MouseWheel { delta, unit, .. } => {
                // Winit produces different values in different browsers and they are all different
                // from native platforms. See ttps://github.com/rust-windowing/winit/issues/22
                //
                // This hack is based on manual tests and might break in future. But this is the
                // best I could come up with to mitigate the issue.
                let zoom = match unit {
                    egui::MouseWheelUnit::Point => delta[1] as f64 / 120.0,
                    egui::MouseWheelUnit::Line => delta[1] as f64 / 6.0,
                    egui::MouseWheelUnit::Page => delta[1] as f64,
                };

                if zoom.abs() < 0.0001 {
                    return None;
                }

                Some(RawUserEvent::Scroll(zoom))
            }

            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MapStateMessenger {
    pub requires_redraw: Arc<AtomicBool>,
    pub context: egui::Context,
}

impl Messenger for MapStateMessenger {
    fn request_redraw(&self) {
        log::trace!("Redraw requested");
        if !self.requires_redraw.swap(true, Ordering::Relaxed) {
            self.context.request_repaint();
        }
    }
}
