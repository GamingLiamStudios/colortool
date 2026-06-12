use std::{
    fs::File,
    io::{
        Read,
        Write,
    },
    num::NonZero,
    os::fd::{
        AsFd,
        OwnedFd,
    },
    process::{
        ChildStdout,
        Stdio,
    },
    time::Duration,
};

use calloop::{
    Interest,
    generic::Generic,
    timer::{
        TimeoutAction,
        Timer,
    },
};
use half::f16;
use rgb::{
    ColorComponentMap,
    ComponentMap,
    Rgb,
    Rgba,
};
use smithay_client_toolkit::{
    compositor::{
        CompositorHandler,
        CompositorState,
    },
    delegate_compositor,
    delegate_dmabuf,
    delegate_output,
    delegate_registry,
    delegate_xdg_shell,
    delegate_xdg_window,
    dmabuf::{
        DmabufHandler,
        DmabufState,
    },
    output::{
        OutputHandler,
        OutputState,
    },
    reexports::{
        calloop::{
            EventLoop,
            InsertError,
            LoopHandle,
        },
        calloop_wayland_source::WaylandSource,
        client::{
            ConnectError,
            Connection,
            globals::{
                GlobalError,
                registry_queue_init,
            },
        },
    },
    registry::{
        ProvidesRegistryState,
        RegistryState,
    },
    registry_handlers,
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{
                Window,
                WindowConfigure,
                WindowHandler,
            },
        },
    },
};
use tracing::Level;
use tracing_subscriber::{
    Layer,
    filter::Targets,
    layer::SubscriberExt,
};
use wayland_client::{
    QueueHandle,
    protocol::{
        wl_buffer::WlBuffer,
        wl_output::{
            Transform,
            WlOutput,
        },
        wl_surface::WlSurface,
    },
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self,
    ZwpLinuxBufferParamsV1,
};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("Failed to initalize Wayland connection")]
    WaylandConnection(#[from] ConnectError),

    #[error("Global Error")]
    WaylandGlobal(#[from] GlobalError),

    #[error("Wayland Eventloop Error")]
    WaylandInsert(#[from] InsertError<WaylandSource<App>>),

    #[error("Spotread init Error")]
    SpotreadInsert(#[from] InsertError<Generic<ChildStdout>>),

    #[error("I/O Error")]
    GenericIO(#[from] std::io::Error),
}

struct Framebuffer {
    gbm:      gbm::BufferObject<()>,
    fd:       OwnedFd,
    size:     (u32, u32),
    params:   ZwpLinuxBufferParamsV1,
    wayland:  Option<WlBuffer>,
    released: bool,
}

struct App {
    registry_state: RegistryState,
    output_state:   OutputState,
    dmabuf_state:   DmabufState,

    gbm_device:   gbm::Device<File>,
    framebuffers: [Framebuffer; 2],
    surface:      WlSurface,
    window:       Window,

    pub is_running: bool,
    swatch_color:   rgb::Rgba<f16>,
    swatch_updated: bool,

    spotread:            std::process::Child,
    spotread_in:         std::process::ChildStdin,
    pub detected_swatch: Rgb<f64>, // in unnormalized XYZ

    loop_handle: LoopHandle<'static, Self>,
}

impl Drop for App {
    fn drop(&mut self) {
        _ = self.spotread_in.write_all("qq".as_bytes());
        _ = self.spotread.wait();
    }
}

impl App {
    pub fn new(loop_handle: LoopHandle<'static, App>) -> Result<Self, Error> {
        let conn = Connection::connect_to_env()?;

        let (globals, event_queue) = registry_queue_init(&conn)?;
        let qh = event_queue.handle();
        WaylandSource::new(conn.clone(), event_queue).insert(loop_handle.clone())?;

        let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
        let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg_shell missing");
        let dmabuf_state = DmabufState::new(&globals, &qh);

        let surface = compositor.create_surface(&qh);
        let window = xdg_shell.create_window(
            surface.clone(),
            smithay_client_toolkit::shell::xdg::window::WindowDecorations::None,
            &qh,
        );

        window.set_title("colortool");
        window.set_app_id("org.glstudios.colortool");
        window.set_min_size(Some((256, 256)));
        window.commit();

        surface.frame(&qh, surface.clone());
        surface.commit();

        let card = File::options()
            .write(true)
            .read(true)
            .open("/dev/dri/renderD128")?;
        let gbm_device = gbm::Device::new(card)?;

        let mut spotread = std::process::Command::new("spotread")
            .arg("-e")
            .env("ARGYLL_NOT_INTERACTIVE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        let spotread_out = spotread
            .stdout
            .take()
            .expect("Failed to get stdout for spotread");
        let spotread_in = spotread
            .stdin
            .take()
            .expect("Failed to get stdin for spotread");

        loop_handle.insert_source(
            Generic::new(spotread_out, Interest::READ, calloop::Mode::Edge),
            |readiness, stdout, app| {
                if !readiness.readable {
                    tracing::warn!(?readiness, "Spotread callback triggered with no new output");
                    return Ok(calloop::PostAction::Continue);
                }

                let mut buf = vec![0; 4096];
                let bytes_read = unsafe { stdout.get_mut().read(buf.as_mut_slice()) }?;
                buf.truncate(bytes_read);

                let string = String::from_utf8(buf).expect("Non-UTF8 output from spotread");

                // Process string
                let mut results = string
                    .lines()
                    .map(|s| s.trim())
                    .filter(|s| s.starts_with("Result is XYZ: "));
                if let Some(result) = results.next() {
                    let values: Vec<_> = result
                        .split_whitespace()
                        .skip(3)
                        .take(3)
                        .map(|s| {
                            s.trim_end_matches(',')
                                .parse()
                                .expect("Returned XYZ not valid")
                        })
                        .collect();
                    app.detected_swatch = Rgb::new(values[0], values[1], values[2]);
                    tracing::debug!(detected = ?app.detected_swatch);
                }

                Ok(calloop::PostAction::Continue)
            },
        )?;

        Ok(Self {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),

            framebuffers: std::array::from_fn(|i| {
                let buffer = gbm_device
                    .create_buffer_object(
                        256,
                        256,
                        gbm::Format::Abgr16161616f,
                        gbm::BufferObjectFlags::LINEAR | gbm::BufferObjectFlags::RENDERING,
                    )
                    .expect("failed to create framebuffer");

                let stride = size_of::<Rgba<f16>>() as u32 * 256;
                let fd = buffer.fd().expect("failed to get framebuffer fd");

                let params = dmabuf_state
                    .create_params(&qh)
                    .expect("Failed to create dmabuf");
                params.add(fd.as_fd(), 0, 0, stride, 0);

                Framebuffer {
                    fd,
                    gbm: buffer,
                    size: (256, 256),
                    wayland: None,
                    params: params.create(
                        256 as i32,
                        256 as i32,
                        0x48344241, // ABGR f16
                        zwp_linux_buffer_params_v1::Flags::empty(),
                    ),
                    released: true,
                }
            }),
            dmabuf_state,
            swatch_color: Rgba::new(
                f16::from_f32(0.0),
                f16::from_f32(0.0),
                f16::from_f32(0.0),
                f16::from_f32(1.0),
            ),
            swatch_updated: true,
            detected_swatch: Rgb::new(-1.0, -1.0, -1.0),

            surface,
            window,
            gbm_device,

            spotread,
            spotread_in,

            loop_handle,
            is_running: false,
        })
    }

    pub fn set_swatch(
        &mut self,
        swatch: Rgb<f32>,
    ) {
        self.swatch_color = swatch.with_alpha(1.0).map(f16::from_f32);
        self.swatch_updated = false;
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_transform: Transform,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &WlSurface,
        _time: u32,
    ) {
        let framebuffer = &mut self.framebuffers[0];
        if !framebuffer.released {
            tracing::warn!("Framebuffer not yet released!");
            return;
        }
        framebuffer.released = false;

        //tracing::debug!("Rendering frame!");
        let (width, height) = framebuffer.size;
        framebuffer
            .gbm
            .map_mut(0, 0, width, height, |mapped| {
                bytemuck::cast_slice_mut(mapped.buffer_mut()).fill(self.swatch_color);
            })
            .expect("Failed to write to framebuffer");

        let Some(buffer) = framebuffer.wayland.as_ref() else {
            tracing::warn!("Framebuffer not yet created!");
            return;
        };
        surface.attach(Some(buffer), 0, 0);
        surface.damage(0, 0, width as i32, height as i32);
        surface.frame(qh, surface.clone());
        surface.commit();

        self.framebuffers.swap(0, 1);

        if !self.swatch_updated {
            // Trigger new spotread
            self.loop_handle
                .insert_source(
                    Timer::from_duration(Duration::from_millis(200)),
                    |_, _, app| {
                        app.spotread_in
                            .write_all("\n".as_bytes())
                            .expect("Failed to write command to spotread");
                        TimeoutAction::Drop
                    },
                )
                .expect("Failed to attach new write command to event loop");
        }

        self.swatch_updated = true;
    }
}
impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }
}
impl WindowHandler for App {
    fn request_close(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _window: &Window,
    ) {
        self.is_running = false;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        tracing::debug!(?configure);
        let (new_width, new_height) = configure.new_size;
        let (old_width, old_height) = self.framebuffers[0].size;
        let width = new_width.map(NonZero::get).unwrap_or(old_width);
        let height = new_height.map(NonZero::get).unwrap_or(old_height);

        for framebuffer in &mut self.framebuffers {
            framebuffer.released = false;
            framebuffer.size = (width, height);
            framebuffer.wayland = None;

            let buffer = self
                .gbm_device
                .create_buffer_object(
                    width,
                    height,
                    gbm::Format::Abgr16161616f,
                    gbm::BufferObjectFlags::LINEAR | gbm::BufferObjectFlags::SCANOUT,
                )
                .expect("failed to create framebuffer");

            let stride = size_of::<Rgba<f16>>() as u32 * width;
            let fd = buffer.fd().expect("failed to get framebuffer fd");

            let params = self
                .dmabuf_state
                .create_params(&qh)
                .expect("Failed to create dmabuf");
            params.add(fd.as_fd(), 0, 0, stride, 0);
            framebuffer.params = params.create(
                width as i32,
                height as i32,
                0x48344241, // ABGR f16
                zwp_linux_buffer_params_v1::Flags::empty(),
            );
            framebuffer.fd = fd;
            framebuffer.gbm = buffer;
        }
    }
}

impl DmabufHandler for App {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn created(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        params: &ZwpLinuxBufferParamsV1,
        buffer: WlBuffer,
    ) {
        tracing::debug!("New buffer created");
        let Some(framebuffer) = self
            .framebuffers
            .iter_mut()
            .find(|framebuffer| framebuffer.params == *params)
        else {
            return;
        };
        framebuffer.wayland = Some(buffer);
        framebuffer.released = true;

        if self.framebuffers[0].params == *params {
            self.surface
                .attach(self.framebuffers[0].wayland.as_ref(), 0, 0);
            self.surface.commit();
        }
    }

    fn failed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _params: &ZwpLinuxBufferParamsV1,
    ) {
        tracing::debug!("Failed buffer created");
    }

    fn released(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        buffer: &WlBuffer,
    ) {
        let Some(framebuffer) = self
            .framebuffers
            .iter_mut()
            .find(|framebuffer| framebuffer.wayland.as_ref() == Some(buffer))
        else {
            return;
        };

        framebuffer.released = true;
    }

    fn dmabuf_feedback(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _proxy: &wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        _feedback: smithay_client_toolkit::dmabuf::DmabufFeedback,
    ) {
    }
}

impl ProvidesRegistryState for App {
    registry_handlers![OutputState,];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_dmabuf!(App);
delegate_registry!(App);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stdout = tracing_subscriber::fmt::layer();
    let registry = tracing_subscriber::registry().with(
        stdout.with_filter(
            Targets::default()
                .with_target("colortool", Level::TRACE)
                .with_default(Level::INFO),
        ),
    );
    tracing::subscriber::set_global_default(registry)?;

    tracing::info!("Hello World!");

    let mut event_loop = EventLoop::<App>::try_new().expect("Failed to initalize event loop");
    let mut app = App::new(event_loop.handle())?;

    let handle = event_loop.handle();
    handle.insert_source(
        Timer::from_duration(Duration::from_millis(10000)),
        |_event, _metadata, app| {
            tracing::debug!("Trigger!");
            app.set_swatch(Rgb::new(1.0, 0.0, 0.0));

            TimeoutAction::Drop
        },
    )?;

    app.is_running = true;
    loop {
        event_loop.dispatch(None, &mut app)?;

        if !app.is_running {
            break;
        }
    }

    Ok(())
}
