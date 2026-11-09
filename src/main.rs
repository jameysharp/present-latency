use std::ptr::NonNull;

use bytemuck::{Pod, Zeroable};
use pollster::FutureExt;
use wayland_client::protocol::{
    wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wgpu::util::DeviceExt;
use wgpu_text::glyph_brush::ab_glyph::FontRef;
use wgpu_text::glyph_brush::{Section, Text};
use wgpu_text::{BrushBuilder, TextBrush};

struct Events;
struct IgnoreEvents;

#[derive(Debug, Default)]
struct Globals(Vec<(u32, String, u32)>);

struct State {
    running: bool,
    presentation_time: wp_presentation::WpPresentation,
    base_surface: wl_surface::WlSurface,
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    brush: TextBrush<FontRef<'static>>,
    stipple: Stipple,
    stipple_buffer: wgpu::Buffer,
    bounds: Size,
    requested_size: Size,
    configured_size: Size,
    last_motion: f64,
    last_animated: f64,
    animating: bool,
    velocity: [i32; 2],
    refresh_ns: u32,
    present_flags: WEnum<wp_presentation_feedback::Kind>,
    stats: Stats,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Size {
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
struct Stipple {
    offset: [i32; 2],
}

impl Stipple {
    const VERTEX_COUNT: u32 = 4;

    const BUFFER_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<Stipple>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &wgpu::vertex_attr_array![
            0 => Sint32x2,
        ],
    };

    const PRIMITIVE_STATE: wgpu::PrimitiveState = wgpu::PrimitiveState {
        topology: wgpu::PrimitiveTopology::TriangleStrip,
        strip_index_format: None,
        front_face: wgpu::FrontFace::Cw,
        cull_mode: None,
        unclipped_depth: false,
        polygon_mode: wgpu::PolygonMode::Fill,
        conservative: false,
    };
}

fn main() {
    // * first, Wayland setup

    let connection = Connection::connect_to_env().unwrap();
    let display = connection.display();

    let mut globals = Globals::default();
    let mut global_events = connection.new_event_queue();
    let global_qh = global_events.handle();
    let registry = display.get_registry(&global_qh, Events);
    global_events.roundtrip(&mut globals).unwrap();

    let mut event_queue = connection.new_event_queue();
    let qh = event_queue.handle();

    let compositor: wl_compositor::WlCompositor = globals
        .bind_singleton(&registry, &qh, IgnoreEvents)
        .unwrap();
    let wm_base: xdg_wm_base::XdgWmBase = globals.bind_singleton(&registry, &qh, Events).unwrap();
    let presentation_time: wp_presentation::WpPresentation =
        globals.bind_singleton(&registry, &qh, Events).unwrap();

    // Cheat: pretend there's only one seat ever; and bind it just to get events
    let _seat: wl_seat::WlSeat = globals.bind_singleton(&registry, &qh, Events).unwrap();

    let base_surface = compositor.create_surface(&qh, IgnoreEvents);
    let xdg_surface = wm_base.get_xdg_surface(&base_surface, &qh, Events);
    let xdg_toplevel = xdg_surface.get_toplevel(&qh, Events);
    xdg_toplevel.set_title("presentation timing test".to_owned());
    xdg_toplevel.set_fullscreen(None);
    base_surface.frame(&qh, Events);
    base_surface.commit();

    // * done with Wayland setup, start WGPU setup

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
        Box::new(connection.backend()),
    ));

    // a 'static lifetime is a white lie (the window will stay alive until the
    // end of main, which is almost the same thing) and I'm confused about why
    // I'm allowed to claim that it's true, but wayland-client expects state
    // structures to be 'static for reasons I don't understand, so run with it
    let surface: wgpu::Surface<'static> = instance
        .create_surface(wgpu::SurfaceTarget::from_window_without_display(
            WaylandWindow(base_surface.clone()),
        ))
        .unwrap();

    let power_preference = wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::None);
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface: Some(&surface),
            ..Default::default()
        })
        .block_on()
        .unwrap();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .block_on()
        .unwrap();

    let cap = surface.get_capabilities(&adapter);
    let surface_format = cap.formats[0];

    // * done with general WGPU setup, now create app-specific resources

    // we don't know the surface size until the first time it's configured so
    // use a placeholder size here initially
    let brush = BrushBuilder::using_font_bytes(include_bytes!("VeraMoBd.ttf"))
        .unwrap()
        .build(&device, 500, 500, surface_format);

    let stipple = Stipple { offset: [0, 0] };
    let stipple_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("stipple"),
        contents: bytemuck::cast_slice(&[stipple]),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
    });

    let shader_module = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("stipple"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("stipple"),
        layout: Some(&pipeline_layout),
        primitive: Stipple::PRIMITIVE_STATE,
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: None,
            compilation_options: Default::default(),
            buffers: &[Some(Stipple::BUFFER_LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: None,
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });

    // * all setup complete, now just crank the event loop

    let mut state = State {
        running: true,
        presentation_time,
        base_surface,
        instance,
        device,
        queue,
        surface,
        surface_format,
        pipeline,
        brush,
        stipple,
        stipple_buffer,
        bounds: Size::default(),
        requested_size: Size::default(),
        configured_size: Size {
            width: -1,
            height: -1,
        },
        last_motion: 0.0,
        last_animated: 0.0,
        animating: true,
        velocity: [1, 0],
        refresh_ns: 0,
        present_flags: WEnum::Value(wp_presentation_feedback::Kind::empty()),
        stats: Stats::default(),
    };
    while state.running {
        event_queue.blocking_dispatch(&mut state).unwrap();
    }
}

impl Size {
    fn or(self, other: Self) -> Self {
        Size {
            width: if self.width != 0 {
                self.width
            } else {
                other.width
            },
            height: if self.height != 0 {
                self.height
            } else {
                other.height
            },
        }
    }
}

impl State {
    fn get_current_texture(&mut self) -> Option<wgpu::SurfaceTexture> {
        loop {
            let current_size = self.requested_size.or(self.bounds).or(Size {
                width: 500,
                height: 500,
            });
            if self.configured_size == current_size {
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(texture) => return Some(texture),
                    wgpu::CurrentSurfaceTexture::Occluded => return None,
                    wgpu::CurrentSurfaceTexture::Timeout => return None,
                    wgpu::CurrentSurfaceTexture::Suboptimal(_texture) => {}
                    wgpu::CurrentSurfaceTexture::Outdated => {}
                    wgpu::CurrentSurfaceTexture::Lost => {
                        self.surface = self
                            .instance
                            .create_surface(wgpu::SurfaceTarget::from_window_without_display(
                                WaylandWindow(self.base_surface.clone()),
                            ))
                            .unwrap();
                    }
                    wgpu::CurrentSurfaceTexture::Validation => {
                        unreachable!("By default, validation errors panic");
                    }
                }
            } else {
                self.configured_size = current_size;
            }

            let surface_config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.surface_format,
                color_space: wgpu::SurfaceColorSpace::Auto,
                // Request compatibility with the sRGB-format texture view we‘re going to create later.
                view_formats: vec![self.surface_format.add_srgb_suffix()],
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                width: current_size.width.cast_unsigned(),
                height: current_size.height.cast_unsigned(),
                desired_maximum_frame_latency: 1,
                present_mode: wgpu::PresentMode::Fifo,
            };

            self.surface.configure(&self.device, &surface_config);
            self.brush.resize_view(
                current_size.width as f32,
                current_size.height as f32,
                &self.queue,
            );
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let Some(surface_texture) = self.get_current_texture() else {
            return;
        };

        let text = format!(
            "mean={:.0}ms ({:.2}vblank) stddev={:.0}ms N={} min={:.0}ms max={:.0}ms {:?}",
            self.stats.mean,
            self.stats.mean / (self.refresh_ns as f64 / 1e6),
            self.stats.stddev(),
            self.stats.count,
            self.stats.min,
            self.stats.max,
            self.present_flags,
        );
        let text = Section::new()
            .add_text(
                Text::new(&text)
                    .with_color([0.0, 0.8, 0.4, 1.0])
                    .with_scale(32.0),
            )
            .with_screen_position((10.0, 10.0));
        self.brush
            .queue(&self.device, &self.queue, [&text])
            .unwrap();

        let texture_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.surface_format.add_srgb_suffix()),
                ..Default::default()
            });

        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut renderpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &texture_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        renderpass.set_pipeline(&self.pipeline);
        renderpass.set_vertex_buffer(0, self.stipple_buffer.slice(..));
        renderpass.draw(0..Stipple::VERTEX_COUNT, 0..1);

        self.brush.draw(&mut renderpass);

        drop(renderpass);

        self.queue.submit([encoder.finish()]);
        self.base_surface.frame(qh, Events);
        self.queue.present(surface_texture);
    }
}

#[derive(Clone)]
struct WaylandWindow(wl_surface::WlSurface);

impl wgpu::rwh::HasWindowHandle for WaylandWindow {
    fn window_handle(&self) -> Result<wgpu::rwh::WindowHandle<'_>, wgpu::rwh::HandleError> {
        let raw = wgpu::rwh::RawWindowHandle::from(wgpu::rwh::WaylandWindowHandle::new(
            NonNull::new(self.0.id().as_ptr()).unwrap().cast(),
        ));
        Ok(unsafe { wgpu::rwh::WindowHandle::borrow_raw(raw) })
    }
}

#[derive(Default)]
struct Stats {
    min: f64,
    max: f64,
    mean: f64,
    m2: f64,
    count: u64,
}

impl Stats {
    fn add(&mut self, v: f64) {
        self.count += 1;
        if self.count == 1 {
            self.min = v;
            self.max = v;
            self.mean = v;
        } else {
            if v < self.min {
                self.min = v;
            }
            if v > self.max {
                self.max = v;
            }
            let delta = v - self.mean;
            self.mean += delta / self.count as f64;
            self.m2 += delta * (v - self.mean);
        }
    }

    fn stddev(&self) -> f64 {
        (self.m2 / self.count as f64).sqrt()
    }
}

impl<I: Proxy> Dispatch<I, IgnoreEvents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &I,
        _event: I::Event,
        _data: &IgnoreEvents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_registry::WlRegistry, Events> for Globals {
    fn event(
        state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &Events,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                state.0.push((name, interface, version));
            }
            wl_registry::Event::GlobalRemove { name } => {
                let i = state.0.iter().rposition(|g| g.0 == name).unwrap();
                state.0.swap_remove(i);
            }
            _ => {}
        }
    }
}

impl Globals {
    fn bind_singleton<I: Proxy + 'static, U: Send + Sync + 'static, S: Dispatch<I, U> + 'static>(
        &self,
        registry: &wl_registry::WlRegistry,
        qh: &QueueHandle<S>,
        udata: U,
    ) -> Option<I> {
        let desc = I::interface();
        let g = self.0.iter().find(|g| g.1 == desc.name)?;
        Some(registry.bind(g.0, desc.version.min(g.2), qh, udata))
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, Events> for State {
    fn event(
        _state: &mut Self,
        proxy: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _data: &Events,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            proxy.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, Events> for State {
    fn event(
        state: &mut Self,
        proxy: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _data: &Events,
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            proxy.ack_configure(serial);
            state.draw(qhandle);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, Events> for State {
    fn event(
        state: &mut Self,
        _proxy: &xdg_toplevel::XdgToplevel,
        event: <xdg_toplevel::XdgToplevel as Proxy>::Event,
        _data: &Events,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Close => {
                eprintln!("quitting due to window close request");
                state.running = false;
            }
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state.requested_size = Size { width, height };
            }
            xdg_toplevel::Event::ConfigureBounds { width, height } => {
                state.bounds = Size { width, height };
            }
            _ => (),
        }
    }
}

impl Dispatch<wl_callback::WlCallback, Events> for State {
    fn event(
        state: &mut Self,
        _proxy: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _data: &Events,
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { callback_data } = event {
            // "current time, in milliseconds, with an undefined base", but I
            // suspect most implementations use CLOCK_MONOTONIC
            let time = callback_data as f64 / 1e3;
            let should_animate = time - state.last_motion > 0.25;
            if !state.animating && should_animate {
                let latency_ms = (state.last_motion - state.last_animated) * 1e3;
                println!("{latency_ms:.3}");
                state.stats.add(latency_ms);
                state.velocity = [-state.velocity[1], state.velocity[0]];
            }
            state.animating = should_animate;
            if should_animate {
                state.stipple.offset[0] += state.velocity[0];
                state.stipple.offset[1] += state.velocity[1];
                state.queue.write_buffer(
                    &state.stipple_buffer,
                    0,
                    bytemuck::cast_slice(&[state.stipple]),
                );
                state
                    .presentation_time
                    .feedback(&state.base_surface, qhandle, Events);
            }
            state.draw(qhandle);
        }
    }
}

impl Dispatch<wp_presentation::WpPresentation, Events> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wp_presentation::WpPresentation,
        event: <wp_presentation::WpPresentation as Proxy>::Event,
        _data: &Events,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            match rustix::time::ClockId::try_from(clk_id as i32) {
                Ok(clock) => eprintln!("presentation clock is {clock:?}"),
                _ => eprintln!("unknown presentation clock {clk_id}"),
            }
        }
    }
}

impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, Events> for State {
    fn event(
        state: &mut Self,
        _proxy: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _data: &Events,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let wp_presentation_feedback::Event::Presented {
            tv_sec_hi,
            tv_sec_lo,
            tv_nsec,
            refresh,
            flags,
            ..
        } = event
        {
            let tv_sec = ((tv_sec_hi as u64) << 32) | (tv_sec_lo as u64);
            let time = tv_sec as f64 + (tv_nsec as f64 / 1e9);
            state.last_animated = time;
            state.refresh_ns = refresh;
            state.present_flags = flags;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, Events> for State {
    fn event(
        _state: &mut Self,
        proxy: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &Events,
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if let WEnum::Value(capabilities) = capabilities {
                if capabilities.contains(wl_seat::Capability::Pointer) {
                    proxy.get_pointer(qhandle, Events);
                }
                if capabilities.contains(wl_seat::Capability::Keyboard) {
                    proxy.get_keyboard(qhandle, Events);
                }
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, Events> for State {
    fn event(
        state: &mut Self,
        _proxy: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &Events,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Motion { time, .. } = event {
            // Wayland documentation says this timestamp has "millisecond
            // granularity" but an "undefined base". I think that in practice,
            // compositors using libinput pass along whatever timestamp that
            // library provides, which is documented to be CLOCK_MONOTONIC.
            let time = time as f64 / 1e3;
            state.last_motion = time;
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, Events> for State {
    fn event(
        state: &mut Self,
        _proxy: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &Events,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            eprintln!("quitting due to key press");
            state.running = false;
        }
    }
}
