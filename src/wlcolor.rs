use std::{
    collections::HashMap,
    ops::RangeInclusive,
    sync::{
        Arc,
        Mutex,
    },
};

use smithay_client_toolkit::{
    error::GlobalError,
    globals::GlobalData,
};
use wayland_client::{
    Connection,
    Dispatch,
    QueueHandle,
    WEnum,
    globals::{
        BindError,
        GlobalList,
    },
    protocol::{
        wl_output::WlOutput,
        wl_surface::WlSurface,
    },
};
pub use wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::{
    Feature,
    Primaries as NamedPrimaries,
    RenderIntent,
    TransferFunction as NamedEotf,
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1,
    wp_color_management_surface_feedback_v1,
    wp_color_manager_v1,
    wp_image_description_info_v1,
    wp_image_description_v1,
};

#[derive(Debug)]
pub struct ColorState {
    color_manager: wp_color_manager_v1::WpColorManagerV1,
    outputs:       HashMap<WlOutput, ColorOutput>,

    supported_idents:    Vec<WEnum<RenderIntent>>,
    supported_features:  Vec<WEnum<Feature>>,
    supported_tfs:       Vec<WEnum<NamedEotf>>,
    supported_primaries: Vec<WEnum<NamedPrimaries>>,
}

#[derive(Debug)]
struct ColorOutput {
    manager:      wp_color_management_output_v1::WpColorManagementOutputV1,
    info:         wp_image_description_v1::WpImageDescriptionV1,
    pending_info: Option<wp_image_description_v1::WpImageDescriptionV1>,
}

#[derive(Debug, Clone)]
pub enum Primaries {
    Named(WEnum<NamedPrimaries>),
    Custom {
        red:   (f64, f64),
        green: (f64, f64),
        blue:  (f64, f64),
        white: (f64, f64),
    },
}

#[derive(Debug, Clone)]
pub enum Eotf {
    Named(WEnum<NamedEotf>),
    Power(f64),
}

#[derive(Debug, Clone)]
pub enum ImageDescriptionInfo {
    Parametric {
        primaries:  Primaries,
        eotf:       Eotf,
        luma_range: RangeInclusive<f64>,
        ref_luma:   f64,

        // Either the target display or the content's mastering display, depending on the context.
        mastering_primaries:  Option<Primaries>,
        mastering_luma_range: Option<RangeInclusive<f64>>,

        target_max_cll:  Option<f64>,
        target_max_fall: Option<f64>,
    },
}

// TOOD: Support ICC profiles
#[derive(Debug, Default)]
pub struct ImageDescriptionBuilder {
    primaries:  Option<Primaries>,
    eotf:       Option<Eotf>,
    luma_range: Option<RangeInclusive<f64>>,
    ref_luma:   Option<f64>,

    mastering_primaries:  Option<Primaries>,
    mastering_luma_range: Option<RangeInclusive<f64>>,

    target_max_cll:  Option<f64>,
    target_max_fall: Option<f64>,
}

impl ImageDescriptionBuilder {
    pub fn build(&self) -> ImageDescriptionInfo {
        ImageDescriptionInfo::Parametric {
            primaries:  self.primaries.clone().expect("Missing primaries"),
            eotf:       self.eotf.clone().expect("Missing EOTF"),
            luma_range: self.luma_range.clone().expect("Missing luma range"),
            ref_luma:   self.ref_luma.expect("Missing reference white luminance"),

            mastering_primaries:  self.mastering_primaries.clone(),
            mastering_luma_range: self.mastering_luma_range.clone(),

            target_max_cll:  self.target_max_cll,
            target_max_fall: self.target_max_fall,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageDescriptionData(Arc<Mutex<ImageDescriptionInner>>);

#[derive(Debug, Clone)]
pub enum ImageDescriptionSource {
    Output(WlOutput),
    Surface(WlSurface),
}

#[derive(Debug)]
pub enum ImageDescriptionInner {
    NotReady(ImageDescriptionSource),
    Building {
        builder: Arc<Mutex<ImageDescriptionBuilder>>,
        source:  ImageDescriptionSource,
    },
    Defined(ImageDescriptionInfo),
}

pub trait ColorHandler: Sized {
    fn color_handler(&mut self) -> &mut ColorState;

    /// Supported image subvalues are available in the ``ColorState``
    fn compositor_features(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
    );

    /// Triggered when the description of a monitored output changes.
    fn output_update(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        output: &WlOutput,
        description: ImageDescriptionInfo,
    );

    /// Triggered when the preferred description of a monitored surface changes.
    fn surface_update(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        proxy: &wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        surface: &WlSurface,
        identity: u64,
    );
}

#[derive(Debug)]
pub struct OutputFeedbackData {
    output: WlOutput,
}

#[derive(Debug)]
pub struct SurfaceFeedbackData {
    surface: WlSurface,
}

impl ColorState {
    pub fn bind<D>(
        globals: &GlobalList,
        qh: &QueueHandle<D>,
    ) -> Result<Self, BindError>
    where
        D: Dispatch<wp_color_manager_v1::WpColorManagerV1, GlobalData> + 'static,
    {
        let color_manager = globals.bind(qh, 1..=2, GlobalData)?;

        Ok(Self {
            color_manager,
            outputs: HashMap::new(),

            supported_idents: Vec::new(),
            supported_features: Vec::new(),
            supported_tfs: Vec::new(),
            supported_primaries: Vec::new(),
        })
    }

    pub fn track_output<D>(
        &mut self,
        qh: &QueueHandle<D>,
        output: WlOutput,
    ) where
        D: Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, OutputFeedbackData>
            + Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
            + 'static,
    {
        let manager = self
            .color_manager
            .get_output(&output, qh, OutputFeedbackData {
                output: output.clone(),
            });
        let info = manager.get_image_description(
            qh,
            ImageDescriptionData(Arc::new(Mutex::new(ImageDescriptionInner::NotReady(
                ImageDescriptionSource::Output(output.clone()),
            )))),
        );

        self.outputs.insert(output, ColorOutput {
            manager,
            info,
            pending_info: None,
        });
    }

    pub fn release_output(
        &mut self,
        output: &WlOutput,
    ) {
        self.outputs.remove(output);
    }

    pub fn get_surface_feedback<D>(
        &self,
        surface: &WlSurface,
        qh: &QueueHandle<D>,
    ) -> Result<
        wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        GlobalError,
    >
    where
        D: Dispatch<
                wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
                SurfaceFeedbackData,
            > + 'static,
    {
        Ok(self
            .color_manager
            .get_surface_feedback(surface, qh, SurfaceFeedbackData {
                surface: surface.clone(),
            }))
    }
}

impl<D> Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, OutputFeedbackData, D>
    for ColorState
where
    D: Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, OutputFeedbackData>
        + Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
        + ColorHandler
        + 'static,
{
    fn event(
        state: &mut D,
        _proxy: &wp_color_management_output_v1::WpColorManagementOutputV1,
        event: wp_color_management_output_v1::Event,
        data: &OutputFeedbackData,
        _conn: &Connection,
        qh: &QueueHandle<D>,
    ) {
        match event {
            wp_color_management_output_v1::Event::ImageDescriptionChanged {} => {
                let output = state
                    .color_handler()
                    .outputs
                    .get_mut(&data.output)
                    .expect("Got event on untracked output");

                let new_info = output.manager.get_image_description(
                    qh,
                    ImageDescriptionData(Arc::new(Mutex::new(ImageDescriptionInner::NotReady(
                        ImageDescriptionSource::Output(data.output.clone()),
                    )))),
                );
                output.pending_info = Some(new_info);
            },
            _ => unreachable!(),
        }
    }
}

impl<D>
    Dispatch<
        wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        SurfaceFeedbackData,
        D,
    > for ColorState
where
    D: Dispatch<
            wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
            SurfaceFeedbackData,
        > + ColorHandler,
{
    fn event(
        state: &mut D,
        proxy: &wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        event: wp_color_management_surface_feedback_v1::Event,
        data: &SurfaceFeedbackData,
        conn: &Connection,
        qh: &QueueHandle<D>,
    ) {
        match event {
            wp_color_management_surface_feedback_v1::Event::PreferredChanged2 {
                identity_hi,
                identity_lo,
            } => {
                let identity = (identity_hi as u64) << 32 | (identity_lo as u64);
                state.surface_update(conn, qh, proxy, &data.surface, identity);
            },
            wp_color_management_surface_feedback_v1::Event::PreferredChanged { identity } => {
                state.surface_update(conn, qh, proxy, &data.surface, identity as u64);
            },
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData, D>
    for ColorState
where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
        + Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ImageDescriptionData>
        + ColorHandler
        + 'static,
{
    fn event(
        _state: &mut D,
        proxy: &wp_image_description_v1::WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        data: &ImageDescriptionData,
        _conn: &Connection,
        qh: &QueueHandle<D>,
    ) {
        let mut guard = data.0.lock().unwrap();

        match event {
            wp_image_description_v1::Event::Failed { cause: _, msg: _ } => {
                todo!()
            },
            wp_image_description_v1::Event::Ready { identity: _ } => {
                let ImageDescriptionInner::NotReady(ref source) = *guard else {
                    return;
                };
                let builder = Arc::new(Mutex::new(ImageDescriptionBuilder::default()));
                *guard = ImageDescriptionInner::Building {
                    builder,
                    source: source.clone(),
                };
                _ = proxy.get_information(qh, data.clone());
            },
            wp_image_description_v1::Event::Ready2 {
                identity_hi: _,
                identity_lo: _,
            } => {
                let ImageDescriptionInner::NotReady(ref source) = *guard else {
                    return;
                };
                let builder = Arc::new(Mutex::new(ImageDescriptionBuilder::default()));
                *guard = ImageDescriptionInner::Building {
                    builder,
                    source: source.clone(),
                };
                _ = proxy.get_information(qh, data.clone());
            },
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ImageDescriptionData, D>
    for ColorState
where
    D: Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
        + Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ImageDescriptionData>
        + ColorHandler
        + 'static,
{
    fn event(
        state: &mut D,
        _proxy: &wp_image_description_info_v1::WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        data: &ImageDescriptionData,
        conn: &Connection,
        qh: &QueueHandle<D>,
    ) {
        use wp_image_description_info_v1::Event;

        let mut guard = data.0.lock().unwrap();
        match event {
            Event::IccFile {
                icc: _,
                icc_size: _,
            } => {
                todo!()
            },
            Event::Done => {
                let ImageDescriptionInner::Building { builder, source } = &*guard else {
                    return;
                };
                let description = builder.lock().unwrap().build();
                let source = source.clone();
                *guard = ImageDescriptionInner::Defined(description.clone());

                match source {
                    ImageDescriptionSource::Output(output) => {
                        state.output_update(conn, qh, &output, description);
                    },
                    ImageDescriptionSource::Surface(_) => {
                        todo!()
                    },
                }
            },
            Event::PrimariesNamed { primaries } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                builder.lock().unwrap().primaries = Some(Primaries::Named(primaries));
            },
            Event::TfNamed { tf } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                builder.lock().unwrap().eotf = Some(Eotf::Named(tf));
            },
            Event::Luminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                // Scale the min_luminance value from the fixed-point representation used in the
                // protocol to actual nits.
                let min_lum = f64::from(min_lum) / 10000.0;

                let max_lum = f64::from(max_lum);
                let reference_lum = f64::from(reference_lum);

                builder.lock().unwrap().luma_range = Some(min_lum..=max_lum);
                builder.lock().unwrap().ref_luma = Some(reference_lum);
            },
            Event::TfPower { eexp } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                let power = f64::from(eexp) / 10000.0;
                builder.lock().unwrap().eotf = Some(Eotf::Power(power));
            },
            Event::Primaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                builder.lock().unwrap().primaries = Some(Primaries::Custom {
                    red:   (f64::from(r_x) / 1_000_000.0, f64::from(r_y) / 1_000_000.0),
                    green: (f64::from(g_x) / 1_000_000.0, f64::from(g_y) / 1_000_000.0),
                    blue:  (f64::from(b_x) / 1_000_000.0, f64::from(b_y) / 1_000_000.0),
                    white: (f64::from(w_x) / 1_000_000.0, f64::from(w_y) / 1_000_000.0),
                });
            },
            Event::TargetLuminance { min_lum, max_lum } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                // Scale the min_luminance value from the fixed-point representation used in the
                // protocol to actual nits.
                let min_lum = f64::from(min_lum) / 10000.0;

                let max_lum = f64::from(max_lum);
                builder.lock().unwrap().mastering_luma_range = Some(min_lum..=max_lum);
            },
            Event::TargetMaxCll { max_cll } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                builder.lock().unwrap().target_max_cll = Some(f64::from(max_cll));
            },
            Event::TargetMaxFall { max_fall } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                builder.lock().unwrap().target_max_fall = Some(f64::from(max_fall));
            },
            Event::TargetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                let ImageDescriptionInner::Building { builder, .. } = &*guard else {
                    return;
                };
                builder.lock().unwrap().mastering_primaries = Some(Primaries::Custom {
                    red:   (f64::from(r_x) / 1_000_000.0, f64::from(r_y) / 1_000_000.0),
                    green: (f64::from(g_x) / 1_000_000.0, f64::from(g_y) / 1_000_000.0),
                    blue:  (f64::from(b_x) / 1_000_000.0, f64::from(b_y) / 1_000_000.0),
                    white: (f64::from(w_x) / 1_000_000.0, f64::from(w_y) / 1_000_000.0),
                });
            },
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<wp_color_manager_v1::WpColorManagerV1, GlobalData, D> for ColorState
where
    D: Dispatch<wp_color_manager_v1::WpColorManagerV1, GlobalData> + ColorHandler + 'static,
{
    fn event(
        state: &mut D,
        proxy: &wp_color_manager_v1::WpColorManagerV1,
        event: wp_color_manager_v1::Event,
        data: &GlobalData,
        conn: &Connection,
        qhandle: &QueueHandle<D>,
    ) {
        use wp_color_manager_v1::Event;

        match event {
            Event::Done => {
                state.compositor_features(conn, qhandle);
            },
            Event::SupportedFeature { feature } => {
                state.color_handler().supported_features.push(feature);
            },
            Event::SupportedPrimariesNamed { primaries } => {
                state.color_handler().supported_primaries.push(primaries);
            },
            Event::SupportedIntent { render_intent } => {
                state.color_handler().supported_idents.push(render_intent);
            },
            Event::SupportedTfNamed { tf } => {
                state.color_handler().supported_tfs.push(tf);
            },
            _ => unreachable!(),
        }
    }
}
