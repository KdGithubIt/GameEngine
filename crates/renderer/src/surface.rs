use crate::context::GpuContext;
use std::fmt;
use std::sync::Arc;
use wgpu::{
    CompositeAlphaMode, Instance, PresentMode, Surface, SurfaceConfiguration, TextureFormat,
    TextureUsages,
};
use winit::window::Window;

/// Configures presentation behavior for a window surface.
#[derive(Debug, Clone)]
pub struct WindowSurfaceDescriptor {
    /// The intended use of acquired surface textures.
    pub usage: TextureUsages,
    /// The requested presentation mode.
    pub present_mode: PresentMode,
    /// The preferred surface texture format, or `None` to prefer an sRGB format.
    pub preferred_format: Option<TextureFormat>,
    /// The preferred alpha composition mode, or `None` to use the first supported mode.
    pub preferred_alpha_mode: Option<CompositeAlphaMode>,
    /// The maximum number of frames queued for presentation.
    pub desired_maximum_frame_latency: u32,
}

impl Default for WindowSurfaceDescriptor {
    fn default() -> Self {
        Self {
            usage: TextureUsages::RENDER_ATTACHMENT,
            present_mode: PresentMode::AutoVsync,
            preferred_format: None,
            preferred_alpha_mode: None,
            desired_maximum_frame_latency: 2,
        }
    }
}

/// Reports a failure while creating a window rendering surface.
#[derive(Debug)]
pub enum WindowSurfaceError {
    /// The GPU backend could not create a surface for the window.
    CreateSurface(wgpu::CreateSurfaceError),
    /// The surface did not report any supported texture formats.
    NoSupportedFormats,
    /// The surface did not report any supported alpha modes.
    NoSupportedAlphaModes,
    /// The requested texture usage is not supported by the surface.
    UnsupportedUsage {
        /// The requested usage flags.
        usage: TextureUsages,
    },
    /// The requested presentation mode is not supported by the surface.
    UnsupportedPresentMode {
        /// The requested presentation mode.
        present_mode: PresentMode,
    },
    /// The requested texture format is not supported by the surface.
    UnsupportedFormat {
        /// The requested texture format.
        format: TextureFormat,
    },
    /// The requested alpha composition mode is not supported by the surface.
    UnsupportedAlphaMode {
        /// The requested alpha composition mode.
        alpha_mode: CompositeAlphaMode,
    },
    /// The requested surface dimensions exceed the device limit.
    InvalidDimensions {
        /// The requested width.
        width: u32,
        /// The requested height.
        height: u32,
        /// The maximum supported two-dimensional texture size.
        maximum: u32,
    },
}

impl fmt::Display for WindowSurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateSurface(error) => {
                write!(formatter, "failed to create window surface: {error}")
            }
            Self::NoSupportedFormats => {
                formatter.write_str("window surface has no supported formats")
            }
            Self::NoSupportedAlphaModes => {
                formatter.write_str("window surface has no supported alpha modes")
            }
            Self::UnsupportedUsage { usage } => {
                write!(formatter, "window surface does not support usage {usage:?}")
            }
            Self::UnsupportedPresentMode { present_mode } => write!(
                formatter,
                "window surface does not support present mode {present_mode:?}"
            ),
            Self::UnsupportedFormat { format } => {
                write!(formatter, "window surface does not support format {format:?}")
            }
            Self::UnsupportedAlphaMode { alpha_mode } => write!(
                formatter,
                "window surface does not support alpha mode {alpha_mode:?}"
            ),
            Self::InvalidDimensions {
                width,
                height,
                maximum,
            } => write!(
                formatter,
                "window surface dimensions {width}x{height} exceed the device limit of {maximum}x{maximum}"
            ),
        }
    }
}

impl std::error::Error for WindowSurfaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateSurface(error) => Some(error),
            Self::NoSupportedFormats
            | Self::NoSupportedAlphaModes
            | Self::UnsupportedUsage { .. }
            | Self::UnsupportedPresentMode { .. }
            | Self::UnsupportedFormat { .. }
            | Self::UnsupportedAlphaMode { .. }
            | Self::InvalidDimensions { .. } => None,
        }
    }
}

/// Owns a window surface before an adapter-specific configuration is chosen.
///
/// This state exists because adapter selection may depend on the surface, while
/// the surface capabilities needed for configuration depend on the adapter.
pub struct UnconfiguredWindowSurface {
    surface: Surface<'static>,
    width: u32,
    height: u32,
}

impl UnconfiguredWindowSurface {
    /// Creates a surface for `window` without choosing a presentation format.
    ///
    /// # Errors
    ///
    /// Returns an error when the GPU backend cannot create a surface for the
    /// window.
    pub fn new(instance: &Instance, window: Arc<Window>) -> Result<Self, WindowSurfaceError> {
        let size = window.inner_size();
        let surface = instance
            .create_surface(window)
            .map_err(WindowSurfaceError::CreateSurface)?;

        Ok(Self {
            surface,
            width: size.width,
            height: size.height,
        })
    }

    /// Returns the raw surface for compatible adapter selection.
    pub fn surface(&self) -> &Surface<'static> {
        &self.surface
    }

    /// Chooses an adapter-supported presentation configuration and applies it
    /// to `context`.
    ///
    /// # Errors
    ///
    /// Returns an error when the surface does not expose the capabilities
    /// required for presentation.
    pub fn configure(self, context: &GpuContext) -> Result<WindowSurface, WindowSurfaceError> {
        let descriptor = WindowSurfaceDescriptor::default();
        self.configure_with_descriptor(context, &descriptor)
    }

    /// Chooses and applies a presentation configuration using `descriptor`.
    ///
    /// # Errors
    ///
    /// Returns an error when the surface does not support the requested or
    /// required presentation capabilities.
    pub fn configure_with_descriptor(
        self,
        context: &GpuContext,
        descriptor: &WindowSurfaceDescriptor,
    ) -> Result<WindowSurface, WindowSurfaceError> {
        let capabilities = self.surface.get_capabilities(context.adapter());
        if !capabilities.usages.contains(descriptor.usage) {
            return Err(WindowSurfaceError::UnsupportedUsage {
                usage: descriptor.usage,
            });
        }
        let present_mode_is_automatic = matches!(
            descriptor.present_mode,
            PresentMode::AutoVsync | PresentMode::AutoNoVsync
        );
        if !present_mode_is_automatic
            && !capabilities
                .present_modes
                .contains(&descriptor.present_mode)
        {
            return Err(WindowSurfaceError::UnsupportedPresentMode {
                present_mode: descriptor.present_mode,
            });
        }

        let format = if let Some(format) = descriptor.preferred_format {
            capabilities
                .formats
                .contains(&format)
                .then_some(format)
                .ok_or(WindowSurfaceError::UnsupportedFormat { format })?
        } else {
            capabilities
                .formats
                .iter()
                .find(|format| format.is_srgb())
                .or_else(|| capabilities.formats.first())
                .copied()
                .ok_or(WindowSurfaceError::NoSupportedFormats)?
        };
        let alpha_mode = if let Some(alpha_mode) = descriptor.preferred_alpha_mode {
            capabilities
                .alpha_modes
                .contains(&alpha_mode)
                .then_some(alpha_mode)
                .ok_or(WindowSurfaceError::UnsupportedAlphaMode { alpha_mode })?
        } else {
            capabilities
                .alpha_modes
                .first()
                .copied()
                .ok_or(WindowSurfaceError::NoSupportedAlphaModes)?
        };

        let width = self.width.max(1);
        let height = self.height.max(1);
        validate_surface_dimensions(width, height, context.device())?;

        let config = SurfaceConfiguration {
            usage: descriptor.usage,
            format,
            width,
            height,
            present_mode: descriptor.present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: descriptor.desired_maximum_frame_latency,
        };

        self.surface.configure(context.device(), &config);

        Ok(WindowSurface {
            surface: self.surface,
            config,
            format,
        })
    }
}

/// Owns a window surface and its current presentation configuration.
pub struct WindowSurface {
    surface: Surface<'static>,
    config: SurfaceConfiguration,
    format: TextureFormat,
}

impl WindowSurface {
    /// Returns the raw configured surface.
    pub fn surface(&self) -> &Surface<'static> {
        &self.surface
    }

    /// Returns the current presentation configuration.
    pub fn config(&self) -> &SurfaceConfiguration {
        &self.config
    }

    /// Returns the selected surface texture format.
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// Configures the surface using its current settings and original GPU context.
    pub fn configure(&self, context: &GpuContext) {
        self.surface.configure(context.device(), &self.config);
    }

    /// Updates the presentation size and reconfigures the surface.
    ///
    /// Zero-sized dimensions are ignored because they cannot be configured.
    /// `context` must be the context used to create this surface configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested dimensions exceed the device limit.
    pub fn resize(
        &mut self,
        context: &GpuContext,
        width: u32,
        height: u32,
    ) -> Result<(), WindowSurfaceError> {
        if width > 0 && height > 0 {
            validate_surface_dimensions(width, height, context.device())?;
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(context.device(), &self.config);
        }
        Ok(())
    }
}

fn validate_surface_dimensions(
    width: u32,
    height: u32,
    device: &wgpu::Device,
) -> Result<(), WindowSurfaceError> {
    let maximum = device.limits().max_texture_dimension_2d;
    if width > maximum || height > maximum {
        return Err(WindowSurfaceError::InvalidDimensions {
            width,
            height,
            maximum,
        });
    }
    Ok(())
}
