use std::{
    collections::HashMap,
    ops::RangeInclusive,
    sync::{
        Arc,
        Mutex,
    },
};

use smithay_client_toolkit::globals::GlobalData;
use wayland_client::{
    Connection,
    Dispatch,
    Proxy,
    QueueHandle,
    QueueProxyData,
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
    wp_color_management_surface_v1,
    wp_color_manager_v1,
    wp_image_description_creator_params_v1,
    wp_image_description_info_v1,
    wp_image_description_v1,
};

#[derive(Debug)]
pub struct ColorState {
    color_manager: wp_color_manager_v1::WpColorManagerV1,
    outputs:       HashMap<WlOutput, ColorOutput>,
    surfaces:      HashMap<WlSurface, ColorSurface>,

    supported_idents:    Vec<WEnum<RenderIntent>>,
    supported_features:  Vec<WEnum<Feature>>,
    supported_tfs:       Vec<WEnum<NamedEotf>>,
    supported_primaries: Vec<WEnum<NamedPrimaries>>,
}

#[derive(Debug)]
struct ColorOutput {
    manager: wp_color_management_output_v1::WpColorManagementOutputV1,

    info:         wp_image_description_v1::WpImageDescriptionV1,
    pending_info: Option<wp_image_description_v1::WpImageDescriptionV1>,
}

#[derive(Debug, Clone)]
struct ColorSurface {
    feedback: Option<wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1>,
    manager:  wp_color_management_surface_v1::WpColorManagementSurfaceV1,

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
    NotReady {
        source: ImageDescriptionSource,
        info:   Option<(ImageDescriptionInfo, RenderIntent)>,
    },
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
        proxy: &wp_image_description_v1::WpImageDescriptionV1,
        output: &WlOutput,
        description: ImageDescriptionInfo,
    );

    /// Triggered when the preferred description of a monitored surface changes.
    fn surface_update(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        proxy: &wp_image_description_v1::WpImageDescriptionV1,
        surface: &WlSurface,
        description: ImageDescriptionInfo,
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
            surfaces: HashMap::new(),

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
            ImageDescriptionData(Arc::new(Mutex::new(ImageDescriptionInner::NotReady {
                source: ImageDescriptionSource::Output(output.clone()),
                info:   None,
            }))),
        );

        self.outputs.insert(output, ColorOutput {
            manager,
            info,
            pending_info: None,
        });
    }

    pub fn set_surface_description_proxy<D>(
        &mut self,
        qh: &QueueHandle<D>,
        surface: &WlSurface,
        description: &wp_image_description_v1::WpImageDescriptionV1,
        render_intent: RenderIntent,
    ) where
        D: Dispatch<
                wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
                GlobalData,
            > + Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
            + Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, GlobalData>
            + 'static,
    {
        // Ensure surface is managed
        let entry = self
            .surfaces
            .entry(surface.clone())
            .or_insert_with(|| ColorSurface {
                feedback:     None,
                manager:      self.color_manager.get_surface(surface, qh, GlobalData),
                info:         description.clone(),
                pending_info: None,
            });
        entry.info = description.clone();
        entry
            .manager
            .set_image_description(description, render_intent);
        surface.commit();
    }

    pub fn set_surface_description<D>(
        &mut self,
        qh: &QueueHandle<D>,
        surface: &WlSurface,
        description: ImageDescriptionInfo,
        render_intent: RenderIntent,
    ) where
        D: Dispatch<
                wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
                GlobalData,
            > + Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
            + Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, GlobalData>
            + 'static,
    {
        // Create image description
        let image = match &description {
            ImageDescriptionInfo::Parametric {
                primaries,
                eotf,
                luma_range,
                ref_luma,
                mastering_primaries,
                mastering_luma_range,
                target_max_cll,
                target_max_fall,
            } => {
                let creator = self.color_manager.create_parametric_creator(qh, GlobalData);

                match primaries {
                    Primaries::Named(named) => {
                        creator.set_primaries_named(
                            named.into_result().expect(
                                "Unknown named primary supplied to set_surface_description",
                            ),
                        );
                    },
                    Primaries::Custom {
                        red,
                        green,
                        blue,
                        white,
                    } => {
                        creator.set_primaries(
                            (red.0 * 1_000_000.0) as i32,
                            (red.1 * 1_000_000.0) as i32,
                            (green.0 * 1_000_000.0) as i32,
                            (green.1 * 1_000_000.0) as i32,
                            (blue.0 * 1_000_000.0) as i32,
                            (blue.1 * 1_000_000.0) as i32,
                            (white.0 * 1_000_000.0) as i32,
                            (white.1 * 1_000_000.0) as i32,
                        );
                    },
                }

                match eotf {
                    Eotf::Named(named) => {
                        creator.set_tf_named(named.into_result().expect(
                            "Unknown named transfer function supplied to set_surface_description",
                        ));
                    },
                    Eotf::Power(power) => {
                        creator.set_tf_power((power * 10_000.0) as u32);
                    },
                }

                creator.set_luminances(
                    (*luma_range.start() * 10_000.0) as u32,
                    *luma_range.end() as u32,
                    *ref_luma as u32,
                );

                if let Some(mastering_primaries) = mastering_primaries {
                    match mastering_primaries {
                        Primaries::Named(_) => {
                            // TODO: Convert named primaries to custom primaries
                            todo!(
                                "Named mastering primaries are not supported in set_surface_description"
                            );
                        },
                        Primaries::Custom {
                            red,
                            green,
                            blue,
                            white,
                        } => {
                            creator.set_mastering_display_primaries(
                                (red.0 * 1_000_000.0) as i32,
                                (red.1 * 1_000_000.0) as i32,
                                (green.0 * 1_000_000.0) as i32,
                                (green.1 * 1_000_000.0) as i32,
                                (blue.0 * 1_000_000.0) as i32,
                                (blue.1 * 1_000_000.0) as i32,
                                (white.0 * 1_000_000.0) as i32,
                                (white.1 * 1_000_000.0) as i32,
                            );
                        },
                    }
                }
                if let Some(mastering_luma_range) = mastering_luma_range {
                    creator.set_mastering_luminance(
                        (*mastering_luma_range.start() * 10_000.0) as u32,
                        *mastering_luma_range.end() as u32,
                    );
                }
                if let Some(target_max_cll) = target_max_cll {
                    creator.set_max_cll((target_max_cll * 10_000.0) as u32);
                }
                if let Some(target_max_fall) = target_max_fall {
                    creator.set_max_fall((target_max_fall * 10_000.0) as u32);
                }

                creator.create(
                    qh,
                    ImageDescriptionData(Arc::new(Mutex::new(ImageDescriptionInner::NotReady {
                        source: ImageDescriptionSource::Surface(surface.clone()),
                        info:   Some((description, render_intent)),
                    }))),
                )
            },
        };

        // Ensure surface is managed
        let entry = self
            .surfaces
            .entry(surface.clone())
            .or_insert_with(|| ColorSurface {
                feedback:     None,
                manager:      self.color_manager.get_surface(surface, qh, GlobalData),
                info:         image.clone(),
                pending_info: None,
            });
        entry.info = image;
    }

    pub fn release_output(
        &mut self,
        output: &WlOutput,
    ) {
        self.outputs.remove(output);
    }

    pub fn release_surface(
        &mut self,
        surface: &WlSurface,
    ) {
        self.surfaces.remove(surface);
    }

    pub fn track_surface<D>(
        &mut self,
        qh: &QueueHandle<D>,
        surface: &WlSurface,
    ) where
        D: Dispatch<
                wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
                SurfaceFeedbackData,
            > + Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
            + Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, GlobalData>
            + 'static,
    {
        self.surfaces
            .entry(surface.clone())
            .and_modify(|info| {
                let feedback =
                    self.color_manager
                        .get_surface_feedback(surface, qh, SurfaceFeedbackData {
                            surface: surface.clone(),
                        });
                info.pending_info = Some(feedback.get_preferred_parametric(
                    qh,
                    ImageDescriptionData(Arc::new(Mutex::new(ImageDescriptionInner::NotReady {
                        source: ImageDescriptionSource::Surface(surface.clone()),
                        info:   None,
                    }))),
                ));
                info.feedback = Some(feedback);
            })
            .or_insert_with(|| {
                let manager = self.color_manager.get_surface(surface, qh, GlobalData);

                let feedback =
                    self.color_manager
                        .get_surface_feedback(surface, qh, SurfaceFeedbackData {
                            surface: surface.clone(),
                        });
                let info = feedback.get_preferred_parametric(
                    qh,
                    ImageDescriptionData(Arc::new(Mutex::new(ImageDescriptionInner::NotReady {
                        source: ImageDescriptionSource::Surface(surface.clone()),
                        info:   None,
                    }))),
                );

                ColorSurface {
                    feedback: Some(feedback),
                    manager,
                    info: info.clone(),
                    pending_info: Some(info),
                }
            });
    }

    pub fn get_surface_description(
        &self,
        surface: &WlSurface,
    ) -> Option<ImageDescriptionInfo> {
        self.surfaces.get(surface).map(|info| {
            let data: &ImageDescriptionData = info.info.data().unwrap();
            let guard = data.0.lock().unwrap();
            match &*guard {
                ImageDescriptionInner::Defined(info) => info.clone(),
                _ => panic!("Surface description is not ready"),
            }
        })
    }
}

impl<D>
    Dispatch<
        wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
        GlobalData,
        D,
    > for ColorState
where
    D: Dispatch<
            wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
            GlobalData,
        > + ColorHandler
        + 'static,
{
    fn event(
        _state: &mut D,
        _proxy: &wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
        _event: wp_image_description_creator_params_v1::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
        // No events for type
    }
}

impl<D> Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, GlobalData, D>
    for ColorState
where
    D: Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, GlobalData>
        + ColorHandler
        + 'static,
{
    fn event(
        _state: &mut D,
        _proxy: &wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        _event: wp_color_management_surface_v1::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
        // No events for type
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
            wp_color_management_output_v1::Event::ImageDescriptionChanged => {
                let output = state
                    .color_handler()
                    .outputs
                    .get_mut(&data.output)
                    .expect("Got event on untracked output");

                let new_info = output.manager.get_image_description(
                    qh,
                    ImageDescriptionData(Arc::new(Mutex::new(ImageDescriptionInner::NotReady {
                        source: ImageDescriptionSource::Output(data.output.clone()),
                        info:   None,
                    }))),
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
        > + Dispatch<wp_image_description_v1::WpImageDescriptionV1, ImageDescriptionData>
        + ColorHandler
        + 'static,
{
    fn event(
        state: &mut D,
        _proxy: &wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        event: wp_color_management_surface_feedback_v1::Event,
        data: &SurfaceFeedbackData,
        _conn: &Connection,
        qh: &QueueHandle<D>,
    ) {
        match event {
            wp_color_management_surface_feedback_v1::Event::PreferredChanged { identity: _ }
            | wp_color_management_surface_feedback_v1::Event::PreferredChanged2 {
                identity_hi: _,
                identity_lo: _,
            } => {
                let surface = state
                    .color_handler()
                    .surfaces
                    .get_mut(&data.surface)
                    .expect("Got event on untracked surface");

                let new_info = surface.feedback.as_ref().unwrap().get_preferred_parametric(
                    qh,
                    ImageDescriptionData(Arc::new(Mutex::new(ImageDescriptionInner::NotReady {
                        source: ImageDescriptionSource::Surface(data.surface.clone()),
                        info:   None,
                    }))),
                );
                surface.pending_info = Some(new_info);
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
        state: &mut D,
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
            wp_image_description_v1::Event::Ready { identity: _ }
            | wp_image_description_v1::Event::Ready2 {
                identity_hi: _,
                identity_lo: _,
            } => {
                let ImageDescriptionInner::NotReady {
                    ref source,
                    ref info,
                } = *guard
                else {
                    return;
                };
                let source = source.clone();

                if let Some((info, intent)) = info.clone() {
                    // If we already have information about the image, its probably from a
                    // set_surface_description call.
                    *guard = ImageDescriptionInner::Defined(info);
                    let ImageDescriptionSource::Surface(surface) = source else {
                        return;
                    };
                    state
                        .color_handler()
                        .surfaces
                        .get(&surface)
                        .expect("Surface doesn't exist")
                        .manager
                        .set_image_description(proxy, intent);
                    surface.commit();
                } else {
                    // If we don't have any information about the image yet, its probably a
                    // requested image.
                    let builder = Arc::new(Mutex::new(ImageDescriptionBuilder::default()));
                    *guard = ImageDescriptionInner::Building {
                        builder,
                        source: source.clone(),
                    };
                    _ = proxy.get_information(qh, data.clone());
                    tracing::debug!("Requested image description information for {:?}", source);
                }
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
                tracing::info!("Received ICC profile, but ICC profiles are not yet supported");
                //todo!()
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
                        let proxy =
                            if let Some(info) = state.color_handler().outputs.get_mut(&output) {
                                info.info = info
                                    .pending_info
                                    .take()
                                    .expect("Output info should be pending");
                                info.info.clone()
                            } else {
                                return;
                            };

                        state.output_update(conn, qh, &proxy, &output, description);
                    },
                    ImageDescriptionSource::Surface(surface) => {
                        let proxy =
                            if let Some(info) = state.color_handler().surfaces.get_mut(&surface) {
                                info.info = info
                                    .pending_info
                                    .take()
                                    .expect("Surface info should be pending");
                                info.info.clone()
                            } else {
                                return;
                            };
                        state.surface_update(conn, qh, &proxy, &surface, description);
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
